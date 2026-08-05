use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{
    CloudProvider, ProviderContext, ProviderError, ProviderErrorKind, ProviderFactory, ProviderId,
    ProviderIdentity, ProviderReadiness, ProviderRegistry, ProviderResult, ProviderSetting,
    ProviderSettingsSnapshot, RemoteItem, RemoteRef, RemoteSource,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderStatus {
    Disabled,
    NeedsConfig,
    Connecting,
    Ready,
    ConfigError(String),
    ConnectionError(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCatalogEntry {
    pub id: ProviderId,
    pub display_name: String,
    pub identity: ProviderIdentity,
    pub status: ProviderStatus,
    pub available: bool,
    pub settings_schema: Vec<ProviderSetting>,
    pub settings: ProviderSettingsSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSettingsSaveResult {
    pub provider: ProviderCatalogEntry,
    pub retained_previous_instance: bool,
}

pub struct ProviderSlot {
    factory: Arc<dyn ProviderFactory>,
    status: ProviderStatus,
    instance: Option<Arc<dyn CloudProvider>>,
}

impl ProviderSlot {
    pub fn display_name(&self) -> &'static str {
        self.factory.display_name()
    }

    pub fn status(&self) -> &ProviderStatus {
        &self.status
    }

    pub fn instance(&self) -> Option<&Arc<dyn CloudProvider>> {
        self.instance.as_ref()
    }

    pub fn factory(&self) -> &Arc<dyn ProviderFactory> {
        &self.factory
    }
}

#[derive(Default)]
pub struct ProviderManager {
    slots: BTreeMap<ProviderId, ProviderSlot>,
}

impl ProviderManager {
    pub fn from_registry(registry: &ProviderRegistry) -> Self {
        let slots = registry
            .iter()
            .map(|(id, factory)| {
                (
                    id.clone(),
                    ProviderSlot {
                        factory: Arc::clone(factory),
                        status: ProviderStatus::Disabled,
                        instance: None,
                    },
                )
            })
            .collect();
        Self { slots }
    }

    pub async fn initialize(&mut self, context: &ProviderContext) {
        let ids: Vec<_> = self.slots.keys().cloned().collect();
        for id in ids {
            self.initialize_one(&id, context.clone()).await;
        }
    }

    pub async fn initialize_one(&mut self, id: &ProviderId, context: ProviderContext) {
        let Some(slot) = self.slots.get_mut(id) else {
            return;
        };
        match slot.factory.readiness(&context) {
            ProviderReadiness::Disabled => {
                slot.status = ProviderStatus::Disabled;
                slot.instance = None;
            }
            ProviderReadiness::NeedsConfig => {
                slot.status = ProviderStatus::NeedsConfig;
                slot.instance = None;
            }
            ProviderReadiness::ReadyToConnect => {
                slot.status = ProviderStatus::Connecting;
                match slot.factory.create(context).await {
                    Ok(instance) => {
                        slot.status = ProviderStatus::Ready;
                        slot.instance = Some(instance);
                    }
                    Err(error) => {
                        slot.status = match error.kind {
                            ProviderErrorKind::Config => ProviderStatus::ConfigError(error.message),
                            _ => ProviderStatus::ConnectionError(error.message),
                        };
                        slot.instance = None;
                    }
                }
            }
        }
    }

    pub fn get(&self, id: &ProviderId) -> Option<&ProviderSlot> {
        self.slots.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ProviderId, &ProviderSlot)> {
        self.slots.iter()
    }

    fn instance_for(&self, identity: &ProviderIdentity) -> Option<Arc<dyn CloudProvider>> {
        self.slots
            .get(&identity.provider_id)
            .and_then(ProviderSlot::instance)
            .filter(|provider| provider.identity() == identity)
            .cloned()
    }

    async fn save_and_rebuild(
        &mut self,
        id: &ProviderId,
        context: &ProviderContext,
        changes: &BTreeMap<String, String>,
    ) -> ProviderResult<bool> {
        let slot = self.slots.get_mut(id).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::NotFound,
                format!("provider {id} is not registered"),
            )
        })?;
        slot.factory.save_settings(context, changes)?;
        let previous = slot.instance.clone();
        let retained_previous_instance = previous.is_some();
        match slot.factory.readiness(context) {
            ProviderReadiness::Disabled => {
                slot.status = ProviderStatus::Disabled;
                slot.instance = None;
                Ok(false)
            }
            ProviderReadiness::NeedsConfig => {
                slot.status = ProviderStatus::NeedsConfig;
                slot.instance = previous;
                Ok(retained_previous_instance)
            }
            ProviderReadiness::ReadyToConnect => {
                slot.status = ProviderStatus::Connecting;
                match slot.factory.create(context.clone()).await {
                    Ok(instance) => {
                        slot.status = ProviderStatus::Ready;
                        slot.instance = Some(instance);
                        Ok(false)
                    }
                    Err(error) => {
                        slot.status = provider_error_status(&error);
                        slot.instance = previous;
                        Ok(retained_previous_instance)
                    }
                }
            }
        }
    }
}

