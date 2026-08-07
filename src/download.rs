use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::mpsc::Sender,
};

use crate::{
    plan::{DownloadPlan, PlannedFile},
    provider::{CloudProvider, ProviderIdentity, RemoteDownload, RemoteItem},
};

const STREAM_RETRY_LIMIT: usize = 4;
const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct DownloadOptions {
    pub overwrite: bool,
    pub resume: bool,
    pub concurrency: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TransferId(pub usize);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DownloadJobState {
    Queued,
    Scanning,
    Running,
    Paused,
    Finished,
    Failed,
    Cancelled,
    Interrupted,
}

impl DownloadJobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TransferState {
    Queued,
    Running,
    Paused,
    Completed,
    Skipped,
    Failed,
    Interrupted,
}

impl TransferState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Skipped | Self::Failed | Self::Interrupted
        )
    }
}

#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Scanning {
        job_id: JobId,
        provider: ProviderIdentity,
    },
    PlanReady {
        job_id: JobId,
        provider: ProviderIdentity,
        files: usize,
        items: usize,
        bytes: u64,
        pre_skipped: usize,
        workers: usize,
    },
    TransferQueued {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        path: PathBuf,
        size: Option<u64>,
    },
    FileStarted {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        index: usize,
        total: usize,
        path: PathBuf,
        size: Option<u64>,
    },
    Progress {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        index: usize,
        total: usize,
        path: PathBuf,
        file_written: u64,
        file_total: Option<u64>,
        bytes_per_second: f64,
    },
    Retrying {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        path: PathBuf,
        offset: u64,
        attempt: usize,
        max_attempts: usize,
        error: String,
    },
    Paused {
        job_id: JobId,
        provider: ProviderIdentity,
    },
    Resumed {
        job_id: JobId,
        provider: ProviderIdentity,
    },
    FileSkipped {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        path: PathBuf,
        reason: String,
    },
    FileCompleted {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        path: PathBuf,
        bytes: u64,
    },
    FileFailed {
        job_id: JobId,
        provider: ProviderIdentity,
        transfer_id: TransferId,
        path: PathBuf,
        bytes: u64,
        error: String,
    },
    Finished {
        job_id: JobId,
        provider: ProviderIdentity,
        summary: DownloadSummary,
    },
    Fatal {
        job_id: JobId,
        provider: ProviderIdentity,
        error: String,
    },
    Cancelled {
        job_id: JobId,
        provider: ProviderIdentity,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DownloadSummary {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub bytes_written: u64,
    pub partial_bytes: u64,
}

enum FileOutcome {
    Downloaded(u64),
    Skipped(String),
    Failed { bytes: u64, error: String },
}

#[derive(Clone, Copy)]
struct TransferContext<'a> {
    job_id: JobId,
    transfer_id: TransferId,
    index: usize,
    total: usize,
    events: &'a Sender<DownloadEvent>,
}

pub async fn run_download_job(
    job_id: JobId,
    client: Arc<dyn CloudProvider>,
    root: RemoteItem,
    destination: PathBuf,
    options: DownloadOptions,
    events: Sender<DownloadEvent>,
) {
    let provider = client.identity().clone();
    let _ = events
        .send(DownloadEvent::Scanning {
            job_id,
            provider: provider.clone(),
        })
        .await;
    let plan = match DownloadPlan::build(client.as_ref(), &root).await {
        Ok(plan) => plan,
        Err(error) => {
            let _ = events
                .send(DownloadEvent::Fatal {
                    job_id,
                    provider,
                    error: format!("download plan failed: {error:#}"),
                })
                .await;
            return;
        }
    };
    if let Err(error) = execute_plan(
        client.as_ref(),
        job_id,
        &provider,
        plan,
        &destination,
        options,
        &events,
    )
    .await
    {
        let _ = events
            .send(DownloadEvent::Fatal {
                job_id,
                provider,
                error: format!("download initialization failed: {error:#}"),
            })
            .await;
    }
}

