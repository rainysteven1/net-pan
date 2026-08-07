use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{RwLock, broadcast, mpsc},
};

use crate::{
    download::JobId,
    job::{DownloadJobRequest, DownloadJobSnapshot, DownloadManager},
    provider::{
        ProviderCatalogEntry, ProviderError, ProviderErrorKind, ProviderId, ProviderIdentity,
        ProviderRuntime, ProviderSettingsSaveResult, RemoteItem, RemoteRef, RemoteSource,
    },
    settings::NetworkSettings,
    state::{StateStore, default_state_root},
};

pub const PROTOCOL_VERSION: u32 = 5;
const MAX_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcRequest {
    pub version: u32,
    pub client_id: String,
    pub request_id: u64,
    pub command: IpcCommand,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcCommand {
    Status,
    Providers,
    Sources {
        identity: ProviderIdentity,
    },
    ListChildren {
        parent: RemoteRef,
    },
    RefreshItem {
        item: RemoteRef,
    },
    SaveProviderSettings {
        identity: ProviderIdentity,
        changes: BTreeMap<String, String>,
    },
    CreateProfile {
        provider_id: ProviderId,
        name: String,
    },
    ReconfigureNetwork {
        network: NetworkSettings,
    },
    Submit {
        request: Box<DownloadJobRequest>,
    },
    Cancel {
        job_id: JobId,
    },
    Pause {
        job_id: JobId,
    },
    Resume {
        job_id: JobId,
    },
    Subscribe {
        after_revision: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcResponse {
    pub version: u32,
    pub request_id: u64,
    pub result: Option<IpcResult>,
    pub error: Option<IpcError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResult {
    Status {
        revision: u64,
        jobs: Vec<DownloadJobSnapshot>,
    },
    Providers {
        providers: Vec<ProviderCatalogEntry>,
    },
    Sources {
        sources: Vec<RemoteSource>,
    },
    Children {
        items: Vec<RemoteItem>,
    },
    Item {
        item: RemoteItem,
    },
    ProviderSettingsSaved {
        outcome: ProviderSettingsSaveResult,
    },
    ProfileCreated {
        profile: ProviderCatalogEntry,
    },
    NetworkReconfigured {
        providers: Vec<ProviderCatalogEntry>,
    },
    Submitted {
        job_id: JobId,
        reused: bool,
    },
    Cancelled {
        job_id: JobId,
    },
    Paused {
        job_id: JobId,
    },
    Resumed {
        job_id: JobId,
    },
    Snapshot {
        revision: u64,
        jobs: Vec<DownloadJobSnapshot>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone)]
pub struct DaemonRuntime {
    jobs: DownloadManager,
    state: StateStore,
    providers: Arc<RwLock<ProviderRuntime>>,
    revision: Arc<AtomicU64>,
    changes: broadcast::Sender<u64>,
}

impl DaemonRuntime {
    pub fn new(jobs: DownloadManager, state: StateStore, providers: ProviderRuntime) -> Self {
        let revision = Arc::new(AtomicU64::new(0));
        let (changes, _) = broadcast::channel(1024);
        let mut events = jobs.subscribe();
        let event_revision = Arc::clone(&revision);
        let event_changes = changes.clone();
        tokio::spawn(async move {
            while let Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) = events.recv().await {
                let revision = event_revision.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = event_changes.send(revision);
            }
        });
        Self {
            jobs,
            state,
            providers: Arc::new(RwLock::new(providers)),
            revision,
            changes,
        }
    }

    pub async fn recover(&self) -> Result<usize> {
        let recovered = self.state.recover_interrupted()?;
        for (job_id, request) in self.state.load_interrupted_requests()? {
            let Some(provider) = self.provider_for(&request).await else {
                continue;
            };
            self.jobs.resume(job_id, provider, request).await?;
        }
        Ok(recovered)
    }

    pub fn state_store(&self) -> StateStore {
        self.state.clone()
    }

    async fn execute(
        &self,
        client_id: &str,
        request_id: u64,
        command: IpcCommand,
    ) -> Result<IpcResult, IpcError> {
        match command {
            IpcCommand::Status => Ok(IpcResult::Status {
                revision: self.revision.load(Ordering::Relaxed),
                jobs: self.snapshots().await.map_err(internal_error)?,
            }),
            IpcCommand::Providers => Ok(IpcResult::Providers {
                providers: self.providers.read().await.catalog(),
            }),
            IpcCommand::Sources { identity } => {
                let provider = self
                    .provider_for_identity(&identity)
                    .await
                    .ok_or_else(|| provider_unavailable(&identity))?;
                Ok(IpcResult::Sources {
                    sources: provider.sources().await.map_err(provider_error)?,
                })
            }
            IpcCommand::ListChildren { parent } => {
                let provider = self
                    .provider_for_identity(&parent.identity)
                    .await
                    .ok_or_else(|| provider_unavailable(&parent.identity))?;
                Ok(IpcResult::Children {
                    items: provider
                        .list_children(&parent)
                        .await
                        .map_err(provider_error)?,
                })
            }
            IpcCommand::RefreshItem { item } => {
                let provider = self
                    .provider_for_identity(&item.identity)
                    .await
                    .ok_or_else(|| provider_unavailable(&item.identity))?;
                Ok(IpcResult::Item {
                    item: provider.refresh_item(&item).await.map_err(provider_error)?,
                })
            }
            IpcCommand::SaveProviderSettings { identity, changes } => {
                let outcome = self
                    .providers
                    .write()
                    .await
                    .save_provider_settings(&identity, &changes)
                    .await
                    .map_err(provider_error)?;
                Ok(IpcResult::ProviderSettingsSaved { outcome })
            }
            IpcCommand::CreateProfile { provider_id, name } => {
                let profile = self
                    .providers
                    .write()
                    .await
                    .create_profile(&provider_id, &name)
                    .await
                    .map_err(provider_error)?;
                Ok(IpcResult::ProfileCreated { profile })
            }
            IpcCommand::ReconfigureNetwork { network } => {
                let providers = self
                    .providers
                    .write()
                    .await
                    .reconfigure_network(network)
                    .await;
                Ok(IpcResult::NetworkReconfigured { providers })
            }
            IpcCommand::Submit { request } => {
                let provider = self
                    .provider_for(&request)
                    .await
                    .ok_or_else(|| provider_unavailable(&request.target.reference.identity))?;
                let (job_id, created) = self
                    .jobs
                    .submit_idempotent(client_id, request_id, provider, *request)
                    .await
                    .map_err(internal_error)?;
                Ok(IpcResult::Submitted {
                    job_id,
                    reused: !created,
                })
            }
            IpcCommand::Cancel { job_id } => {
                if !self.jobs.cancel(job_id).await {
                    return Err(IpcError {
                        code: "job_not_active".to_owned(),
                        message: format!("Job {} is not active", job_id.0),
                        retryable: false,
                    });
                }
                Ok(IpcResult::Cancelled { job_id })
            }
            IpcCommand::Pause { job_id } => {
                if !self.jobs.pause(job_id).await {
                    return Err(IpcError {
                        code: "job_not_active".to_owned(),
                        message: format!("Job {} is not active", job_id.0),
                        retryable: false,
                    });
                }
                Ok(IpcResult::Paused { job_id })
            }
            IpcCommand::Resume { job_id } => {
                let snapshot = self.jobs.snapshot(job_id).await.ok_or_else(|| IpcError {
                    code: "job_not_found".to_owned(),
                    message: format!("Job {} was not found", job_id.0),
                    retryable: false,
                })?;
                let provider = self.provider_for(&snapshot.request).await.ok_or_else(|| {
                    provider_unavailable(&snapshot.request.target.reference.identity)
                })?;
                self.jobs
                    .resume_paused(job_id, provider)
                    .await
                    .map_err(internal_error)?;
                Ok(IpcResult::Resumed { job_id })
            }
            IpcCommand::Subscribe { .. } => Err(IpcError {
                code: "invalid_request".to_owned(),
                message: "subscribe must use a streaming IPC connection".to_owned(),
                retryable: false,
            }),
        }
    }

    pub async fn snapshots(&self) -> Result<Vec<DownloadJobSnapshot>> {
        let persisted = self.state.load_snapshots()?;
        let mut jobs: BTreeMap<_, _> = persisted
            .into_iter()
            .map(|snapshot| (snapshot.id, snapshot))
            .collect();
        for snapshot in self.jobs.snapshots().await {
            jobs.insert(snapshot.id, snapshot);
        }
        Ok(jobs.into_values().collect())
    }

    async fn provider_for(
        &self,
        request: &DownloadJobRequest,
    ) -> Option<Arc<dyn crate::provider::CloudProvider>> {
        self.provider_for_identity(&request.target.reference.identity)
            .await
    }

    async fn provider_for_identity(
        &self,
        identity: &ProviderIdentity,
    ) -> Option<Arc<dyn crate::provider::CloudProvider>> {
        self.providers.read().await.provider(identity)
    }
}

fn provider_unavailable(identity: &ProviderIdentity) -> IpcError {
    IpcError {
        code: "provider_unavailable".to_owned(),
        message: format!(
            "Provider {}/{} is unavailable",
            identity.provider_id, identity.profile_id
        ),
        retryable: true,
    }
}

fn provider_error(error: ProviderError) -> IpcError {
    let code = match error.kind {
        ProviderErrorKind::Config => "provider_config",
        ProviderErrorKind::Auth => "provider_auth",
        ProviderErrorKind::Permission => "provider_permission",
        ProviderErrorKind::RateLimit => "provider_rate_limit",
        ProviderErrorKind::Network => "provider_network",
        ProviderErrorKind::NotFound => "provider_not_found",
        ProviderErrorKind::Unsupported => "provider_unsupported",
        ProviderErrorKind::Internal => "provider_internal",
    };
    IpcError {
        code: code.to_owned(),
        message: error.message,
        retryable: error.retryable,
    }
}

pub async fn serve<F>(socket: &Path, runtime: DaemonRuntime, shutdown: F) -> Result<()>
where
    F: Future<Output = ()>,
{
    prepare_socket(socket).await?;
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("failed to bind daemon socket: {}", socket.display()))?;
    restrict_socket(socket)?;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, runtime).await;
                });
            }
            _ = &mut shutdown => break,
        }
    }
    drop(listener);
    if socket.exists() {
        fs::remove_file(socket)
            .with_context(|| format!("failed to remove daemon socket: {}", socket.display()))?;
    }
    Ok(())
}

