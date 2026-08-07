use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use crate::{
    daemon::DaemonClient,
    download::{DownloadOptions, JobId},
    job::{DownloadJobRequest, DownloadJobSnapshot, DownloadManager},
    job_client::{JobBackend, JobUpdate},
    provider::{
        ProviderCatalogEntry, ProviderId, ProviderIdentity, ProviderRuntime,
        ProviderSettingsSaveResult, RemoteItem, RemoteRef, RemoteSource,
    },
    settings::NetworkSettings,
};

#[async_trait]
pub trait PanClient: Send + Sync {
    fn is_daemon(&self) -> bool;
    async fn provider_catalog(&self) -> Result<Vec<ProviderCatalogEntry>>;
    async fn sources(&self, identity: &ProviderIdentity) -> Result<Vec<RemoteSource>>;
    async fn list_children(&self, parent: &RemoteRef) -> Result<Vec<RemoteItem>>;
    async fn refresh_item(&self, item: &RemoteRef) -> Result<RemoteItem>;
    async fn save_provider_settings(
        &self,
        identity: &ProviderIdentity,
        changes: &BTreeMap<String, String>,
    ) -> Result<ProviderSettingsSaveResult>;
    async fn create_profile(
        &self,
        provider_id: &ProviderId,
        name: &str,
    ) -> Result<ProviderCatalogEntry>;
    async fn reconfigure_network(
        &self,
        network: NetworkSettings,
    ) -> Result<Vec<ProviderCatalogEntry>>;
    async fn submit(
        &self,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId>;
    async fn cancel(&self, job_id: JobId) -> Result<bool>;
    async fn pause(&self, job_id: JobId) -> Result<bool>;
    async fn resume_job(&self, job_id: JobId) -> Result<bool>;
    async fn jobs(&self) -> Result<Vec<DownloadJobSnapshot>>;
    async fn try_next_update(&self) -> Option<JobUpdate>;
    async fn reconnect_if_needed(&self);
}

pub struct EmbeddedPanClient {
    providers: Arc<RwLock<ProviderRuntime>>,
    jobs: Mutex<JobBackend>,
}

impl EmbeddedPanClient {
    pub fn new(providers: ProviderRuntime, jobs: DownloadManager) -> Self {
        Self {
            providers: Arc::new(RwLock::new(providers)),
            jobs: Mutex::new(JobBackend::embedded(jobs)),
        }
    }

    pub async fn resume(&self, recovery: Vec<(JobId, DownloadJobRequest)>) -> Vec<String> {
        let mut failures = Vec::new();
        for (job_id, request) in recovery {
            let provider = self
                .providers
                .read()
                .await
                .provider(&request.target.reference.identity);
            let Some(provider) = provider else {
                failures.push(format!(
                    "Job {} remains interrupted: Provider {}/{} is unavailable",
                    job_id.0,
                    request.target.reference.identity.provider_id,
                    request.target.reference.identity.profile_id
                ));
                continue;
            };
            if let Err(error) = self
                .jobs
                .lock()
                .await
                .resume(job_id, provider, request)
                .await
            {
                failures.push(format!("Could not resume Job {}: {error:#}", job_id.0));
            }
        }
        failures
    }

    async fn provider_for(
        &self,
        identity: &ProviderIdentity,
    ) -> Result<Arc<dyn crate::provider::CloudProvider>> {
        self.providers
            .read()
            .await
            .provider(identity)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Provider {}/{} is unavailable",
                    identity.provider_id,
                    identity.profile_id
                )
            })
    }
}

#[async_trait]
impl PanClient for EmbeddedPanClient {
    fn is_daemon(&self) -> bool {
        false
    }

    async fn provider_catalog(&self) -> Result<Vec<ProviderCatalogEntry>> {
        Ok(self.providers.read().await.catalog())
    }

    async fn sources(&self, identity: &ProviderIdentity) -> Result<Vec<RemoteSource>> {
        Ok(self.provider_for(identity).await?.sources().await?)
    }

    async fn list_children(&self, parent: &RemoteRef) -> Result<Vec<RemoteItem>> {
        Ok(self
            .provider_for(&parent.identity)
            .await?
            .list_children(parent)
            .await?)
    }

    async fn refresh_item(&self, item: &RemoteRef) -> Result<RemoteItem> {
        Ok(self
            .provider_for(&item.identity)
            .await?
            .refresh_item(item)
            .await?)
    }

