use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use tokio::sync::{broadcast, mpsc};

use crate::{
    daemon::DaemonClient,
    download::{DownloadEvent, DownloadOptions, JobId},
    ipc::{IpcResponse, IpcResult},
    job::{DownloadJobRequest, DownloadJobSnapshot, DownloadManager},
    provider::{CloudProvider, RemoteItem},
};

pub enum JobUpdate {
    Event(DownloadEvent),
    Snapshot {
        revision: u64,
        jobs: Vec<DownloadJobSnapshot>,
    },
    Disconnected(String),
}

pub enum JobBackend {
    Embedded(EmbeddedJobs),
    Daemon(DaemonJobs),
}

pub struct EmbeddedJobs {
    manager: DownloadManager,
    receiver: broadcast::Receiver<DownloadEvent>,
}

pub struct DaemonJobs {
    client: DaemonClient,
    revision: u64,
    subscription: Option<mpsc::Receiver<std::result::Result<IpcResponse, String>>>,
    pending_submit: Option<(u64, Vec<u8>)>,
    last_reconnect: Option<Instant>,
}

impl JobBackend {
    pub fn embedded(manager: DownloadManager) -> Self {
        let receiver = manager.subscribe();
        Self::Embedded(EmbeddedJobs { manager, receiver })
    }

    pub async fn daemon(client: DaemonClient) -> Result<Self> {
        let subscription = client.subscribe(0).await?;
        Ok(Self::Daemon(DaemonJobs {
            client,
            revision: 0,
            subscription: Some(subscription),
            pending_submit: None,
            last_reconnect: None,
        }))
    }

    pub fn is_daemon(&self) -> bool {
        matches!(self, Self::Daemon(_))
    }