pub async fn request(socket: &Path, request: &IpcRequest) -> Result<IpcResponse> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("failed to connect to daemon: {}", socket.display()))?;
    let mut encoded = serde_json::to_vec(request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.shutdown().await?;
    let mut reader = BufReader::new(stream).take(MAX_MESSAGE_BYTES);
    let mut response = String::new();
    reader.read_line(&mut response).await?;
    if response.is_empty() {
        bail!("daemon closed the connection without a response");
    }
    serde_json::from_str(&response).context("daemon returned an invalid IPC response")
}

pub async fn subscribe(
    socket: &Path,
    client_id: String,
    after_revision: u64,
) -> Result<mpsc::Receiver<std::result::Result<IpcResponse, String>>> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("failed to connect to daemon: {}", socket.display()))?;
    let request = IpcRequest {
        version: PROTOCOL_VERSION,
        client_id,
        request_id: after_revision,
        command: IpcCommand::Subscribe { after_revision },
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    let parsed = serde_json::from_str(&line)
                        .map_err(|error| format!("invalid daemon subscription response: {error}"));
                    if sender.send(parsed).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = sender
                        .send(Err("daemon subscription disconnected".to_owned()))
                        .await;
                    break;
                }
                Err(error) => {
                    let _ = sender
                        .send(Err(format!("daemon subscription failed: {error}")))
                        .await;
                    break;
                }
            }
        }
    });
    Ok(receiver)
}