    async fn save_provider_settings(
        &self,
        identity: &ProviderIdentity,
        changes: &BTreeMap<String, String>,
    ) -> Result<ProviderSettingsSaveResult> {
        Ok(self
            .providers
            .write()
            .await
            .save_provider_settings(identity, changes)
            .await?)
    }

    async fn create_profile(
        &self,
        provider_id: &ProviderId,
        name: &str,
    ) -> Result<ProviderCatalogEntry> {
        Ok(self
            .providers
            .write()
            .await
            .create_profile(provider_id, name)
            .await?)
    }

    async fn reconfigure_network(
        &self,
        network: NetworkSettings,
    ) -> Result<Vec<ProviderCatalogEntry>> {
        Ok(self
            .providers
            .write()
            .await
            .reconfigure_network(network)
            .await)
    }

    async fn submit(
        &self,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId> {
        let provider = self.provider_for(&target.reference.identity).await?;
        self.jobs
            .lock()
            .await
            .submit(provider, target, destination, options)
            .await
    }

    async fn cancel(&self, job_id: JobId) -> Result<bool> {
        self.jobs.lock().await.cancel(job_id).await
    }

    async fn pause(&self, job_id: JobId) -> Result<bool> {
        self.jobs.lock().await.pause(job_id).await
    }

    async fn resume_job(&self, job_id: JobId) -> Result<bool> {
        let snapshot = self
            .jobs
            .lock()
            .await
            .snapshots()
            .await?
            .into_iter()
            .find(|snapshot| snapshot.id == job_id)
            .ok_or_else(|| anyhow::anyhow!("download Job {} was not found", job_id.0))?;
        let provider = self
            .provider_for(&snapshot.request.target.reference.identity)
            .await?;
        self.jobs.lock().await.resume_paused(job_id, provider).await
    }

    async fn jobs(&self) -> Result<Vec<DownloadJobSnapshot>> {
        self.jobs.lock().await.snapshots().await
    }

    async fn try_next_update(&self) -> Option<JobUpdate> {
        self.jobs.lock().await.try_next()
    }

    async fn reconnect_if_needed(&self) {}
}

pub struct DaemonPanClient {
    daemon: DaemonClient,
    jobs: Mutex<JobBackend>,
}

impl DaemonPanClient {
    pub async fn new(daemon: DaemonClient) -> Result<Self> {
        let jobs = JobBackend::daemon(daemon.clone()).await?;
        Ok(Self {
            daemon,
            jobs: Mutex::new(jobs),
        })
    }
}

#[async_trait]
impl PanClient for DaemonPanClient {
    fn is_daemon(&self) -> bool {
        true
    }

    async fn provider_catalog(&self) -> Result<Vec<ProviderCatalogEntry>> {
        self.daemon.providers().await
    }

    async fn sources(&self, identity: &ProviderIdentity) -> Result<Vec<RemoteSource>> {
        self.daemon.sources(identity.clone()).await
    }

    async fn list_children(&self, parent: &RemoteRef) -> Result<Vec<RemoteItem>> {
        self.daemon.list_children(parent.clone()).await
    }

    async fn refresh_item(&self, item: &RemoteRef) -> Result<RemoteItem> {
        self.daemon.refresh_item(item.clone()).await
    }

    async fn save_provider_settings(
        &self,
        identity: &ProviderIdentity,
        changes: &BTreeMap<String, String>,
    ) -> Result<ProviderSettingsSaveResult> {
        self.daemon
            .save_provider_settings(identity.clone(), changes.clone())
            .await
    }

    async fn create_profile(
        &self,
        provider_id: &ProviderId,
        name: &str,
    ) -> Result<ProviderCatalogEntry> {
        self.daemon
            .create_profile(provider_id.clone(), name.to_owned())
            .await
    }

    async fn reconfigure_network(
        &self,
        network: NetworkSettings,
    ) -> Result<Vec<ProviderCatalogEntry>> {
        self.daemon.reconfigure_network(network).await
    }