async fn execute_plan(
    client: &dyn CloudProvider,
    job_id: JobId,
    provider: &ProviderIdentity,
    plan: DownloadPlan,
    destination: &Path,
    options: DownloadOptions,
    events: &Sender<DownloadEvent>,
) -> Result<()> {
    ensure_real_directory(destination).await?;
    for relative in &plan.directories {
        let path = destination.join(relative);
        ensure_real_directory(&path).await?;
    }

    let _ = events
        .send(DownloadEvent::PlanReady {
            job_id,
            provider: provider.clone(),
            files: plan.files.len(),
            items: plan.files.len().saturating_add(plan.skipped.len()),
            bytes: plan.total_bytes,
            pre_skipped: plan.skipped.len(),
            workers: options.concurrency.clamp(1, 5),
        })
        .await;

    let total_files = plan.files.len();
    let mut summary = DownloadSummary {
        skipped: plan.skipped.len(),
        ..DownloadSummary::default()
    };
    for (position, skipped) in plan.skipped.into_iter().enumerate() {
        let transfer_id = TransferId(total_files + position + 1);
        let _ = events
            .send(DownloadEvent::FileSkipped {
                job_id,
                provider: provider.clone(),
                transfer_id,
                path: skipped.relative_path,
                reason: skipped.reason,
            })
            .await;
    }

    let workers = options.concurrency.clamp(1, 5);
    for (position, file) in plan.files.iter().enumerate() {
        let _ = events
            .send(DownloadEvent::TransferQueued {
                job_id,
                provider: provider.clone(),
                transfer_id: TransferId(position + 1),
                path: file.relative_path.clone(),
                size: file.item.size,
            })
            .await;
    }
    let downloads = stream::iter(plan.files.into_iter().enumerate().map(|(position, file)| {
        let path = file.relative_path.clone();
        let target = destination.join(&path);
        async move {
            let index = position + 1;
            let transfer_id = TransferId(index);
            let _ = events
                .send(DownloadEvent::FileStarted {
                    job_id,
                    provider: provider.clone(),
                    transfer_id,
                    index,
                    total: total_files,
                    path: path.clone(),
                    size: file.item.size,
                })
                .await;
            let outcome = download_one(
                client,
                &file,
                &target,
                options,
                TransferContext {
                    job_id,
                    transfer_id,
                    index,
                    total: total_files,
                    events,
                },
            )
            .await;
            (transfer_id, path, outcome)
        }
    }))
    .buffer_unordered(workers);
    tokio::pin!(downloads);
    while let Some((transfer_id, path, outcome)) = downloads.next().await {
        match outcome {
            FileOutcome::Downloaded(bytes) => {
                summary.downloaded += 1;
                summary.bytes_written = summary.bytes_written.saturating_add(bytes);
                let _ = events
                    .send(DownloadEvent::FileCompleted {
                        job_id,
                        provider: provider.clone(),
                        transfer_id,
                        path,
                        bytes,
                    })
                    .await;
            }
            FileOutcome::Skipped(reason) => {
                summary.skipped += 1;
                let _ = events
                    .send(DownloadEvent::FileSkipped {
                        job_id,
                        provider: provider.clone(),
                        transfer_id,
                        path,
                        reason,
                    })
                    .await;
            }
            FileOutcome::Failed { bytes, error } => {
                summary.failed += 1;
                summary.partial_bytes = summary.partial_bytes.saturating_add(bytes);
                let _ = events
                    .send(DownloadEvent::FileFailed {
                        job_id,
                        provider: provider.clone(),
                        transfer_id,
                        path,
                        bytes,
                        error,
                    })
                    .await;
            }
        }
    }

    let _ = events
        .send(DownloadEvent::Finished {
            job_id,
            provider: provider.clone(),
            summary,
        })
        .await;
    Ok(())
}

async fn download_one(
    client: &dyn CloudProvider,
    file: &PlannedFile,
    target: &Path,
    options: DownloadOptions,
    context: TransferContext<'_>,
) -> FileOutcome {
    let partial = partial_path(target);
    match download_one_inner(client, file, target, &partial, options, context).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let bytes = actual_partial_bytes(&partial).await;
            FileOutcome::Failed {
                bytes,
                error: format!("{error:#}"),
            }
        }
    }
}

