use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, broadcast, mpsc, oneshot},
    task::AbortHandle,
};

use crate::{
    download::{
        DownloadEvent, DownloadJobState, DownloadOptions, DownloadSummary, JobId, TransferId,
        TransferState, run_download_job,
    },
    provider::{CloudProvider, RemoteItem},
    state::{StateStore, SubmissionPersistence, TransferProgress},
};

const EVENT_CAPACITY: usize = 8192;
const WORKER_EVENT_CAPACITY: usize = 2048;
const PROGRESS_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadJobRequest {
    pub schema_version: u32,
    pub target: RemoteItem,
    pub destination: PathBuf,
    pub options: DownloadOptions,
}

impl DownloadJobRequest {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(target: RemoteItem, destination: PathBuf, options: DownloadOptions) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            target,
            destination,
            options,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferSnapshot {
    pub id: TransferId,
    pub path: PathBuf,
    pub state: TransferState,
    pub size: Option<u64>,
    pub written: u64,
    #[serde(default)]
    pub bytes_per_second: f64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadJobSnapshot {
    pub id: JobId,
    pub request: DownloadJobRequest,
    pub state: DownloadJobState,
    #[serde(with = "transfer_snapshot_map")]
    pub transfers: BTreeMap<TransferId, TransferSnapshot>,
    pub summary: Option<DownloadSummary>,
    pub error: Option<String>,
}

mod transfer_snapshot_map {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{TransferId, TransferSnapshot};

    pub fn serialize<S>(
        transfers: &BTreeMap<TransferId, TransferSnapshot>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        transfers.values().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<TransferId, TransferSnapshot>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let transfers = Vec::<TransferSnapshot>::deserialize(deserializer)?;
        Ok(transfers
            .into_iter()
            .map(|transfer| (transfer.id, transfer))
            .collect())
    }
}

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<DownloadManagerInner>,
}

struct DownloadManagerInner {
    next_job_id: AtomicU64,
    events: broadcast::Sender<DownloadEvent>,
    tasks: Mutex<BTreeMap<JobId, AbortHandle>>,
    snapshots: Mutex<BTreeMap<JobId, DownloadJobSnapshot>>,
    state: Option<StateStore>,
    persistence_error: Mutex<Option<String>>,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self::with_optional_state(None, 1)
    }

    pub fn persistent(state: StateStore) -> Result<Self> {
        let next_job_id = state.next_job_id()?;
        Ok(Self::with_optional_state(Some(state), next_job_id))
    }