pub fn default_socket_path() -> Result<PathBuf> {
    Ok(default_state_root()?.join("net-pan.sock"))
}

pub fn load_or_create_client_id() -> Result<String> {
    use sha2::{Digest, Sha256};

    let root = default_state_root()?;
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let path = root.join("client-id");
    if let Ok(value) = fs::read_to_string(&path) {
        return validate_stored_client_id(value);
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    let digest = Sha256::digest(format!("{}:{nonce}", std::process::id()).as_bytes());
    let value = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_stored_client_id(fs::read_to_string(path)?)
        }
        Err(error) => Err(error).context("failed to persist IPC client ID"),
    }
}

fn validate_stored_client_id(value: String) -> Result<String> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > 128 {
        bail!("stored IPC client ID is invalid");
    }
    Ok(value)
}

async fn handle_connection(stream: UnixStream, runtime: DaemonRuntime) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader).take(MAX_MESSAGE_BYTES);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let parsed = serde_json::from_str::<IpcRequest>(&line);
    if let Ok(IpcRequest {
        version: PROTOCOL_VERSION,
        client_id,
        request_id,
        command: IpcCommand::Subscribe { after_revision },
    }) = parsed.as_ref()
    {
        return handle_subscription(
            &mut writer,
            &runtime,
            client_id,
            *request_id,
            *after_revision,
        )
        .await;
    }
    let response = match parsed {
        Ok(request) if request.version == PROTOCOL_VERSION => {
            let request_id = request.request_id;
            match runtime
                .execute(&request.client_id, request_id, request.command)
                .await
            {
                Ok(result) => IpcResponse {
                    version: PROTOCOL_VERSION,
                    request_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => IpcResponse {
                    version: PROTOCOL_VERSION,
                    request_id,
                    result: None,
                    error: Some(error),
                },
            }
        }
        Ok(request) => IpcResponse {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: None,
            error: Some(IpcError {
                code: "version_mismatch".to_owned(),
                message: format!(
                    "IPC protocol {} is not supported; daemon uses {}",
                    request.version, PROTOCOL_VERSION
                ),
                retryable: false,
            }),
        },
        Err(error) => IpcResponse {
            version: PROTOCOL_VERSION,
            request_id: 0,
            result: None,
            error: Some(IpcError {
                code: "invalid_request".to_owned(),
                message: format!("invalid IPC request: {error}"),
                retryable: false,
            }),
        },
    };
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.shutdown().await?;
    Ok(())
}