async fn download_one_inner(
    client: &dyn CloudProvider,
    file: &PlannedFile,
    target: &Path,
    partial: &Path,
    options: DownloadOptions,
    context: TransferContext<'_>,
) -> Result<FileOutcome> {
    if fs::try_exists(target).await? && !options.overwrite {
        return Ok(FileOutcome::Skipped(
            "target exists (use --overwrite to replace it)".to_owned(),
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("target file has no parent directory"))?;
    ensure_real_directory(parent).await?;
    if let Ok(metadata) = fs::symlink_metadata(partial).await {
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to write through partial-file symlink {}",
                partial.display()
            );
        }
        if !metadata.is_file() {
            bail!("partial path is not a regular file: {}", partial.display());
        }
    }

    let expected_size = file.item.size;
    let mut offset =
        if options.resume && file.item.supports_resume() && fs::try_exists(partial).await? {
            fs::metadata(partial).await?.len()
        } else {
            0
        };
    let opened = client.open_download(&file.item, offset).await?;
    let (append, total_size, mut remote_stream) = match opened {
        RemoteDownload::AlreadyComplete { expected_size } => {
            if expected_size == offset
                && file.item.size == Some(expected_size)
                && fs::try_exists(partial).await?
            {
                fs::rename(partial, target).await?;
                return Ok(FileOutcome::Downloaded(offset));
            }
            bail!("provider reported completion for a mismatched partial file");
        }
        RemoteDownload::Partial {
            start,
            stream,
            total_size,
        } => {
            if start != offset || offset == 0 {
                bail!("provider resume response does not match the local .part file");
            }
            (true, total_size, stream)
        }
        RemoteDownload::Full { stream, total_size } => {
            offset = 0;
            (false, total_size, stream)
        }
    };
    let expected_size = expected_size.or(total_size);

    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(partial)
        .await
        .with_context(|| format!("failed to open {} for writing", partial.display()))?;
    let mut written = offset;
    let mut progress_window_written = written;
    let mut progress_window_started = Instant::now();
    let mut retry_count = 0;
    loop {
        let wait = PROGRESS_EVENT_INTERVAL.saturating_sub(progress_window_started.elapsed());
        let chunk = match tokio::time::timeout(wait, remote_stream.next()).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                send_progress(
                    client,
                    file,
                    context,
                    written,
                    expected_size,
                    progress_window_written,
                    progress_window_started,
                )
                .await;
                progress_window_written = written;
                progress_window_started = Instant::now();
                continue;
            }
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error)
                if options.resume
                    && file.item.supports_resume()
                    && retry_count < STREAM_RETRY_LIMIT =>
            {
                retry_count += 1;
                let reason = error.to_string();
                output.flush().await.with_context(|| {
                    format!("failed to flush partial file: {}", partial.display())
                })?;
                let _ = context
                    .events
                    .send(DownloadEvent::Retrying {
                        job_id: context.job_id,
                        provider: client.identity().clone(),
                        transfer_id: context.transfer_id,
                        path: file.relative_path.clone(),
                        offset: written,
                        attempt: retry_count,
                        max_attempts: STREAM_RETRY_LIMIT,
                        error: reason.clone(),
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(250 * retry_count as u64))
                    .await;
                let reopened = client.open_download(&file.item, written).await.map_err(|open| {
                        anyhow!(
                            "download stream retry {retry_count}/{STREAM_RETRY_LIMIT} failed at byte offset {written}: original stream error: {reason}; reopen error: {open}"
                        )
                    })?;
                remote_stream = match reopened {
                    RemoteDownload::Partial { start, stream, .. } if start == written => stream,
                    RemoteDownload::AlreadyComplete { expected_size }
                        if expected_size == written && file.item.size == Some(expected_size) =>
                    {
                        break;
                    }
                    RemoteDownload::Partial { start, .. } => {
                        bail!(
                            "download stream retry {retry_count}/{STREAM_RETRY_LIMIT} returned byte offset {start}, expected {written}; original stream error: {reason}"
                        );
                    }
                    RemoteDownload::Full { .. } => {
                        bail!(
                            "download stream retry {retry_count}/{STREAM_RETRY_LIMIT} ignored Range at byte offset {written}; original stream error: {reason}"
                        );
                    }
                    RemoteDownload::AlreadyComplete { expected_size } => {
                        bail!(
                            "download stream retry {retry_count}/{STREAM_RETRY_LIMIT} reported size {expected_size} at byte offset {written}; original stream error: {reason}"
                        );
                    }
                };
                progress_window_written = written;
                progress_window_started = Instant::now();
                continue;
            }
            Err(error) => {
                bail!(
                    "provider download stream failed after {retry_count}/{STREAM_RETRY_LIMIT} retries at byte offset {written} (expected {}): {error}",
                    expected_size
                        .map(|size| size.to_string())
                        .unwrap_or_else(|| "unknown".to_owned())
                );
            }
        };
        output
            .write_all(&chunk)
            .await
            .with_context(|| format!("failed to write local file: {}", partial.display()))?;
        written = written.saturating_add(chunk.len() as u64);
        if progress_window_started.elapsed() >= PROGRESS_EVENT_INTERVAL
            || expected_size == Some(written)
        {
            send_progress(
                client,
                file,
                context,
                written,
                expected_size,
                progress_window_written,
                progress_window_started,
            )
            .await;
            progress_window_written = written;
            progress_window_started = Instant::now();
        }
    }
    if written != progress_window_written {
        send_progress(
            client,
            file,
            context,
            written,
            expected_size,
            progress_window_written,
            progress_window_started,
        )
        .await;
    }
    output.flush().await?;
    output.sync_all().await?;
    drop(output);

    if let Some(expected) = expected_size
        && written != expected
    {
        bail!(
            "size check failed: expected {expected} bytes, wrote {written}; retained {}",
            partial.display()
        );
    }
    fs::rename(partial, target)
        .await
        .with_context(|| format!("failed to finalize {} atomically", target.display()))?;
    Ok(FileOutcome::Downloaded(written))
}