    fn with_optional_state(state: Option<StateStore>, next_job_id: u64) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            inner: Arc::new(DownloadManagerInner {
                next_job_id: AtomicU64::new(next_job_id),
                events,
                tasks: Mutex::new(BTreeMap::new()),
                snapshots: Mutex::new(BTreeMap::new()),
                state,
                persistence_error: Mutex::new(None),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.inner.events.subscribe()
    }

    pub async fn submit(
        &self,
        provider: Arc<dyn CloudProvider>,
        target: RemoteItem,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Result<JobId> {
        let job_id = JobId(self.inner.next_job_id.fetch_add(1, Ordering::Relaxed));
        let request = DownloadJobRequest::new(target, destination, options);
        ensure_provider_matches(provider.as_ref(), &request)?;
        if let Some(state) = &self.inner.state {
            state.record_submission(job_id, &request)?;
        }
        self.start_job(job_id, provider, request).await?;
        Ok(job_id)
    }

    pub async fn submit_idempotent(
        &self,
        client_id: &str,
        request_id: u64,
        provider: Arc<dyn CloudProvider>,
        request: DownloadJobRequest,
    ) -> Result<(JobId, bool)> {
        ensure_provider_matches(provider.as_ref(), &request)?;
        let job_id = JobId(self.inner.next_job_id.fetch_add(1, Ordering::Relaxed));
        let persistence = match &self.inner.state {
            Some(state) => {
                state.record_idempotent_submission(client_id, request_id, job_id, &request)?
            }
            None => SubmissionPersistence::Created,
        };
        match persistence {
            SubmissionPersistence::Existing(existing) => Ok((existing, false)),
            SubmissionPersistence::Created => {
                self.start_job(job_id, provider, request).await?;
                Ok((job_id, true))
            }
        }
    }

    pub async fn resume(
        &self,
        job_id: JobId,
        provider: Arc<dyn CloudProvider>,
        request: DownloadJobRequest,
    ) -> Result<()> {
        ensure!(
            request.schema_version == DownloadJobRequest::SCHEMA_VERSION,
            "unsupported download Job request schema {}",
            request.schema_version
        );
        ensure_provider_matches(provider.as_ref(), &request)?;
        let next = job_id.0.checked_add(1).expect("job ID space exhausted");
        self.inner.next_job_id.fetch_max(next, Ordering::Relaxed);
        if let Some(state) = &self.inner.state {
            state.mark_requeued(job_id)?;
        }
        self.start_job(job_id, provider, request).await
    }

    async fn start_job(
        &self,
        job_id: JobId,
        provider: Arc<dyn CloudProvider>,
        request: DownloadJobRequest,
    ) -> Result<()> {
        ensure!(
            !self.inner.tasks.lock().await.contains_key(&job_id),
            "download Job {} is already active",
            job_id.0
        );
        self.inner.snapshots.lock().await.insert(
            job_id,
            DownloadJobSnapshot {
                id: job_id,
                request: request.clone(),
                state: DownloadJobState::Queued,
                transfers: BTreeMap::new(),
                summary: None,
                error: None,
            },
        );
        let (events, mut receiver) = mpsc::channel(WORKER_EVENT_CAPACITY);
        let event_bus = self.inner.events.clone();
        let state = self.inner.state.clone();
        let manager = Arc::clone(&self.inner);
        let forwarder = tokio::spawn(async move {
            let mut progress = BTreeMap::new();
            let mut interval = tokio::time::interval(PROGRESS_FLUSH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    event = receiver.recv() => {
                        let Some(event) = event else {
                            persist_progress(&manager, state.as_ref(), &mut progress).await;
                            break;
                        };
                        apply_snapshot(&manager.snapshots, &event).await;
                        if let DownloadEvent::Progress {
                            job_id,
                            transfer_id,
                            file_written,
                            file_total,
                            ..
                        } = &event
                        {
                            progress.insert((*job_id, *transfer_id), TransferProgress {
                                job_id: *job_id,
                                transfer_id: *transfer_id,
                                written: *file_written,
                                size: *file_total,
                            });
                        } else if let Some(state) = &state
                            && let Err(error) = state.record_event(&event)
                        {
                            *manager.persistence_error.lock().await = Some(format!("{error:#}"));
                        }
                        if let Some(key) = terminal_transfer(&event) {
                            progress.remove(&key);
                        }
                        let _ = event_bus.send(event);
                    }
                    _ = interval.tick(), if !progress.is_empty() => {
                        persist_progress(&manager, state.as_ref(), &mut progress).await;
                    }
                }
            }
        });
        let tasks = Arc::clone(&self.inner);
        let (start, ready) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = ready.await;
            run_download_job(
                job_id,
                provider,
                request.target,
                request.destination,
                request.options,
                events,
            )
            .await;
            let _ = forwarder.await;
            tasks.tasks.lock().await.remove(&job_id);
        });
        self.inner
            .tasks
            .lock()
            .await
            .insert(job_id, task.abort_handle());
        let _ = start.send(());
        Ok(())
    }

    pub async fn cancel(&self, job_id: JobId) -> bool {
        let Some(task) = self.inner.tasks.lock().await.remove(&job_id) else {
            return false;
        };
        task.abort();
        let provider = {
            let mut snapshots = self.inner.snapshots.lock().await;
            let Some(snapshot) = snapshots.get_mut(&job_id) else {
                return true;
            };
            snapshot.state = DownloadJobState::Cancelled;
            for transfer in snapshot.transfers.values_mut() {
                if !transfer.state.is_terminal() {
                    transfer.state = TransferState::Interrupted;
                }
            }
            snapshot.request.target.reference.identity.clone()
        };
        let event = DownloadEvent::Cancelled { job_id, provider };
        if let Some(state) = &self.inner.state
            && let Err(error) = state.record_event(&event)
        {
            *self.inner.persistence_error.lock().await = Some(format!("{error:#}"));
        }
        let _ = self.inner.events.send(event);
        true
    }

    pub async fn active_jobs(&self) -> Vec<JobId> {
        self.inner.tasks.lock().await.keys().copied().collect()
    }

    pub async fn snapshot(&self, job_id: JobId) -> Option<DownloadJobSnapshot> {
        self.inner.snapshots.lock().await.get(&job_id).cloned()
    }

    pub async fn snapshots(&self) -> Vec<DownloadJobSnapshot> {
        self.inner
            .snapshots
            .lock()
            .await
            .values()
            .cloned()
            .collect()
    }

    pub async fn persistence_error(&self) -> Option<String> {
        self.inner.persistence_error.lock().await.clone()
    }
}