    pub async fn submit(
        &mut self,
        provider: Arc<dyn CloudProvider>,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId> {
        match self {
            Self::Embedded(embedded) => {
                embedded
                    .manager
                    .submit(provider, target, destination, options)
                    .await
            }
            Self::Daemon(daemon) => submit_daemon(daemon, target, destination, options).await,
        }
    }

    pub async fn submit_remote(
        &mut self,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId> {
        match self {
            Self::Daemon(daemon) => submit_daemon(daemon, target, destination, options).await,
            Self::Embedded(_) => bail!("embedded submission requires a Provider instance"),
        }
    }

    pub async fn resume(
        &mut self,
        job_id: JobId,
        provider: Arc<dyn CloudProvider>,
        request: DownloadJobRequest,
    ) -> Result<()> {
        match self {
            Self::Embedded(embedded) => embedded.manager.resume(job_id, provider, request).await,
            Self::Daemon(_) => Ok(()),
        }
    }

    pub fn try_next(&mut self) -> Option<JobUpdate> {
        match self {
            Self::Embedded(embedded) => match embedded.receiver.try_recv() {
                Ok(event) => Some(JobUpdate::Event(event)),
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => Some(
                    JobUpdate::Disconnected(format!("download event stream lagged by {skipped}")),
                ),
                Err(broadcast::error::TryRecvError::Closed) => Some(JobUpdate::Disconnected(
                    "download event stream closed".to_owned(),
                )),
                Err(broadcast::error::TryRecvError::Empty) => None,
            },
            Self::Daemon(daemon) => {
                let receiver = daemon.subscription.as_mut()?;
                match receiver.try_recv() {
                    Ok(Ok(response)) => {
                        if let Some(error) = response.error {
                            return Some(JobUpdate::Disconnected(format!(
                                "{}: {}",
                                error.code, error.message
                            )));
                        }
                        match response.result {
                            Some(IpcResult::Snapshot { revision, jobs }) => {
                                daemon.revision = revision;
                                Some(JobUpdate::Snapshot { revision, jobs })
                            }
                            _ => None,
                        }
                    }
                    Ok(Err(error)) => {
                        daemon.subscription = None;
                        Some(JobUpdate::Disconnected(error))
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        daemon.subscription = None;
                        Some(JobUpdate::Disconnected(
                            "daemon subscription disconnected".to_owned(),
                        ))
                    }
                    Err(mpsc::error::TryRecvError::Empty) => None,
                }
            }
        }
    }

    pub async fn reconnect_if_needed(&mut self) {
        let Self::Daemon(daemon) = self else {
            return;
        };
        if daemon.subscription.is_some()
            || daemon
                .last_reconnect
                .is_some_and(|last| last.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        daemon.last_reconnect = Some(Instant::now());
        if let Ok(subscription) = daemon.client.subscribe(daemon.revision).await {
            daemon.subscription = Some(subscription);
        }
    }

    pub async fn snapshots(&self) -> Result<Vec<DownloadJobSnapshot>> {
        match self {
            Self::Embedded(embedded) => Ok(embedded.manager.snapshots().await),
            Self::Daemon(daemon) => match daemon.client.status().await? {
                IpcResult::Status { jobs, .. } => Ok(jobs),
                _ => bail!("daemon returned an unexpected status response"),
            },
        }
    }

    pub async fn cancel(&self, job_id: JobId) -> Result<bool> {
        match self {
            Self::Embedded(embedded) => Ok(embedded.manager.cancel(job_id).await),
            Self::Daemon(daemon) => {
                daemon.client.cancel(job_id).await?;
                Ok(true)
            }
        }
    }
}

async fn submit_daemon(
    daemon: &mut DaemonJobs,
    target: RemoteItem,
    destination: PathBuf,
    options: DownloadOptions,
) -> Result<JobId> {
    let request = DownloadJobRequest::new(target, destination, options);
    let encoded = serde_json::to_vec(&request)?;
    let request_id = match &daemon.pending_submit {
        Some((request_id, pending)) if pending == &encoded => *request_id,
        _ => {
            let request_id = DaemonClient::new_request_id()?;
            daemon.pending_submit = Some((request_id, encoded));
            request_id
        }
    };
    let result = daemon.client.submit(request_id, request).await?;
    let IpcResult::Submitted { job_id, .. } = result else {
        bail!("daemon returned an unexpected submit response");
    };
    daemon.pending_submit = None;
    Ok(job_id)
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::{
        daemon::DaemonService,
        download::{DownloadJobState, TransferState},
        provider::{
            DownloadCapability, ProfileId, ProviderCapabilities, ProviderId, ProviderIdentity,
            ProviderResult, RemoteDownload, RemoteItemKind, RemoteObjectId, RemoteRef,
            RemoteSource, SourceId,
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

    fn download_options() -> DownloadOptions {
        DownloadOptions {
            overwrite: false,
            resume: true,
            concurrency: 2,
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
            if offset >= 4 {
                return Ok(RemoteDownload::AlreadyComplete { expected_size: 4 });
            }
            let bytes = Bytes::from_static(b"data").slice(offset as usize..);
            let body = stream::once(async move { Ok(bytes) }).boxed();
            if offset == 0 {
                Ok(RemoteDownload::Full {
                    stream: body,
                    total_size: Some(4),
                })
            } else {
                Ok(RemoteDownload::Partial {
                    start: offset,
                    stream: body,
                    total_size: Some(4),
                })
            }
        }
    }

    async fn start_daemon(
        socket: &Path,
        database: &Path,
        provider: Arc<TestProvider>,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let state = StateStore::open(database).unwrap();
        let providers = crate::provider::ProviderRuntime::for_test(vec![provider]);
        let service = DaemonService::prepare(socket.to_path_buf(), state, providers, Vec::new())
            .await
            .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let ready_socket = socket.to_path_buf();
        let server = tokio::spawn(async move {
            service
                .run(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !ready_socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        (stop, server)
    }

    #[tokio::test]
    async fn daemon_backend_subscribes_reconnects_and_avoids_restart_id_collisions() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("daemon.sock");
        let database = temporary.path().join("state.db");
        let provider = Arc::new(TestProvider::new());

        let (stop, server) = start_daemon(&socket, &database, Arc::clone(&provider)).await;
        let mut backend = JobBackend::daemon(DaemonClient::new(
            socket.clone(),
            "stable-client".to_owned(),
        ))
        .await
        .unwrap();
        let first = backend
            .submit(
                provider.clone(),
                provider.file(),
                temporary.path().join("first"),
                download_options(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(JobUpdate::Snapshot { jobs, .. }) = backend.try_next()
                    && jobs.iter().any(|job| {
                        job.id == first
                            && job.state == DownloadJobState::Finished
                            && job
                                .transfers
                                .values()
                                .any(|transfer| transfer.state == TransferState::Completed)
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let _ = stop.send(());
        server.await.unwrap();
        if let JobBackend::Daemon(daemon) = &mut backend {
            daemon.subscription = None;
        }

        let (stop, server) = start_daemon(&socket, &database, Arc::clone(&provider)).await;
        backend.reconnect_if_needed().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    backend.try_next(),
                    Some(JobUpdate::Snapshot { ref jobs, .. })
                        if jobs.iter().any(|job| job.id == first)
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        drop(backend);
        let mut restarted_backend = JobBackend::daemon(DaemonClient::new(
            socket.clone(),
            "stable-client".to_owned(),
        ))
        .await
        .unwrap();
        let second = restarted_backend
            .submit(
                provider.clone(),
                provider.file(),
                temporary.path().join("second"),
                download_options(),
            )
            .await
            .unwrap();
        assert_ne!(first, second);

        let _ = stop.send(());
        server.await.unwrap();
    }
}