    async fn submit(
        &self,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId> {
        self.jobs
            .lock()
            .await
            .submit_remote(target, destination, options)
            .await
    }

    async fn cancel(&self, job_id: JobId) -> Result<bool> {
        self.jobs.lock().await.cancel(job_id).await
    }

    async fn pause(&self, job_id: JobId) -> Result<bool> {
        self.jobs.lock().await.pause(job_id).await
    }

    async fn resume_job(&self, job_id: JobId) -> Result<bool> {
        self.daemon.resume(job_id).await?;
        Ok(true)
    }

    async fn jobs(&self) -> Result<Vec<DownloadJobSnapshot>> {
        self.jobs.lock().await.snapshots().await
    }

    async fn try_next_update(&self) -> Option<JobUpdate> {
        self.jobs.lock().await.try_next()
    }

    async fn reconnect_if_needed(&self) {
        self.jobs.lock().await.reconnect_if_needed().await;
    }
}

pub fn require_provider(
    providers: &[ProviderCatalogEntry],
    id: &ProviderId,
) -> Result<ProviderCatalogEntry> {
    providers
        .iter()
        .find(|provider| &provider.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider {id} is not registered"))
}

pub fn require_available(provider: &ProviderCatalogEntry) -> Result<()> {
    if provider.available {
        Ok(())
    } else {
        bail!("provider {} is not available", provider.id)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::{
        daemon::DaemonService,
        provider::{
            CloudProvider, DownloadCapability, ProfileId, ProviderCapabilities, ProviderIdentity,
            ProviderResult, RemoteDownload, RemoteItemKind, RemoteObjectId, SourceId,
        },
        state::StateStore,
    };

    struct TestProvider {
        identity: ProviderIdentity,
    }

    impl TestProvider {
        fn new() -> Self {
            Self {
                identity: ProviderIdentity::new(
                    ProviderId::new("test").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
            }
        }

        fn item(&self, id: &str, name: &str, kind: RemoteItemKind) -> RemoteItem {
            RemoteItem {
                reference: RemoteRef {
                    identity: self.identity.clone(),
                    source_id: SourceId::new("source").unwrap(),
                    object_id: RemoteObjectId::new(id).unwrap(),
                },
                name: name.to_owned(),
                kind,
                size: (kind == RemoteItemKind::File).then_some(4),
                checksum: None,
                download: DownloadCapability::Supported { resume: true },
            }
        }
    }

    #[async_trait]
    impl CloudProvider for TestProvider {
        fn identity(&self) -> &ProviderIdentity {
            &self.identity
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn sources(&self) -> ProviderResult<Vec<RemoteSource>> {
            Ok(vec![RemoteSource {
                id: SourceId::new("source").unwrap(),
                name: "Source".to_owned(),
                root: self.item("root", "Root", RemoteItemKind::Folder),
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
            let body = stream::once(async { Ok(Bytes::from_static(b"data")) }).boxed();
            Ok(RemoteDownload::Partial {
                start: offset,
                stream: body,
                total_size: Some(offset + 4),
            })
        }
    }

    #[tokio::test]
    async fn embedded_and_daemon_clients_expose_the_same_provider_contract() {
        let embedded_provider: Arc<dyn CloudProvider> = Arc::new(TestProvider::new());
        let embedded = EmbeddedPanClient::new(
            ProviderRuntime::for_test(vec![embedded_provider]),
            DownloadManager::new(),
        );

        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("daemon.sock");
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        let daemon_provider: Arc<dyn CloudProvider> = Arc::new(TestProvider::new());
        let service = DaemonService::prepare(
            socket.clone(),
            state,
            ProviderRuntime::for_test(vec![daemon_provider]),
            Vec::new(),
        )
        .await
        .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            service
                .run(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let daemon = DaemonPanClient::new(DaemonClient::new(
            socket.clone(),
            "pan-client-parity".to_owned(),
        ))
        .await
        .unwrap();

        let embedded_catalog = embedded.provider_catalog().await.unwrap();
        let daemon_catalog = daemon.provider_catalog().await.unwrap();
        assert_eq!(daemon_catalog, embedded_catalog);
        let identity = embedded_catalog[0].identity.clone();
        let embedded_sources = embedded.sources(&identity).await.unwrap();
        let daemon_sources = daemon.sources(&identity).await.unwrap();
        assert_eq!(daemon_sources, embedded_sources);
        let root = embedded_sources[0].root.reference.clone();
        let embedded_children = embedded.list_children(&root).await.unwrap();
        let daemon_children = daemon.list_children(&root).await.unwrap();
        assert_eq!(daemon_children, embedded_children);
        assert_eq!(
            daemon
                .refresh_item(&embedded_children[0].reference)
                .await
                .unwrap(),
            embedded
                .refresh_item(&embedded_children[0].reference)
                .await
                .unwrap()
        );

        let _ = stop.send(());
        server.await.unwrap();
        assert!(!socket.exists());
    }
}
