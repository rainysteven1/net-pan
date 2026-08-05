use std::{collections::BTreeMap, env, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use pan_tui::{
    client::{EmbeddedPanClient, PanClient},
    download::DownloadOptions,
    job::DownloadManager,
    provider::{
        CloudProvider, DownloadCapability, ProfileId, ProviderCapabilities, ProviderContext,
        ProviderFactory, ProviderId, ProviderIdentity, ProviderReadiness, ProviderRegistry,
        ProviderResult, ProviderRuntime, RemoteDownload, RemoteItem, RemoteItemKind,
        RemoteObjectId, RemoteRef, RemoteSource, SourceId,
    },
    settings::NetworkSettings,
    tui::{BrowserProvider, BrowserRuntime, run_browser},
};

struct DemoProvider {
    identity: ProviderIdentity,
    label: &'static str,
}

impl DemoProvider {
    fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            identity: ProviderIdentity::new(
                ProviderId::new(id).unwrap(),
                ProfileId::new("default").unwrap(),
            ),
            label,
        }
    }

    fn item(&self, source: &SourceId, id: &str, name: String, kind: RemoteItemKind) -> RemoteItem {
        RemoteItem {
            reference: RemoteRef {
                identity: self.identity.clone(),
                source_id: source.clone(),
                object_id: RemoteObjectId::new(id).unwrap(),
            },
            name,
            kind,
            size: (kind == RemoteItemKind::File).then_some(128 * 1024),
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        }
    }
}

#[async_trait]
impl CloudProvider for DemoProvider {
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
        let source = SourceId::new(format!("{}-source", self.identity.provider_id)).unwrap();
        Ok(vec![RemoteSource {
            id: source.clone(),
            name: format!("{} Source", self.label),
            root: self.item(
                &source,
                "root",
                format!("{} Root", self.label),
                RemoteItemKind::Folder,
            ),
        }])
    }

    async fn list_children(&self, parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
        let source = &parent.source_id;
        Ok(match parent.object_id.as_str() {
            "root" => vec![self.item(
                source,
                "batch",
                "parallel-batch".to_owned(),
                RemoteItemKind::Folder,
            )],
            "batch" => (1..=4)
                .map(|index| {
                    self.item(
                        source,
                        &format!("file-{index}"),
                        format!("part-{index}.bin"),
                        RemoteItemKind::File,
                    )
                })
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn refresh_item(&self, item: &RemoteRef) -> ProviderResult<RemoteItem> {
        Ok(self.item(
            &item.source_id,
            item.object_id.as_str(),
            item.object_id.to_string(),
            RemoteItemKind::File,
        ))
    }

    async fn open_download(
        &self,
        _item: &RemoteItem,
        offset: u64,
    ) -> ProviderResult<RemoteDownload> {
        let total = 128 * 1024_u64;
        let stream = stream::unfold(offset, move |position| async move {
            if position >= total {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
            let size = (total - position).min(1024) as usize;
            Some((Ok(Bytes::from(vec![b'x'; size])), position + size as u64))
        })
        .boxed();
        if offset == 0 {
            Ok(RemoteDownload::Full {
                stream,
                total_size: Some(total),
            })
        } else {
            Ok(RemoteDownload::Partial {
                start: offset,
                stream,
                total_size: Some(total),
            })
        }
    }
}

struct DemoFactory {
    id: &'static str,
    label: &'static str,
}

#[async_trait]
impl ProviderFactory for DemoFactory {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.id).unwrap()
    }

    fn display_name(&self) -> &'static str {
        self.label
    }

    fn readiness(&self, _context: &ProviderContext) -> ProviderReadiness {
        ProviderReadiness::ReadyToConnect
    }

    async fn create(&self, _context: ProviderContext) -> ProviderResult<Arc<dyn CloudProvider>> {
        Ok(Arc::new(DemoProvider::new(self.id, self.label)))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let config_root = home.join(".config/pan-tui-demo");
    let context = ProviderContext {
        config_root: config_root.clone(),
        network: NetworkSettings::default(),
        config_overrides: BTreeMap::new(),
    };
    let mut registry = ProviderRegistry::new();
    registry.register(DemoFactory {
        id: "alpha",
        label: "Alpha Drive",
    })?;
    registry.register(DemoFactory {
        id: "beta",
        label: "Beta Drive",
    })?;
    let provider_runtime = ProviderRuntime::from_registry(&registry, context).await;
    let client: Arc<dyn PanClient> = Arc::new(EmbeddedPanClient::new(
        provider_runtime,
        DownloadManager::new(),
    ));
    let providers = client
        .provider_catalog()
        .await?
        .into_iter()
        .map(BrowserProvider::new)
        .collect();
    let destination = env::var_os("PAN_TUI_DEMO_DEST")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join(format!("pan-tui-demo-{}", std::process::id())));
    run_browser(
        providers,
        0,
        BrowserRuntime::new(
            client,
            config_root.join("config.toml"),
            NetworkSettings::default(),
            destination,
            DownloadOptions {
                overwrite: true,
                resume: true,
                concurrency: 2,
            },
        ),
    )
    .await?;
    Ok(())
}
