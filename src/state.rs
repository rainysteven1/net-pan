use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use crate::{
    download::{
        DownloadEvent, DownloadJobState, DownloadSummary, JobId, TransferId, TransferState,
    },
    job::{DownloadJobRequest, DownloadJobSnapshot, TransferSnapshot},
    notifier::NotificationEvent,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 3;

#[derive(Clone)]
pub struct StateStore {
    connection: Arc<Mutex<Connection>>,
    notification_targets: Arc<RwLock<Vec<String>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredJob {
    pub id: JobId,
    pub state: String,
    pub provider_id: String,
    pub profile_id: String,
    pub destination: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferProgress {
    pub job_id: JobId,
    pub transfer_id: crate::download::TransferId,
    pub written: u64,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub id: i64,
    pub notifier_id: String,
    pub event: NotificationEvent,
    pub attempts: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionPersistence {
    Created,
    Existing(JobId),
}

struct TransferUpdate<'a> {
    job_id: JobId,
    transfer_id: usize,
    path: &'a Path,
    state: &'a str,
    size: Option<u64>,
    written: u64,
    error: Option<&'a str>,
    now: i64,
}

impl StateStore {
    pub fn open(path: &Path) -> Result<Self> {
        let parent = path.parent().context("state database path has no parent")?;
        create_private_dir(parent)?;
        prepare_private_database(path)?;
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open state database: {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&connection)?;
        restrict_file(path)?;
        restrict_sqlite_sidecars(path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            notification_targets: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub fn set_notification_targets(&self, targets: Vec<String>) {
        *self
            .notification_targets
            .write()
            .expect("notification target lock poisoned") = targets;
    }

    pub fn record_submission(&self, job_id: JobId, request: &DownloadJobRequest) -> Result<()> {
        let request_json = serde_json::to_string(request)?;
        let now = Utc::now().timestamp_millis();
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        insert_job(&connection, job_id, request, &request_json, now)?;
        Ok(())
    }

    pub fn record_idempotent_submission(
        &self,
        client_id: &str,
        request_id: u64,
        job_id: JobId,
        request: &DownloadJobRequest,
    ) -> Result<SubmissionPersistence> {
        validate_client_id(client_id)?;
        let request_id = request_id.to_string();
        let request_json = serde_json::to_string(request)?;
        let now = Utc::now().timestamp_millis();
        let mut connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT job_id, request_json FROM ipc_submissions
                 WHERE client_id=?1 AND request_id=?2",
                params![client_id, request_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((existing_job_id, existing_request)) = existing {
            if existing_request != request_json {
                bail!("IPC idempotency key was reused with a different Job request");
            }
            return Ok(SubmissionPersistence::Existing(JobId(
                u64::try_from(existing_job_id).context("stored job ID is negative")?,
            )));
        }
        insert_job(&transaction, job_id, request, &request_json, now)?;
        transaction.execute(
            "INSERT INTO ipc_submissions (
               client_id, request_id, job_id, request_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                client_id,
                request_id,
                sqlite_u64(job_id.0, "job ID")?,
                request_json,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(SubmissionPersistence::Created)
    }

    pub fn record_event(&self, event: &DownloadEvent) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        let notification = notification_from_download_event(event);
        let targets = self
            .notification_targets
            .read()
            .expect("notification target lock poisoned")
            .clone();
        if let Some(notification) = notification
            && !targets.is_empty()
        {
            let mut connection = self
                .connection
                .lock()
                .expect("state database lock poisoned");
            let transaction = connection.transaction()?;
            match event {
                DownloadEvent::Finished {
                    job_id, summary, ..
                } => update_job_state(
                    &transaction,
                    *job_id,
                    "finished",
                    now,
                    Some(&serde_json::to_string(summary)?),
                    None,
                )?,
                DownloadEvent::Fatal { job_id, error, .. } => {
                    update_job_state(&transaction, *job_id, "failed", now, None, Some(error))?
                }
                _ => unreachable!("only terminal events produce notifications"),
            }
            enqueue_notification(&transaction, &targets, &notification, now)?;
            transaction.commit()?;
            return Ok(());
        }
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        match event {
            DownloadEvent::Scanning { job_id, .. } => {
                update_job_state(&connection, *job_id, "scanning", now, None, None)?;
            }
            DownloadEvent::PlanReady {
                job_id,
                files,
                items,
                bytes,
                workers,
                ..
            } => {
                let job_id = sqlite_u64(job_id.0, "job ID")?;
                let files = sqlite_usize(*files, "file count")?;
                let items = sqlite_usize(*items, "item count")?;
                let bytes = sqlite_u64(*bytes, "known byte count")?;
                let workers = sqlite_usize(*workers, "worker count")?;
                connection.execute(
                    "UPDATE jobs SET state='running', files=?2, items=?3, known_bytes=?4,
                     workers=?5, updated_at_ms=?6 WHERE id=?1",
                    params![job_id, files, items, bytes, workers, now],
                )?;
            }
            DownloadEvent::TransferQueued {
                job_id,
                transfer_id,
                path,
                size,
                ..
            } => {
                let job_id = sqlite_u64(job_id.0, "job ID")?;
                let transfer_id = sqlite_usize(transfer_id.0, "transfer ID")?;
                let size = sqlite_optional_u64(*size, "transfer size")?;
                connection.execute(
                    "INSERT INTO transfers (job_id, transfer_id, path, state, size, written, updated_at_ms)
                     VALUES (?1, ?2, ?3, 'queued', ?4, 0, ?5)
                     ON CONFLICT(job_id, transfer_id) DO UPDATE SET
                       path=excluded.path, state='queued', size=excluded.size, updated_at_ms=excluded.updated_at_ms",
                    params![job_id, transfer_id, path.to_string_lossy(), size, now],
                )?;
            }
            DownloadEvent::FileStarted {
                job_id,
                transfer_id,
                path,
                size,
                ..
            } => {
                upsert_transfer(
                    &connection,
                    TransferUpdate {
                        job_id: *job_id,
                        transfer_id: transfer_id.0,
                        path,
                        state: "running",
                        size: *size,
                        written: 0,
                        error: None,
                        now,
                    },
                )?;
            }
            DownloadEvent::Progress { .. } => {}
            DownloadEvent::FileSkipped {
                job_id,
                transfer_id,
                path,
                reason,
                ..
            } => {
                upsert_transfer(
                    &connection,
                    TransferUpdate {
                        job_id: *job_id,
                        transfer_id: transfer_id.0,
                        path,
                        state: "skipped",
                        size: None,
                        written: 0,
                        error: Some(reason),
                        now,
                    },
                )?;
            }
            DownloadEvent::FileCompleted {
                job_id,
                transfer_id,
                path,
                bytes,
                ..
            } => {
                upsert_transfer(
                    &connection,
                    TransferUpdate {
                        job_id: *job_id,
                        transfer_id: transfer_id.0,
                        path,
                        state: "completed",
                        size: Some(*bytes),
                        written: *bytes,
                        error: None,
                        now,
                    },
                )?;
            }
            DownloadEvent::FileFailed {
                job_id,
                transfer_id,
                path,
                bytes,
                error,
                ..
            } => {
                upsert_transfer(
                    &connection,
                    TransferUpdate {
                        job_id: *job_id,
                        transfer_id: transfer_id.0,
                        path,
                        state: "failed",
                        size: None,
                        written: *bytes,
                        error: Some(error),
                        now,
                    },
                )?;
            }
            DownloadEvent::Finished {
                job_id, summary, ..
            } => {
                update_job_state(
                    &connection,
                    *job_id,
                    "finished",
                    now,
                    Some(&serde_json::to_string(summary)?),
                    None,
                )?;
            }
            DownloadEvent::Fatal { job_id, error, .. } => {
                update_job_state(&connection, *job_id, "failed", now, None, Some(error))?;
            }
            DownloadEvent::Cancelled { job_id, .. } => {
                update_job_state(&connection, *job_id, "cancelled", now, None, None)?;
                let job_id = sqlite_u64(job_id.0, "job ID")?;
                connection.execute(
                    "UPDATE transfers SET state='interrupted', updated_at_ms=?2
                     WHERE job_id=?1 AND state IN ('queued', 'running')",
                    params![job_id, now],
                )?;
            }
        }
        Ok(())
    }

    pub fn record_progress_batch(&self, updates: &[TransferProgress]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        let mut connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let transaction = connection.transaction()?;
        {
            let mut statement = transaction.prepare(
                "UPDATE transfers SET written=?3, size=COALESCE(?4, size), updated_at_ms=?5
                 WHERE job_id=?1 AND transfer_id=?2
                   AND state NOT IN ('completed', 'skipped', 'failed', 'interrupted')",
            )?;
            for update in updates {
                statement.execute(params![
                    sqlite_u64(update.job_id.0, "job ID")?,
                    sqlite_usize(update.transfer_id.0, "transfer ID")?,
                    sqlite_u64(update.written, "written byte count")?,
                    sqlite_optional_u64(update.size, "transfer size")?,
                    now,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn recover_interrupted(&self) -> Result<usize> {
        let now = Utc::now().timestamp_millis();
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let changed = connection.execute(
            "UPDATE jobs SET state='interrupted', updated_at_ms=?1
             WHERE state IN ('queued', 'scanning', 'running')",
            params![now],
        )?;
        connection.execute(
            "UPDATE transfers SET state='interrupted', updated_at_ms=?1
             WHERE state IN ('queued', 'running')",
            params![now],
        )?;
        Ok(changed)
    }

    pub fn load_job(&self, job_id: JobId) -> Result<Option<StoredJob>> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let job_id_param = sqlite_u64(job_id.0, "job ID")?;
        let stored = connection
            .query_row(
                "SELECT id, state, provider_id, profile_id, destination FROM jobs WHERE id=?1",
                params![job_id_param],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(anyhow::Error::from)?;
        stored
            .map(|(id, state, provider_id, profile_id, destination)| {
                Ok(StoredJob {
                    id: JobId(u64::try_from(id).context("stored job ID is negative")?),
                    state,
                    provider_id,
                    profile_id,
                    destination: PathBuf::from(destination),
                })
            })
            .transpose()
    }

    pub fn load_interrupted_requests(&self) -> Result<Vec<(JobId, DownloadJobRequest)>> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let mut statement = connection
            .prepare("SELECT id, request_json FROM jobs WHERE state='interrupted' ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut requests = Vec::new();
        for row in rows {
            let (id, request_json) = row?;
            let id = JobId(u64::try_from(id).context("stored job ID is negative")?);
            let request = parse_request(id, &request_json)?;
            requests.push((id, request));
        }
        Ok(requests)
    }

    pub fn load_snapshots(&self) -> Result<Vec<DownloadJobSnapshot>> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let jobs = {
            let mut statement = connection.prepare(
                "SELECT id, state, request_json, summary_json, error FROM jobs ORDER BY id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut snapshots = Vec::with_capacity(jobs.len());
        for (id, state, request_json, summary_json, error) in jobs {
            let id = JobId(u64::try_from(id).context("stored job ID is negative")?);
            let request = parse_request(id, &request_json)?;
            let summary = summary_json
                .map(|summary| serde_json::from_str::<DownloadSummary>(&summary))
                .transpose()
                .with_context(|| format!("job {} has an invalid persisted summary", id.0))?;
            let mut transfers = BTreeMap::new();
            let mut statement = connection.prepare(
                "SELECT transfer_id, path, state, size, written, error
                 FROM transfers WHERE job_id=?1 ORDER BY transfer_id",
            )?;
            let rows = statement.query_map(params![sqlite_u64(id.0, "job ID")?], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?;
            for row in rows {
                let (transfer_id, path, state, size, written, transfer_error) = row?;
                let transfer_id = TransferId(
                    usize::try_from(transfer_id).context("stored transfer ID is invalid")?,
                );
                transfers.insert(
                    transfer_id,
                    TransferSnapshot {
                        id: transfer_id,
                        path: PathBuf::from(path),
                        state: parse_transfer_state(&state)?,
                        size: size
                            .map(|value| {
                                u64::try_from(value).context("stored transfer size is negative")
                            })
                            .transpose()?,
                        written: u64::try_from(written)
                            .context("stored written byte count is negative")?,
                        bytes_per_second: 0.0,
                        error: transfer_error,
                    },
                );
            }
            snapshots.push(DownloadJobSnapshot {
                id,
                request,
                state: parse_job_state(&state)?,
                transfers,
                summary,
                error,
            });
        }
        Ok(snapshots)
    }

    pub fn mark_requeued(&self, job_id: JobId) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        connection.execute(
            "UPDATE jobs SET state='queued', error=NULL, updated_at_ms=?2 WHERE id=?1",
            params![
                sqlite_u64(job_id.0, "job ID")?,
                Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }

    pub fn load_due_notifications(&self, now_ms: i64, limit: usize) -> Result<Vec<OutboxItem>> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let mut statement = connection.prepare(
            "SELECT id, notifier_id, event_json, attempts FROM notification_outbox
             WHERE state='pending' AND next_attempt_at_ms<=?1
             ORDER BY id LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![now_ms, sqlite_usize(limit, "outbox query limit")?],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            let (id, notifier_id, event_json, attempts) = row?;
            items.push(OutboxItem {
                id,
                notifier_id,
                event: serde_json::from_str(&event_json)
                    .context("notification outbox contains invalid event JSON")?,
                attempts: u32::try_from(attempts).context("invalid outbox attempt count")?,
            });
        }
        Ok(items)
    }

    pub fn mark_notification_sent(&self, id: i64) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        connection.execute(
            "UPDATE notification_outbox SET state='sent', updated_at_ms=?2 WHERE id=?1",
            params![id, Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    pub fn retry_notification(&self, id: i64, next_attempt_at_ms: i64, error: &str) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        connection.execute(
            "UPDATE notification_outbox SET attempts=attempts+1, next_attempt_at_ms=?2,
             last_error=?3, updated_at_ms=?4 WHERE id=?1",
            params![id, next_attempt_at_ms, error, Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    pub fn dead_letter_notification(&self, id: i64, error: &str) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        connection.execute(
            "UPDATE notification_outbox SET state='dead_letter', attempts=attempts+1,
             last_error=?2, updated_at_ms=?3 WHERE id=?1",
            params![id, error, Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    pub fn next_job_id(&self) -> Result<u64> {
        let connection = self
            .connection
            .lock()
            .expect("state database lock poisoned");
        let maximum: Option<i64> =
            connection.query_row("SELECT MAX(id) FROM jobs", [], |row| row.get(0))?;
        let maximum = maximum.unwrap_or(0);
        let maximum = u64::try_from(maximum).context("stored job ID is negative")?;
        maximum.checked_add(1).context("job ID space exhausted")
    }
}

pub fn default_state_db_path() -> Result<PathBuf> {
    Ok(default_state_root()?.join("state.db"))
}

pub fn default_state_root() -> Result<PathBuf> {
    if let Some(root) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(root).join("pan-tui"));
    }
    let home = env::var_os("HOME").context("HOME is not set; cannot locate state database")?;
    Ok(PathBuf::from(home).join(".local/state/pan-tui"))
}

fn migrate(connection: &Connection) -> Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("state database schema {version} is newer than supported {SCHEMA_VERSION}");
    }
    if version == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE jobs (
               id INTEGER PRIMARY KEY,
               provider_id TEXT NOT NULL,
               profile_id TEXT NOT NULL,
               source_id TEXT NOT NULL,
               state TEXT NOT NULL,
               destination TEXT NOT NULL,
               request_json TEXT NOT NULL,
               files INTEGER,
               items INTEGER,
               known_bytes INTEGER,
               workers INTEGER,
               summary_json TEXT,
               error TEXT,
               created_at_ms INTEGER NOT NULL,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE transfers (
               job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
               transfer_id INTEGER NOT NULL,
               path TEXT NOT NULL,
               state TEXT NOT NULL,
               size INTEGER,
               written INTEGER NOT NULL DEFAULT 0,
               error TEXT,
               updated_at_ms INTEGER NOT NULL,
               PRIMARY KEY(job_id, transfer_id)
             );
             CREATE TABLE notification_outbox (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               idempotency_key TEXT NOT NULL UNIQUE,
               notifier_id TEXT NOT NULL,
               event_json TEXT NOT NULL,
               state TEXT NOT NULL DEFAULT 'pending',
               attempts INTEGER NOT NULL DEFAULT 0,
               next_attempt_at_ms INTEGER NOT NULL,
               last_error TEXT,
               created_at_ms INTEGER NOT NULL,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE INDEX notification_outbox_due
               ON notification_outbox(state, next_attempt_at_ms, id);
             CREATE TABLE ipc_submissions (
               client_id TEXT NOT NULL,
               request_id TEXT NOT NULL,
               job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
               request_json TEXT NOT NULL,
               created_at_ms INTEGER NOT NULL,
               PRIMARY KEY(client_id, request_id)
             );
             PRAGMA user_version=3;
             COMMIT;",
        )?;
    } else if version == 1 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE notification_outbox (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               idempotency_key TEXT NOT NULL UNIQUE,
               notifier_id TEXT NOT NULL,
               event_json TEXT NOT NULL,
               state TEXT NOT NULL DEFAULT 'pending',
               attempts INTEGER NOT NULL DEFAULT 0,
               next_attempt_at_ms INTEGER NOT NULL,
               last_error TEXT,
               created_at_ms INTEGER NOT NULL,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE INDEX notification_outbox_due
               ON notification_outbox(state, next_attempt_at_ms, id);
             PRAGMA user_version=2;
             COMMIT;",
        )?;
    }
    if version <= 2 {
        let has_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ipc_submissions')",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE ipc_submissions (
                   client_id TEXT NOT NULL,
                   request_id TEXT NOT NULL,
                   job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                   request_json TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   PRIMARY KEY(client_id, request_id)
                 );
                 PRAGMA user_version=3;
                 COMMIT;",
            )?;
        }
    }
    Ok(())
}

fn insert_job(
    connection: &Connection,
    job_id: JobId,
    request: &DownloadJobRequest,
    request_json: &str,
    now: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO jobs (
            id, provider_id, profile_id, source_id, state, destination,
            request_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?7)",
        params![
            sqlite_u64(job_id.0, "job ID")?,
            request.target.reference.identity.provider_id.as_str(),
            request.target.reference.identity.profile_id.as_str(),
            request.target.reference.source_id.as_str(),
            request.destination.to_string_lossy(),
            request_json,
            now,
        ],
    )?;
    Ok(())
}

fn validate_client_id(client_id: &str) -> Result<()> {
    if client_id.trim().is_empty() || client_id.len() > 128 {
        bail!("IPC client ID must contain 1 to 128 bytes");
    }
    Ok(())
}

fn update_job_state(
    connection: &Connection,
    job_id: JobId,
    state: &str,
    now: i64,
    summary: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let job_id = sqlite_u64(job_id.0, "job ID")?;
    connection.execute(
        "UPDATE jobs SET state=?2, summary_json=COALESCE(?3, summary_json),
         error=COALESCE(?4, error), updated_at_ms=?5 WHERE id=?1",
        params![job_id, state, summary, error, now],
    )?;
    Ok(())
}

fn upsert_transfer(connection: &Connection, update: TransferUpdate<'_>) -> Result<()> {
    let job_id = sqlite_u64(update.job_id.0, "job ID")?;
    let transfer_id = sqlite_usize(update.transfer_id, "transfer ID")?;
    let size = sqlite_optional_u64(update.size, "transfer size")?;
    let written = sqlite_u64(update.written, "written byte count")?;
    connection.execute(
        "INSERT INTO transfers (job_id, transfer_id, path, state, size, written, error, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(job_id, transfer_id) DO UPDATE SET
           path=CASE WHEN transfers.state IN ('completed','skipped','failed')
                     THEN transfers.path ELSE excluded.path END,
           state=CASE WHEN transfers.state IN ('completed','skipped','failed')
                      THEN transfers.state ELSE excluded.state END,
           size=CASE WHEN transfers.state IN ('completed','skipped','failed')
                     THEN transfers.size ELSE COALESCE(excluded.size, transfers.size) END,
           written=CASE WHEN transfers.state IN ('completed','skipped','failed')
                        THEN transfers.written ELSE excluded.written END,
           error=CASE WHEN transfers.state IN ('completed','skipped','failed')
                      THEN transfers.error ELSE excluded.error END,
           updated_at_ms=excluded.updated_at_ms",
        params![
            job_id,
            transfer_id,
            update.path.to_string_lossy(),
            update.state,
            size,
            written,
            update.error,
            update.now
        ],
    )?;
    Ok(())
}

fn sqlite_u64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("{label} exceeds SQLite INTEGER range"))
}

fn sqlite_usize(value: usize, label: &str) -> Result<i64> {
    sqlite_u64(value as u64, label)
}

fn sqlite_optional_u64(value: Option<u64>, label: &str) -> Result<Option<i64>> {
    value.map(|value| sqlite_u64(value, label)).transpose()
}

fn parse_request(job_id: JobId, request_json: &str) -> Result<DownloadJobRequest> {
    let request: DownloadJobRequest = serde_json::from_str(request_json)
        .with_context(|| format!("job {} has an invalid persisted request", job_id.0))?;
    if request.schema_version != DownloadJobRequest::SCHEMA_VERSION {
        bail!(
            "job {} request schema {} is not supported",
            job_id.0,
            request.schema_version
        );
    }
    Ok(request)
}

fn parse_job_state(state: &str) -> Result<DownloadJobState> {
    match state {
        "queued" => Ok(DownloadJobState::Queued),
        "scanning" => Ok(DownloadJobState::Scanning),
        "running" => Ok(DownloadJobState::Running),
        "finished" => Ok(DownloadJobState::Finished),
        "failed" => Ok(DownloadJobState::Failed),
        "cancelled" => Ok(DownloadJobState::Cancelled),
        "interrupted" => Ok(DownloadJobState::Interrupted),
        _ => bail!("unknown persisted Job state: {state}"),
    }
}

fn parse_transfer_state(state: &str) -> Result<TransferState> {
    match state {
        "queued" => Ok(TransferState::Queued),
        "running" => Ok(TransferState::Running),
        "completed" => Ok(TransferState::Completed),
        "skipped" => Ok(TransferState::Skipped),
        "failed" => Ok(TransferState::Failed),
        "interrupted" => Ok(TransferState::Interrupted),
        _ => bail!("unknown persisted Transfer state: {state}"),
    }
}

fn notification_from_download_event(event: &DownloadEvent) -> Option<NotificationEvent> {
    match event {
        DownloadEvent::Finished {
            job_id,
            provider,
            summary,
        } if summary.failed > 0 => Some(NotificationEvent::JobPartiallyFailed {
            job_id: *job_id,
            provider: provider.clone(),
            downloaded: summary.downloaded,
            skipped: summary.skipped,
            failed: summary.failed,
        }),
        DownloadEvent::Finished {
            job_id,
            provider,
            summary,
        } => Some(NotificationEvent::JobCompleted {
            job_id: *job_id,
            provider: provider.clone(),
            downloaded: summary.downloaded,
            skipped: summary.skipped,
            bytes_written: summary.bytes_written,
        }),
        DownloadEvent::Fatal {
            job_id, provider, ..
        } => Some(NotificationEvent::JobFailed {
            job_id: *job_id,
            provider: provider.clone(),
        }),
        _ => None,
    }
}

fn enqueue_notification(
    connection: &Connection,
    targets: &[String],
    event: &NotificationEvent,
    now: i64,
) -> Result<()> {
    let event_json = serde_json::to_string(event)?;
    for notifier_id in targets {
        let idempotency_key = format!("{notifier_id}:{}", event.idempotency_suffix());
        connection.execute(
            "INSERT OR IGNORE INTO notification_outbox (
               idempotency_key, notifier_id, event_json, state, attempts,
               next_attempt_at_ms, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?4, ?4)",
            params![idempotency_key, notifier_id, event_json, now],
        )?;
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create state directory: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn restrict_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn prepare_private_database(path: &Path) -> Result<()> {
    if path.exists() {
        return restrict_file(path);
    }
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => restrict_file(path),
        Err(error) => Err(error)
            .with_context(|| format!("failed to create state database: {}", path.display())),
    }
}

fn restrict_sqlite_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            restrict_file(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::DownloadOptions;
    use crate::provider::{
        DownloadCapability, ProfileId, ProviderId, ProviderIdentity, RemoteItem, RemoteItemKind,
        RemoteObjectId, RemoteRef, SourceId,
    };

    fn target() -> RemoteItem {
        RemoteItem {
            reference: RemoteRef {
                identity: ProviderIdentity::new(
                    ProviderId::new("google").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
                source_id: SourceId::new("source").unwrap(),
                object_id: RemoteObjectId::new("opaque-object").unwrap(),
            },
            name: "file.bin".to_owned(),
            kind: RemoteItemKind::File,
            size: Some(10),
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        }
    }

    fn request() -> DownloadJobRequest {
        DownloadJobRequest::new(
            target(),
            PathBuf::from("/tmp/downloads"),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 2,
            },
        )
    }

    #[test]
    fn records_jobs_and_recovers_active_states() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state/state.db");
        let store = StateStore::open(&path).unwrap();
        store.record_submission(JobId(7), &request()).unwrap();
        store
            .record_event(&DownloadEvent::Scanning {
                job_id: JobId(7),
                provider: target().reference.identity,
            })
            .unwrap();
        assert_eq!(store.load_job(JobId(7)).unwrap().unwrap().state, "scanning");
        assert_eq!(store.recover_interrupted().unwrap(), 1);
        assert_eq!(
            store.load_job(JobId(7)).unwrap().unwrap().state,
            "interrupted"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                let sidecar = PathBuf::from(sidecar);
                assert_eq!(
                    fs::metadata(sidecar).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn rejects_a_newer_schema() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state.db");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        let error = StateStore::open(&path).err().unwrap().to_string();
        assert!(error.contains("newer than supported"));
    }

    #[test]
    fn persists_progress_terminal_state_and_cancel_across_reopen() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state.db");
        let store = StateStore::open(&path).unwrap();
        store.record_submission(JobId(9), &request()).unwrap();
        store
            .record_event(&DownloadEvent::TransferQueued {
                job_id: JobId(9),
                provider: target().reference.identity,
                transfer_id: TransferId(3),
                path: PathBuf::from("file.bin"),
                size: Some(10),
            })
            .unwrap();
        store
            .record_progress_batch(&[TransferProgress {
                job_id: JobId(9),
                transfer_id: TransferId(3),
                written: 6,
                size: Some(10),
            }])
            .unwrap();
        store
            .record_event(&DownloadEvent::FileCompleted {
                job_id: JobId(9),
                provider: target().reference.identity,
                transfer_id: TransferId(3),
                path: PathBuf::from("file.bin"),
                bytes: 10,
            })
            .unwrap();
        store
            .record_progress_batch(&[TransferProgress {
                job_id: JobId(9),
                transfer_id: TransferId(3),
                written: 4,
                size: Some(10),
            }])
            .unwrap();
        store
            .record_event(&DownloadEvent::Cancelled {
                job_id: JobId(9),
                provider: target().reference.identity,
            })
            .unwrap();
        assert_eq!(store.next_job_id().unwrap(), 10);
        drop(store);

        let reopened = StateStore::open(&path).unwrap();
        let snapshot = reopened.load_snapshots().unwrap().pop().unwrap();
        assert_eq!(snapshot.state, DownloadJobState::Cancelled);
        let transfer = snapshot.transfers.get(&TransferId(3)).unwrap();
        assert_eq!(transfer.state, TransferState::Completed);
        assert_eq!(transfer.written, 10);
    }

    #[test]
    fn exposes_interrupted_requests_for_resume() {
        let temporary = tempfile::tempdir().unwrap();
        let store = StateStore::open(&temporary.path().join("state.db")).unwrap();
        store.record_submission(JobId(4), &request()).unwrap();
        store
            .record_event(&DownloadEvent::Scanning {
                job_id: JobId(4),
                provider: target().reference.identity,
            })
            .unwrap();
        store.recover_interrupted().unwrap();

        let interrupted = store.load_interrupted_requests().unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].0, JobId(4));
        assert_eq!(interrupted[0].1.target.name, "file.bin");
        store.mark_requeued(JobId(4)).unwrap();
        assert!(store.load_interrupted_requests().unwrap().is_empty());
    }

    #[test]
    fn commits_terminal_job_and_idempotent_outbox_together() {
        let temporary = tempfile::tempdir().unwrap();
        let store = StateStore::open(&temporary.path().join("state.db")).unwrap();
        store.set_notification_targets(vec!["dingtalk".to_owned(), "feishu".to_owned()]);
        store.record_submission(JobId(20), &request()).unwrap();
        let event = DownloadEvent::Finished {
            job_id: JobId(20),
            provider: target().reference.identity,
            summary: DownloadSummary {
                downloaded: 1,
                skipped: 0,
                failed: 0,
                bytes_written: 10,
                partial_bytes: 0,
            },
        };
        store.record_event(&event).unwrap();
        store.record_event(&event).unwrap();

        assert_eq!(
            store.load_job(JobId(20)).unwrap().unwrap().state,
            "finished"
        );
        let outbox = store
            .load_due_notifications(Utc::now().timestamp_millis(), 10)
            .unwrap();
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox[0].attempts, 0);
        store.mark_notification_sent(outbox[0].id).unwrap();
        assert_eq!(
            store
                .load_due_notifications(Utc::now().timestamp_millis(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn migrates_schema_one_to_notification_outbox() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state.db");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);

        let store = StateStore::open(&path).unwrap();
        assert!(
            store
                .load_due_notifications(Utc::now().timestamp_millis(), 10)
                .unwrap()
                .is_empty()
        );
        let connection = store.connection.lock().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
    }
}