pub struct ProviderRuntime {
    context: ProviderContext,
    manager: ProviderManager,
}

impl ProviderRuntime {
    pub async fn from_registry(registry: &ProviderRegistry, context: ProviderContext) -> Self {
        let mut manager = ProviderManager::from_registry(registry);
        manager.initialize(&context).await;
        Self { context, manager }
    }

    #[cfg(test)]
    pub fn for_test(providers: Vec<Arc<dyn CloudProvider>>) -> Self {
        struct InstanceFactory {
            id: ProviderId,
            name: &'static str,
            instance: Arc<dyn CloudProvider>,
        }

        #[async_trait::async_trait]
        impl ProviderFactory for InstanceFactory {
            fn id(&self) -> ProviderId {
                self.id.clone()
            }

            fn display_name(&self) -> &'static str {
                self.name
            }

            fn readiness(&self, _context: &ProviderContext) -> ProviderReadiness {
                ProviderReadiness::ReadyToConnect
            }

            async fn create(
                &self,
                _context: ProviderContext,
            ) -> ProviderResult<Arc<dyn CloudProvider>> {
                Ok(self.instance.clone())
            }
        }

        let slots = providers
            .into_iter()
            .map(|instance| {
                let id = instance.identity().provider_id.clone();
                let name = Box::leak(id.to_string().into_boxed_str());
                let factory: Arc<dyn ProviderFactory> = Arc::new(InstanceFactory {
                    id: id.clone(),
                    name,
                    instance: instance.clone(),
                });
                (
                    id,
                    ProviderSlot {
                        factory,
                        status: ProviderStatus::Ready,
                        instance: Some(instance),
                    },
                )
            })
            .collect();
        Self {
            context: ProviderContext {
                config_root: std::path::PathBuf::from("/tmp/pan-tui-test"),
                network: crate::settings::NetworkSettings::default(),
                config_overrides: BTreeMap::new(),
            },
            manager: ProviderManager { slots },
        }
    }

    pub fn catalog(&self) -> Vec<ProviderCatalogEntry> {
        self.manager
            .iter()
            .map(|(id, slot)| self.catalog_entry(id, slot))
            .collect()
    }

    pub fn provider(&self, identity: &ProviderIdentity) -> Option<Arc<dyn CloudProvider>> {
        self.manager.instance_for(identity)
    }

    pub async fn sources(&self, identity: &ProviderIdentity) -> ProviderResult<Vec<RemoteSource>> {
        self.ready_provider(identity)?.sources().await
    }

    pub async fn list_children(&self, parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
        self.ready_provider(&parent.identity)?
            .list_children(parent)
            .await
    }

    pub async fn refresh_item(&self, item: &RemoteRef) -> ProviderResult<RemoteItem> {
        self.ready_provider(&item.identity)?
            .refresh_item(item)
            .await
    }

    pub async fn save_provider_settings(
        &mut self,
        id: &ProviderId,
        changes: &BTreeMap<String, String>,
    ) -> ProviderResult<ProviderSettingsSaveResult> {
        let retained_previous_instance = self
            .manager
            .save_and_rebuild(id, &self.context, changes)
            .await?;
        let slot = self
            .manager
            .get(id)
            .expect("saved provider remains registered");
        Ok(ProviderSettingsSaveResult {
            provider: self.catalog_entry(id, slot),
            retained_previous_instance,
        })
    }

    pub async fn reconfigure_network(
        &mut self,
        network: crate::settings::NetworkSettings,
    ) -> Vec<ProviderCatalogEntry> {
        self.context.network = network;
        let ids: Vec<_> = self.manager.slots.keys().cloned().collect();
        for id in ids {
            let previous = self
                .manager
                .slots
                .get(&id)
                .and_then(|slot| slot.instance.clone());
            self.manager.initialize_one(&id, self.context.clone()).await;
            if self.manager.slots.get(&id).is_some_and(|slot| {
                slot.instance.is_none()
                    && matches!(
                        slot.status,
                        ProviderStatus::ConfigError(_) | ProviderStatus::ConnectionError(_)
                    )
            }) && previous.is_some()
                && let Some(slot) = self.manager.slots.get_mut(&id)
            {
                slot.instance = previous;
            }
        }
        self.catalog()
    }

    fn ready_provider(
        &self,
        identity: &ProviderIdentity,
    ) -> ProviderResult<Arc<dyn CloudProvider>> {
        self.provider(identity).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Config,
                format!(
                    "provider {}/{} is unavailable",
                    identity.provider_id, identity.profile_id
                ),
            )
            .retryable(true)
        })
    }

    fn catalog_entry(&self, id: &ProviderId, slot: &ProviderSlot) -> ProviderCatalogEntry {
        let identity = slot.instance.as_ref().map_or_else(
            || {
                ProviderIdentity::new(
                    id.clone(),
                    super::ProfileId::new("default").expect("static profile ID is valid"),
                )
            },
            |provider| provider.identity().clone(),
        );
        let settings = slot
            .factory
            .load_settings(&self.context)
            .unwrap_or_else(|error| ProviderSettingsSnapshot {
                read_only_reason: Some(format!("Could not load settings: {error}")),
                ..ProviderSettingsSnapshot::default()
            });
        ProviderCatalogEntry {
            id: id.clone(),
            display_name: slot.display_name().to_owned(),
            identity,
            status: slot.status.clone(),
            available: slot.instance.is_some(),
            settings_schema: slot
                .factory
                .settings_schema()
                .into_iter()
                .map(ProviderSetting::from)
                .collect(),
            settings,
        }
    }
}