async fn handle_subscription<W>(
    writer: &mut W,
    runtime: &DaemonRuntime,
    _client_id: &str,
    request_id: u64,
    _after_revision: u64,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut changes = runtime.changes.subscribe();
    let current_revision = runtime.revision.load(Ordering::Relaxed);
    write_response(
        writer,
        &IpcResponse {
            version: PROTOCOL_VERSION,
            request_id,
            result: Some(IpcResult::Snapshot {
                revision: current_revision,
                jobs: runtime.snapshots().await?,
            }),
            error: None,
        },
    )
    .await?;
    loop {
        let revision = match changes.recv().await {
            Ok(revision) => revision,
            Err(broadcast::error::RecvError::Lagged(_)) => runtime.revision.load(Ordering::Relaxed),
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        };
        write_response(
            writer,
            &IpcResponse {
                version: PROTOCOL_VERSION,
                request_id,
                result: Some(IpcResult::Snapshot {
                    revision,
                    jobs: runtime.snapshots().await?,
                }),
                error: None,
            },
        )
        .await?;
    }
}

async fn write_response<W>(writer: &mut W, response: &IpcResponse) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(response)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

async fn prepare_socket(socket: &Path) -> Result<()> {
    let parent = socket
        .parent()
        .context("daemon socket path has no parent")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if socket.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            let metadata = fs::symlink_metadata(socket)?;
            if !metadata.file_type().is_socket() {
                bail!(
                    "daemon socket path exists but is not a Unix socket: {}",
                    socket.display()
                );
            }
        }
        match UnixStream::connect(socket).await {
            Ok(_) => bail!("another net-pan daemon is already running"),
            Err(_) => fs::remove_file(socket).with_context(|| {
                format!("failed to remove stale daemon socket: {}", socket.display())
            })?,
        }
    }
    Ok(())
}