async fn send_progress(
    client: &dyn CloudProvider,
    file: &PlannedFile,
    context: TransferContext<'_>,
    written: u64,
    expected_size: Option<u64>,
    window_written: u64,
    window_started: Instant,
) {
    let elapsed = window_started.elapsed().as_secs_f64().max(0.001);
    let bytes_per_second = written.saturating_sub(window_written) as f64 / elapsed;
    let _ = context
        .events
        .send(DownloadEvent::Progress {
            job_id: context.job_id,
            provider: client.identity().clone(),
            transfer_id: context.transfer_id,
            index: context.index,
            total: context.total,
            path: file.relative_path.clone(),
            file_written: written,
            file_total: expected_size,
            bytes_per_second,
        })
        .await;
}

fn partial_path(target: &Path) -> PathBuf {
    let mut partial_name = OsString::from(target.as_os_str());
    partial_name.push(".part");
    PathBuf::from(partial_name)
}

#[async_recursion::async_recursion]
async fn ensure_real_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("path is not a real directory: {}", path.display());
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect directory {}", path.display()));
        }
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_real_directory(parent).await?;
    }
    match fs::create_dir(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to create directory {}", path.display()));
        }
    }
    let metadata = fs::symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("path is not a real directory: {}", path.display());
    }
    Ok(())
}