fn provider_error_status(error: &ProviderError) -> ProviderStatus {
    match error.kind {
        ProviderErrorKind::Config => ProviderStatus::ConfigError(error.message.clone()),
        _ => ProviderStatus::ConnectionError(error.message.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::{
        provider::{
            DownloadCapability, ProfileId, ProviderCapabilities, ProviderError, ProviderIdentity,
            ProviderResult, ProviderSettingKind, ProviderSettingSpec, RemoteDownload, RemoteItem,
            RemoteItemKind, RemoteObjectId, RemoteRef, RemoteSource, SourceId,
        },
        settings::NetworkSettings,
    };

    struct FakeProvider {
        identity: ProviderIdentity,
    }

    #[async_trait]
    impl CloudProvider for FakeProvider {
        fn identity(&self) -> &ProviderIdentity {
            &self.identity
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                ranged_downloads: true,
                checksums: false,
            }
        }

        async fn sources(&self) -> ProviderResult<Vec<RemoteSource>> {
            let root = self.item("root", "Root", RemoteItemKind::Folder);
            Ok(vec![RemoteSource {
                id: SourceId::new("source").unwrap(),
                name: "Source".to_owned(),
                root,
            }])
        }

        async fn list_children(&self, _parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
            Ok(vec![self.item("file", "file.bin", RemoteItemKind::File)])
        }

        async fn refresh_item(&self, item: &RemoteRef) -> ProviderResult<RemoteItem> {
            Ok(self.item(item.object_id.as_str(), "file.bin", RemoteItemKind::File))
        }

        async fn open_download(
            &self,
            _item: &RemoteItem,
            offset: u64,
        ) -> ProviderResult<RemoteDownload> {
            let bytes = Bytes::from_static(b"payload");
            let stream = stream::once(async move { Ok(bytes) }).boxed();
            if offset == 0 {
                Ok(RemoteDownload::Full {
                    stream,
                    total_size: Some(7),
                })
            } else {
                Ok(RemoteDownload::Partial {
                    start: offset,
                    stream,
                    total_size: Some(offset + 7),
                })
            }
        }
    }

    impl FakeProvider {
        fn item(&self, id: &str, name: &str, kind: RemoteItemKind) -> RemoteItem {
            RemoteItem {
                reference: RemoteRef {
                    identity: self.identity.clone(),
                    source_id: SourceId::new("source").unwrap(),
                    object_id: RemoteObjectId::new(id).unwrap(),
                },
                name: name.to_owned(),
                kind,
                size: (kind == RemoteItemKind::File).then_some(7),
                checksum: None,
                download: DownloadCapability::Supported { resume: true },
            }
        }
    }

    struct FakeFactory {
        id: &'static str,
        readiness: ProviderReadiness,
        error: Option<ProviderError>,
    }

    struct RebuildFailingFactory {
        fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ProviderFactory for RebuildFailingFactory {
        fn id(&self) -> ProviderId {
            ProviderId::new("rebuild").unwrap()
        }

        fn display_name(&self) -> &'static str {
            "Rebuild Test"
        }

        fn settings_schema(&self) -> Vec<ProviderSettingSpec> {
            vec![ProviderSettingSpec {
                key: "endpoint",
                label: "Endpoint",
                kind: ProviderSettingKind::Text,
                required: true,
            }]
        }

        fn load_settings(
            &self,
            _context: &ProviderContext,
        ) -> ProviderResult<ProviderSettingsSnapshot> {
            Ok(ProviderSettingsSnapshot {
                values: BTreeMap::from([(
                    "endpoint".to_owned(),
                    "https://saved.example".to_owned(),
                )]),
                ..ProviderSettingsSnapshot::default()
            })
        }

        fn save_settings(
            &self,
            _context: &ProviderContext,
            _changes: &BTreeMap<String, String>,
        ) -> ProviderResult<()> {
            self.fail.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn readiness(&self, _context: &ProviderContext) -> ProviderReadiness {
            ProviderReadiness::ReadyToConnect
        }

        async fn create(
            &self,
            _context: ProviderContext,
        ) -> ProviderResult<Arc<dyn CloudProvider>> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderError::new(
                    ProviderErrorKind::Config,
                    "saved endpoint was rejected",
                ));
            }
            Ok(Arc::new(FakeProvider {
                identity: ProviderIdentity::new(
                    ProviderId::new("rebuild").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
            }))
        }
    }

    #[async_trait]
    impl ProviderFactory for FakeFactory {
        fn id(&self) -> ProviderId {
            ProviderId::new(self.id).unwrap()
        }

        fn display_name(&self) -> &'static str {
            self.id
        }

        fn readiness(&self, _context: &ProviderContext) -> ProviderReadiness {
            self.readiness
        }

        async fn create(
            &self,
            _context: ProviderContext,
        ) -> ProviderResult<Arc<dyn CloudProvider>> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(Arc::new(FakeProvider {
                identity: ProviderIdentity::new(
                    ProviderId::new(self.id).unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
            }))
        }
    }

    fn context() -> ProviderContext {
        ProviderContext {
            config_root: PathBuf::from("/tmp/pan-tui-test-config"),
            network: NetworkSettings::default(),
            config_overrides: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn initializes_ready_needs_config_and_failed_providers_independently() {
        let mut registry = ProviderRegistry::new();
        registry
            .register(FakeFactory {
                id: "good",
                readiness: ProviderReadiness::ReadyToConnect,
                error: None,
            })
            .unwrap();
        registry
            .register(FakeFactory {
                id: "missing",
                readiness: ProviderReadiness::NeedsConfig,
                error: None,
            })
            .unwrap();
        registry
            .register(FakeFactory {
                id: "bad",
                readiness: ProviderReadiness::ReadyToConnect,
                error: Some(ProviderError::new(
                    ProviderErrorKind::Auth,
                    "authentication failed",
                )),
            })
            .unwrap();

        let mut manager = ProviderManager::from_registry(&registry);
        manager.initialize(&context()).await;

        assert_eq!(
            manager
                .get(&ProviderId::new("good").unwrap())
                .unwrap()
                .status(),
            &ProviderStatus::Ready
        );
        assert_eq!(
            manager
                .get(&ProviderId::new("missing").unwrap())
                .unwrap()
                .status(),
            &ProviderStatus::NeedsConfig
        );
        assert_eq!(
            manager
                .get(&ProviderId::new("bad").unwrap())
                .unwrap()
                .status(),
            &ProviderStatus::ConnectionError("authentication failed".to_owned())
        );

        let provider = manager
            .get(&ProviderId::new("good").unwrap())
            .unwrap()
            .instance()
            .unwrap();
        let sources = provider.sources().await.unwrap();
        let children = provider
            .list_children(&sources[0].root.reference)
            .await
            .unwrap();
        assert_eq!(children[0].name, "file.bin");
        let operations = provider.operations();
        assert!(operations.list);
        assert!(operations.download);
        assert!(operations.file_info);
        assert!(!operations.upload);
        assert_eq!(
            provider.user_info().await.unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        let RemoteDownload::Full { mut stream, .. } =
            provider.open_download(&children[0], 0).await.unwrap()
        else {
            panic!("expected a full download");
        };
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Bytes::from_static(b"payload")
        );
    }

    #[tokio::test]
    async fn failed_settings_rebuild_retains_previous_instance_and_masked_catalog() {
        let fail = Arc::new(AtomicBool::new(false));
        let mut registry = ProviderRegistry::new();
        registry.register(RebuildFailingFactory { fail }).unwrap();
        let mut runtime = ProviderRuntime::from_registry(&registry, context()).await;
        let id = ProviderId::new("rebuild").unwrap();
        let identity = runtime.catalog()[0].identity.clone();
        let previous = runtime.provider(&identity).unwrap();

        let outcome = runtime
            .save_provider_settings(
                &id,
                &BTreeMap::from([("endpoint".to_owned(), "https://edited.example".to_owned())]),
            )
            .await
            .unwrap();

        assert!(outcome.retained_previous_instance);
        assert!(outcome.provider.available);
        assert!(matches!(
            outcome.provider.status,
            ProviderStatus::ConfigError(ref message) if message == "saved endpoint was rejected"
        ));
        assert_eq!(
            outcome
                .provider
                .settings
                .values
                .get("endpoint")
                .map(String::as_str),
            Some("https://saved.example")
        );
        assert!(Arc::ptr_eq(
            &previous,
            &runtime.provider(&identity).unwrap()
        ));
    }
}