fn ensure_provider_matches(
    provider: &dyn CloudProvider,
    request: &DownloadJobRequest,
) -> Result<()> {
    ensure!(
        provider.identity() == &request.target.reference.identity,
        "download Job Provider identity does not match its target"
    );
    Ok(())
}

async fn persist_progress(
    manager: &DownloadManagerInner,
    state: Option<&StateStore>,
    progress: &mut BTreeMap<(JobId, TransferId), TransferProgress>,
) {
    let Some(state) = state else {
        progress.clear();
        return;
    };
    let updates: Vec<_> = progress.values().cloned().collect();
    progress.clear();
    if let Err(error) = state.record_progress_batch(&updates) {
        *manager.persistence_error.lock().await = Some(format!("{error:#}"));
    }
}

fn terminal_transfer(event: &DownloadEvent) -> Option<(JobId, TransferId)> {
    match event {
        DownloadEvent::FileSkipped {
            job_id,
            transfer_id,
            ..
        }
        | DownloadEvent::FileCompleted {
            job_id,
            transfer_id,
            ..
        }
        | DownloadEvent::FileFailed {
            job_id,
            transfer_id,
            ..
        } => Some((*job_id, *transfer_id)),
        _ => None,
    }
}

async fn apply_snapshot(
    snapshots: &Mutex<BTreeMap<JobId, DownloadJobSnapshot>>,
    event: &DownloadEvent,
) {
    let mut snapshots = snapshots.lock().await;
    let job_id = event_job_id(event);
    let Some(snapshot) = snapshots.get_mut(&job_id) else {
        return;
    };
    match event {
        DownloadEvent::Scanning { .. } => snapshot.state = DownloadJobState::Scanning,
        DownloadEvent::PlanReady { .. } => snapshot.state = DownloadJobState::Running,
        DownloadEvent::TransferQueued {
            transfer_id,
            path,
            size,
            ..
        } => {
            snapshot
                .transfers
                .entry(*transfer_id)
                .or_insert(TransferSnapshot {
                    id: *transfer_id,
                    path: path.clone(),
                    state: TransferState::Queued,
                    size: *size,
                    written: 0,
                    bytes_per_second: 0.0,
                    error: None,
                });
        }
        DownloadEvent::FileStarted {
            transfer_id,
            path,
            size,
            ..
        } => update_transfer(
            snapshot,
            SnapshotTransferUpdate {
                id: *transfer_id,
                path,
                state: TransferState::Running,
                size: *size,
                written: 0,
                bytes_per_second: 0.0,
                error: None,
            },
        ),
        DownloadEvent::Progress {
            transfer_id,
            path,
            file_written,
            file_total,
            bytes_per_second,
            ..
        } => update_transfer(
            snapshot,
            SnapshotTransferUpdate {
                id: *transfer_id,
                path,
                state: TransferState::Running,
                size: *file_total,
                written: *file_written,
                bytes_per_second: *bytes_per_second,
                error: None,
            },
        ),
        DownloadEvent::FileSkipped {
            transfer_id,
            path,
            reason,
            ..
        } => update_transfer(
            snapshot,
            SnapshotTransferUpdate {
                id: *transfer_id,
                path,
                state: TransferState::Skipped,
                size: None,
                written: 0,
                bytes_per_second: 0.0,
                error: Some(reason.clone()),
            },
        ),
        DownloadEvent::FileCompleted {
            transfer_id,
            path,
            bytes,
            ..
        } => update_transfer(
            snapshot,
            SnapshotTransferUpdate {
                id: *transfer_id,
                path,
                state: TransferState::Completed,
                size: Some(*bytes),
                written: *bytes,
                bytes_per_second: 0.0,
                error: None,
            },
        ),
        DownloadEvent::FileFailed {
            transfer_id,
            path,
            bytes,
            error,
            ..
        } => update_transfer(
            snapshot,
            SnapshotTransferUpdate {
                id: *transfer_id,
                path,
                state: TransferState::Failed,
                size: None,
                written: *bytes,
                bytes_per_second: 0.0,
                error: Some(error.clone()),
            },
        ),
        DownloadEvent::Finished { summary, .. } => {
            snapshot.state = DownloadJobState::Finished;
            snapshot.summary = Some(summary.clone());
        }
        DownloadEvent::Fatal { error, .. } => {
            snapshot.state = DownloadJobState::Failed;
            snapshot.error = Some(error.clone());
        }
        DownloadEvent::Cancelled { .. } => snapshot.state = DownloadJobState::Cancelled,
    }
}