fn restrict_socket(socket: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn internal_error(error: impl std::fmt::Display) -> IpcError {
    IpcError {
        code: "internal".to_owned(),
        message: error.to_string(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::{
        download::DownloadOptions,
        provider::{
            CloudProvider, DownloadCapability, ProfileId, ProviderCapabilities, ProviderId,
            ProviderIdentity, ProviderResult, RemoteDownload, RemoteItem, RemoteItemKind,
            RemoteObjectId, RemoteRef, RemoteSource, SourceId,
        },
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

        fn file(&self) -> RemoteItem {
            RemoteItem {
                reference: RemoteRef {
                    identity: self.identity.clone(),
                    source_id: SourceId::new("source").unwrap(),
                    object_id: RemoteObjectId::new("file").unwrap(),
                },
                name: "file.bin".to_owned(),
                kind: RemoteItemKind::File,
                size: Some(4),
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
            ProviderCapabilities {
                ranged_downloads: true,
                checksums: false,
            }
        }

        async fn sources(&self) -> ProviderResult<Vec<RemoteSource>> {
            Ok(Vec::new())
        }

        async fn list_children(&self, _parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
            Ok(Vec::new())
        }

        async fn refresh_item(&self, _item: &RemoteRef) -> ProviderResult<RemoteItem> {
            Ok(self.file())
        }

        async fn open_download(
            &self,
            _item: &RemoteItem,
            offset: u64,
        ) -> ProviderResult<RemoteDownload> {
            let body = stream::once(async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(Bytes::from_static(b"data"))
            })
            .boxed();
            if offset == 0 {
                Ok(RemoteDownload::Full {
                    stream: body,
                    total_size: Some(4),
                })
            } else {
                Ok(RemoteDownload::Partial {
                    start: offset,
                    stream: body,
                    total_size: Some(offset + 4),
                })
            }
        }
    }

    #[tokio::test]
    async fn serves_versioned_status_submit_and_cancel_over_private_socket() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("daemon.sock");
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        let jobs = DownloadManager::persistent(state.clone()).unwrap();
        let provider = Arc::new(TestProvider::new());
        let providers = ProviderRuntime::for_test(vec![provider.clone()]);
        let runtime = DaemonRuntime::new(jobs, state, providers);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server_socket = socket.clone();
        let server = tokio::spawn(async move {
            serve(&server_socket, runtime, async {
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let providers = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "browser-client".to_owned(),
                request_id: 1,
                command: IpcCommand::Providers,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            providers.result,
            Some(IpcResult::Providers { ref providers })
                if providers.len() == 1 && providers[0].available
        ));

        let sources = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "browser-client".to_owned(),
                request_id: 2,
                command: IpcCommand::Sources {
                    identity: provider.identity.clone(),
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            sources.result,
            Some(IpcResult::Sources { ref sources }) if sources.is_empty()
        ));

        let refreshed = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "browser-client".to_owned(),
                request_id: 3,
                command: IpcCommand::RefreshItem {
                    item: provider.file().reference,
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            refreshed.result,
            Some(IpcResult::Item { ref item }) if item.name == "file.bin"
        ));

        let job_request = DownloadJobRequest::new(
            provider.file(),
            temporary.path().join("downloads"),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
        );
        let submitted = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "test-client".to_owned(),
                request_id: 10,
                command: IpcCommand::Submit {
                    request: Box::new(job_request.clone()),
                },
            },
        )
        .await
        .unwrap();
        let Some(IpcResult::Submitted { job_id, reused }) = submitted.result else {
            panic!("expected submitted response");
        };
        assert!(!reused);

        let duplicate = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "test-client".to_owned(),
                request_id: 10,
                command: IpcCommand::Submit {
                    request: Box::new(job_request),
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            duplicate.result,
            Some(IpcResult::Submitted {
                job_id: duplicate_id,
                reused: true,
            }) if duplicate_id == job_id
        ));

        let mut subscription = subscribe(&socket, "subscriber".to_owned(), 0)
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(1), subscription.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            update.result,
            Some(IpcResult::Snapshot { ref jobs, .. }) if jobs.len() == 1
        ));

        let cancelled = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "test-client".to_owned(),
                request_id: 11,
                command: IpcCommand::Cancel { job_id },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            cancelled.result,
            Some(IpcResult::Cancelled { .. })
        ));

        let status = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: "test-client".to_owned(),
                request_id: 12,
                command: IpcCommand::Status,
            },
        )
        .await
        .unwrap();
        let Some(IpcResult::Status { jobs, .. }) = status.result else {
            panic!("expected status response");
        };
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, crate::download::DownloadJobState::Cancelled);

        let mismatch = request(
            &socket,
            &IpcRequest {
                version: PROTOCOL_VERSION + 1,
                client_id: "test-client".to_owned(),
                request_id: 13,
                command: IpcCommand::Status,
            },
        )
        .await
        .unwrap();
        assert_eq!(mismatch.error.unwrap().code, "version_mismatch");

        let _ = stop.send(());
        server.await.unwrap();
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn create_profile_and_save_settings_are_scoped_by_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let context = crate::provider::ProviderContext {
            config_root: temporary.path().join("config"),
            network: NetworkSettings::default(),
        };
        let mut registry = crate::provider::ProviderRegistry::new();
        registry
            .register(crate::provider::google::GoogleProviderFactory)
            .unwrap();
        let providers = ProviderRuntime::from_registry(&registry, context).await;
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        let runtime = DaemonRuntime::new(DownloadManager::new(), state, providers);
        let first = match runtime
            .execute("profile-client", 1, IpcCommand::Providers)
            .await
            .unwrap()
        {
            IpcResult::Providers { providers } => providers[0].clone(),
            _ => panic!("expected Provider catalog"),
        };
        let second = match runtime
            .execute(
                "profile-client",
                2,
                IpcCommand::CreateProfile {
                    provider_id: ProviderId::new("google").unwrap(),
                    name: "Personal Google".to_owned(),
                },
            )
            .await
            .unwrap()
        {
            IpcResult::ProfileCreated { profile } => profile,
            _ => panic!("expected created Profile"),
        };
        assert_ne!(first.identity, second.identity);

        let saved = runtime
            .execute(
                "profile-client",
                3,
                IpcCommand::SaveProviderSettings {
                    identity: second.identity.clone(),
                    changes: BTreeMap::from([("name".to_owned(), "Archive Google".to_owned())]),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            saved,
            IpcResult::ProviderSettingsSaved { ref outcome }
                if outcome.provider.identity == second.identity
                    && outcome.provider.profile_name == "Archive Google"
        ));

        let catalog = match runtime
            .execute("profile-client", 4, IpcCommand::Providers)
            .await
            .unwrap()
        {
            IpcResult::Providers { providers } => providers,
            _ => panic!("expected Provider catalog"),
        };
        assert_eq!(catalog.len(), 2);
        assert_eq!(
            catalog
                .iter()
                .find(|entry| entry.identity == first.identity)
                .unwrap()
                .profile_name,
            first.profile_name
        );
    }

    #[tokio::test]
    async fn refuses_to_remove_a_non_socket_at_the_daemon_path() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("daemon.sock");
        fs::write(&path, b"keep me").unwrap();

        let error = prepare_socket(&path).await.unwrap_err();

        assert!(error.to_string().contains("not a Unix socket"));
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
    }
}