async fn actual_partial_bytes(partial: &Path) -> u64 {
    match fs::symlink_metadata(partial).await {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => 0,
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use reqwest::{StatusCode, header::CONTENT_RANGE};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
    };

    use super::*;
    use crate::provider::{
        DownloadCapability, ProfileId, ProviderCapabilities, ProviderError, ProviderErrorKind,
        ProviderId, ProviderIdentity, ProviderResult, RemoteItemKind, RemoteObjectId, RemoteRef,
        RemoteSource, SourceId,
    };

    struct HttpProvider {
        identity: ProviderIdentity,
        http: reqwest::Client,
        base: String,
    }

    impl HttpProvider {
        fn new(base: String) -> Self {
            Self {
                identity: ProviderIdentity::new(
                    ProviderId::new("test-http").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                base,
            }
        }

        fn item(&self, id: impl Into<String>, name: impl Into<String>, size: u64) -> RemoteItem {
            RemoteItem {
                reference: RemoteRef {
                    identity: self.identity.clone(),
                    source_id: SourceId::new("source").unwrap(),
                    object_id: RemoteObjectId::new(id.into()).unwrap(),
                },
                name: name.into(),
                kind: RemoteItemKind::File,
                size: Some(size),
                checksum: None,
                download: DownloadCapability::Supported { resume: true },
            }
        }
    }

    #[async_trait]
    impl CloudProvider for HttpProvider {
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
            Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                "not used in download tests",
            ))
        }

        async fn open_download(
            &self,
            item: &RemoteItem,
            offset: u64,
        ) -> ProviderResult<RemoteDownload> {
            let mut request = self
                .http
                .get(format!("{}/{}", self.base, item.reference.object_id));
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let response = request.send().await.map_err(|error| {
                ProviderError::new(ProviderErrorKind::Network, error.to_string())
            })?;
            let status = response.status();
            if status == StatusCode::RANGE_NOT_SATISFIABLE && item.size == Some(offset) {
                return Ok(RemoteDownload::AlreadyComplete {
                    expected_size: offset,
                });
            }
            if !status.is_success() {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    format!("test server returned {status}"),
                ));
            }
            if status == StatusCode::PARTIAL_CONTENT && offset > 0 {
                let valid = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with(&format!("bytes {offset}-")));
                if !valid {
                    return Err(ProviderError::new(
                        ProviderErrorKind::Network,
                        "invalid Content-Range",
                    ));
                }
            }
            let stream = response
                .bytes_stream()
                .map(|chunk| {
                    chunk.map_err(|error| {
                        ProviderError::new(ProviderErrorKind::Network, error.to_string())
                    })
                })
                .boxed();
            if status == StatusCode::PARTIAL_CONTENT && offset > 0 {
                Ok(RemoteDownload::Partial {
                    start: offset,
                    stream,
                    total_size: item.size,
                })
            } else {
                Ok(RemoteDownload::Full {
                    stream,
                    total_size: item.size,
                })
            }
        }
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[tokio::test]
    async fn resumes_partial_file_from_local_http_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("Range: bytes=5-") || request.contains("range: bytes=5-"));
            stream
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 6\r\nContent-Range: bytes 5-10/11\r\nConnection: close\r\n\r\n world",
                )
                .await
                .unwrap();
        });

        let client = HttpProvider::new(format!("http://{address}/drive/v3/files"));
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("root/nested/hello.txt");
        fs::create_dir_all(target.parent().unwrap()).await.unwrap();
        fs::write(
            directory.path().join("root/nested/hello.txt.part"),
            b"hello",
        )
        .await
        .unwrap();
        let item = client.item("file-123456789", "hello.txt", 11);
        let planned = PlannedFile {
            item,
            relative_path: PathBuf::from("root/nested/hello.txt"),
        };
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root"), PathBuf::from("root/nested")],
            files: vec![planned],
            skipped: Vec::new(),
            total_bytes: 11,
        };
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(1),
            &client.identity,
            plan,
            directory.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"hello world");
        assert!(
            !fs::try_exists(directory.path().join("root/nested/hello.txt.part"))
                .await
                .unwrap()
        );
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::PlanReady {
                files: 1,
                bytes: 11,
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DownloadEvent::FileCompleted { bytes: 11, .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::Finished {
                summary: DownloadSummary {
                    downloaded: 1,
                    failed: 0,
                    ..
                },
                ..
            }
        )));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn skips_an_existing_local_file_without_opening_the_provider_download() {
        let client = HttpProvider::new("http://127.0.0.1:1/drive/v3/files".to_owned());
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("root/existing.bin");
        fs::create_dir_all(target.parent().unwrap()).await.unwrap();
        fs::write(&target, b"keep-local-copy").await.unwrap();
        let item = client.item("existing-file-123456789", "existing.bin", 1024);
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root")],
            files: vec![PlannedFile {
                item,
                relative_path: PathBuf::from("root/existing.bin"),
            }],
            skipped: Vec::new(),
            total_bytes: 1024,
        };
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(4),
            &client.identity,
            plan,
            directory.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap();

        assert_eq!(fs::read(&target).await.unwrap(), b"keep-local-copy");
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::FileSkipped { reason, .. } if reason.starts_with("target exists")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::Finished {
                summary: DownloadSummary {
                    downloaded: 0,
                    skipped: 1,
                    failed: 0,
                    ..
                },
                ..
            }
        )));
    }

    #[tokio::test]
    async fn progress_speed_uses_the_same_window_and_drops_to_zero_when_stalled() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabcd")
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(650)).await;
            stream.write_all(b"efgh").await.unwrap();
        });

        let client = HttpProvider::new(format!("http://{address}/drive/v3/files"));
        let directory = tempfile::tempdir().unwrap();
        let item = client.item("stalled-file-123456789", "stalled.bin", 8);
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root")],
            files: vec![PlannedFile {
                item,
                relative_path: PathBuf::from("root/stalled.bin"),
            }],
            skipped: Vec::new(),
            total_bytes: 8,
        };
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(3),
            &client.identity,
            plan,
            directory.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap();

        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        let progress: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                DownloadEvent::Progress {
                    file_written,
                    bytes_per_second,
                    ..
                } => Some((*file_written, *bytes_per_second)),
                _ => None,
            })
            .collect();
        assert!(
            progress
                .iter()
                .any(|(written, speed)| *written == 4 && *speed > 0.0),
            "expected a positive speed sample for the first four bytes: {progress:?}"
        );
        assert!(
            progress
                .iter()
                .any(|(written, speed)| *written == 4 && *speed == 0.0),
            "expected the stalled transfer speed to decay to zero: {progress:?}"
        );
        assert_eq!(
            fs::read(directory.path().join("root/stalled.bin"))
                .await
                .unwrap(),
            b"abcdefgh"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retries_a_broken_response_body_from_the_written_offset() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let _ = first.read(&mut request).await.unwrap();
            first
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello",
                )
                .await
                .unwrap();
            drop(first);

            let (mut second, _) = listener.accept().await.unwrap();
            let count = second.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("Range: bytes=5-") || request.contains("range: bytes=5-"));
            second
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 6\r\nContent-Range: bytes 5-10/11\r\nConnection: close\r\n\r\n world",
                )
                .await
                .unwrap();
        });

        let client = HttpProvider::new(format!("http://{address}/drive/v3/files"));
        let directory = tempfile::tempdir().unwrap();
        let item = client.item("flaky-file-123456789", "flaky.bin", 11);
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root")],
            files: vec![PlannedFile {
                item,
                relative_path: PathBuf::from("root/flaky.bin"),
            }],
            skipped: Vec::new(),
            total_bytes: 11,
        };
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(2),
            &client.identity,
            plan,
            directory.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap();

        assert_eq!(
            fs::read(directory.path().join("root/flaky.bin"))
                .await
                .unwrap(),
            b"hello world"
        );
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::Retrying {
                offset: 5,
                attempt: 1,
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DownloadEvent::FileCompleted { bytes: 11, .. }))
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_stream_counts_only_actual_partial_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
        });

        let client = HttpProvider::new(format!("http://{address}/drive/v3/files"));
        let directory = tempfile::tempdir().unwrap();
        let item = client.item("broken-file-123456789", "broken.bin", 100);
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root")],
            files: vec![PlannedFile {
                item,
                relative_path: PathBuf::from("root/broken.bin"),
            }],
            skipped: Vec::new(),
            total_bytes: 100,
        };
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(1),
            &client.identity,
            plan,
            directory.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap();
        let partial = directory.path().join("root/broken.bin.part");
        assert_eq!(fs::metadata(partial).await.unwrap().len(), 5);
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DownloadEvent::FileFailed { bytes: 5, .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            DownloadEvent::Finished {
                summary: DownloadSummary {
                    bytes_written: 0,
                    partial_bytes: 5,
                    failed: 1,
                    ..
                },
                ..
            }
        )));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn limits_internal_file_queue_to_requested_workers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let server_active = active.clone();
        let server_maximum = maximum.clone();
        let server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let active = server_active.clone();
                let maximum = server_maximum.clone();
                handlers.push(tokio::spawn(async move {
                    let mut request = vec![0_u8; 4096];
                    let _ = socket.read(&mut request).await.unwrap();
                    let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now_active, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
                        )
                        .await
                        .unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for handler in handlers {
                handler.await.unwrap();
            }
        });

        let client = HttpProvider::new(format!("http://{address}/drive/v3/files"));
        let files = (0..4)
            .map(|index| PlannedFile {
                item: client.item(format!("file-{index}-123456789"), format!("{index}.bin"), 1),
                relative_path: PathBuf::from(format!("root/{index}.bin")),
            })
            .collect();
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root")],
            files,
            skipped: Vec::new(),
            total_bytes: 4,
        };
        let temporary = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = mpsc::channel(128);
        execute_plan(
            &client,
            JobId(1),
            &client.identity,
            plan,
            temporary.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 2,
            },
            &sender,
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DownloadEvent::PlanReady { workers: 2, .. }))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, DownloadEvent::FileCompleted { .. }))
                .count(),
            4
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_preexisting_symlinked_directory_ancestors() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).await.unwrap();
        fs::create_dir(&outside).await.unwrap();
        std::os::unix::fs::symlink(&outside, root.join("nested")).unwrap();

        let client = HttpProvider::new("http://127.0.0.1:1".to_owned());
        let plan = DownloadPlan {
            root_name: "root".to_owned(),
            directories: vec![PathBuf::from("root"), PathBuf::from("root/nested")],
            files: Vec::new(),
            skipped: Vec::new(),
            total_bytes: 0,
        };
        let (sender, _receiver) = mpsc::channel(8);
        let error = execute_plan(
            &client,
            JobId(1),
            &client.identity,
            plan,
            temporary.path(),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
            &sender,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("real directory"));
    }
}