struct SnapshotTransferUpdate<'a> {
    id: TransferId,
    path: &'a std::path::Path,
    state: TransferState,
    size: Option<u64>,
    written: u64,
    bytes_per_second: f64,
    error: Option<String>,
}

fn update_transfer(snapshot: &mut DownloadJobSnapshot, update: SnapshotTransferUpdate<'_>) {
    let transfer = snapshot
        .transfers
        .entry(update.id)
        .or_insert(TransferSnapshot {
            id: update.id,
            path: update.path.to_path_buf(),
            state: update.state,
            size: update.size,
            written: update.written,
            bytes_per_second: update.bytes_per_second,
            error: update.error.clone(),
        });
    if transfer.state.is_terminal() {
        return;
    }
    transfer.path = update.path.to_path_buf();
    transfer.state = update.state;
    transfer.size = update.size.or(transfer.size);
    transfer.written = update.written;
    transfer.bytes_per_second = update.bytes_per_second;
    transfer.error = update.error;
}

fn event_job_id(event: &DownloadEvent) -> JobId {
    match event {
        DownloadEvent::Scanning { job_id, .. }
        | DownloadEvent::PlanReady { job_id, .. }
        | DownloadEvent::TransferQueued { job_id, .. }
        | DownloadEvent::FileStarted { job_id, .. }
        | DownloadEvent::Progress { job_id, .. }
        | DownloadEvent::FileSkipped { job_id, .. }
        | DownloadEvent::FileCompleted { job_id, .. }
        | DownloadEvent::FileFailed { job_id, .. }
        | DownloadEvent::Finished { job_id, .. }
        | DownloadEvent::Fatal { job_id, .. }
        | DownloadEvent::Cancelled { job_id, .. } => *job_id,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::provider::{
        DownloadCapability, ProfileId, ProviderCapabilities, ProviderId, ProviderIdentity,
        ProviderResult, RemoteDownload, RemoteItemKind, RemoteObjectId, RemoteRef, RemoteSource,
        SourceId,
    };

    struct SlowProvider {
        identity: ProviderIdentity,
    }

    impl SlowProvider {
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
                name: "hello.txt".to_owned(),
                kind: RemoteItemKind::File,
                size: Some(5),
                checksum: None,
                download: DownloadCapability::Supported { resume: true },
            }
        }
    }

    #[async_trait]
    impl CloudProvider for SlowProvider {
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
            if offset >= 5 {
                return Ok(RemoteDownload::AlreadyComplete { expected_size: 5 });
            }
            let remaining = b"hello"[offset as usize..].to_vec();
            let body = stream::once(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(Bytes::from(remaining))
            })
            .boxed();
            if offset == 0 {
                Ok(RemoteDownload::Full {
                    stream: body,
                    total_size: Some(5),
                })
            } else {
                Ok(RemoteDownload::Partial {
                    start: offset,
                    stream: body,
                    total_size: Some(5),
                })
            }
        }
    }

    #[tokio::test]
    async fn job_continues_when_a_subscriber_disconnects() {
        let manager = DownloadManager::new();
        let provider = Arc::new(SlowProvider::new());
        let target = provider.file();
        let temporary = tempfile::tempdir().unwrap();
        let first = manager.subscribe();
        let job_id = manager
            .submit(
                provider,
                target,
                temporary.path().to_path_buf(),
                DownloadOptions {
                    overwrite: false,
                    resume: true,
                    concurrency: 1,
                },
            )
            .await;
        let job_id = job_id.unwrap();
        drop(first);
        let mut reconnected = manager.subscribe();
        let finished = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let DownloadEvent::Finished {
                    job_id: finished_id,
                    ..
                } = reconnected.recv().await.unwrap()
                {
                    break finished_id;
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(finished, job_id);
        assert_eq!(
            tokio::fs::read(temporary.path().join(PathBuf::from("hello.txt")))
                .await
                .unwrap(),
            b"hello"
        );
        assert!(manager.active_jobs().await.is_empty());
        let snapshot = manager.snapshot(job_id).await.unwrap();
        assert_eq!(snapshot.state, DownloadJobState::Finished);
        assert_eq!(snapshot.transfers.len(), 1);
        assert_eq!(
            snapshot.transfers[&TransferId(1)].state,
            TransferState::Completed
        );
    }

    #[tokio::test]
    async fn cancel_removes_an_active_job() {
        let manager = DownloadManager::new();
        let provider = Arc::new(SlowProvider::new());
        let target = provider.file();
        let temporary = tempfile::tempdir().unwrap();
        let job_id = manager
            .submit(
                provider,
                target,
                temporary.path().to_path_buf(),
                DownloadOptions {
                    overwrite: false,
                    resume: true,
                    concurrency: 1,
                },
            )
            .await;
        let job_id = job_id.unwrap();

        assert!(manager.cancel(job_id).await);
        assert!(!manager.cancel(job_id).await);
        assert!(manager.active_jobs().await.is_empty());
        assert_eq!(
            manager.snapshot(job_id).await.unwrap().state,
            DownloadJobState::Cancelled
        );
    }

    #[tokio::test]
    async fn runs_multiple_jobs_and_keeps_stable_snapshots() {
        let manager = DownloadManager::new();
        let provider = Arc::new(SlowProvider::new());
        let temporary = tempfile::tempdir().unwrap();
        let mut receiver = manager.subscribe();
        let first = manager
            .submit(
                provider.clone(),
                provider.file(),
                temporary.path().join("first"),
                DownloadOptions {
                    overwrite: false,
                    resume: true,
                    concurrency: 1,
                },
            )
            .await
            .unwrap();
        let second = manager
            .submit(
                provider,
                SlowProvider::new().file(),
                temporary.path().join("second"),
                DownloadOptions {
                    overwrite: false,
                    resume: true,
                    concurrency: 1,
                },
            )
            .await
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(manager.active_jobs().await.len(), 2);

        let mut finished = std::collections::BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while finished.len() < 2 {
                if let DownloadEvent::Finished { job_id, .. } = receiver.recv().await.unwrap() {
                    finished.insert(job_id);
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(manager.snapshots().await.len(), 2);
        assert!(
            manager
                .snapshots()
                .await
                .iter()
                .all(|snapshot| snapshot.state == DownloadJobState::Finished)
        );
    }

    #[tokio::test]
    async fn persistent_manager_continues_job_ids_and_resumes_interrupted_request() {
        let temporary = tempfile::tempdir().unwrap();
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        let provider = Arc::new(SlowProvider::new());
        let request = DownloadJobRequest::new(
            provider.file(),
            temporary.path().join("recovered"),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
        );
        state.record_submission(JobId(12), &request).unwrap();
        state
            .record_event(&DownloadEvent::Scanning {
                job_id: JobId(12),
                provider: provider.identity.clone(),
            })
            .unwrap();
        state.recover_interrupted().unwrap();

        let manager = DownloadManager::persistent(state.clone()).unwrap();
        let mut receiver = manager.subscribe();
        manager
            .resume(JobId(12), provider.clone(), request)
            .await
            .unwrap();
        let next = manager
            .submit(
                provider.clone(),
                provider.file(),
                temporary.path().join("new"),
                DownloadOptions {
                    overwrite: false,
                    resume: true,
                    concurrency: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(next, JobId(13));

        let mut finished = std::collections::BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while finished.len() < 2 {
                if let DownloadEvent::Finished { job_id, .. } = receiver.recv().await.unwrap() {
                    finished.insert(job_id);
                }
            }
        })
        .await
        .unwrap();
        let snapshots = state.load_snapshots().unwrap();
        assert!(
            snapshots
                .iter()
                .all(|snapshot| snapshot.state == DownloadJobState::Finished)
        );
    }
}
