use std::{collections::BTreeMap, fs, future::Future, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    download::JobId, provider::ProviderIdentity, settings::NetworkSettings, state::StateStore,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationEvent {
    JobCompleted {
        job_id: JobId,
        provider: ProviderIdentity,
        downloaded: usize,
        skipped: usize,
        bytes_written: u64,
    },
    JobPartiallyFailed {
        job_id: JobId,
        provider: ProviderIdentity,
        downloaded: usize,
        skipped: usize,
        failed: usize,
    },
    JobFailed {
        job_id: JobId,
        provider: ProviderIdentity,
    },
    ReauthRequired {
        provider: ProviderIdentity,
    },
}

impl NotificationEvent {
    pub fn idempotency_suffix(&self) -> String {
        match self {
            Self::JobCompleted { job_id, .. } => format!("job:{}:completed", job_id.0),
            Self::JobPartiallyFailed { job_id, .. } => {
                format!("job:{}:partially_failed", job_id.0)
            }
            Self::JobFailed { job_id, .. } => format!("job:{}:failed", job_id.0),
            Self::ReauthRequired { provider } => format!(
                "provider:{}:{}:reauth_required",
                provider.provider_id, provider.profile_id
            ),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::JobCompleted {
                job_id,
                provider,
                downloaded,
                skipped,
                bytes_written,
            } => format!(
                "pan-tui Job {} completed on {}/{}: {} downloaded, {} skipped, {} bytes written",
                job_id.0,
                provider.provider_id,
                provider.profile_id,
                downloaded,
                skipped,
                bytes_written
            ),
            Self::JobPartiallyFailed {
                job_id,
                provider,
                downloaded,
                skipped,
                failed,
            } => format!(
                "pan-tui Job {} partially failed on {}/{}: {} downloaded, {} skipped, {} failed",
                job_id.0, provider.provider_id, provider.profile_id, downloaded, skipped, failed
            ),
            Self::JobFailed { job_id, provider } => format!(
                "pan-tui Job {} failed on {}/{}",
                job_id.0, provider.provider_id, provider.profile_id
            ),
            Self::ReauthRequired { provider } => format!(
                "pan-tui requires authentication for {}/{}",
                provider.provider_id, provider.profile_id
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WebhookNotifierConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook: String,
    #[serde(default)]
    pub secret: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NotifierConfigFile {
    #[serde(default)]
    pub notifiers: BTreeMap<String, WebhookNotifierConfig>,
}

impl NotifierConfigFile {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read notifier config: {}", path.display())
                });
            }
        };
        toml::from_str(&contents)
            .with_context(|| format!("invalid notifier config TOML: {}", path.display()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifierSettingKind {
    Boolean,
    Text,
    Secret,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotifierSettingSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: NotifierSettingKind,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifierErrorKind {
    Config,
    RateLimit,
    Network,
    Remote,
}

#[derive(Clone, Debug)]
pub struct NotifierError {
    pub kind: NotifierErrorKind,
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for NotifierError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NotifierError {}

pub type NotifierResult<T> = std::result::Result<T, NotifierError>;

#[async_trait]
pub trait Notifier: Send + Sync {
    fn id(&self) -> &str;
    async fn send(&self, event: &NotificationEvent) -> NotifierResult<()>;
}

pub trait NotifierFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn settings_schema(&self) -> Vec<NotifierSettingSpec> {
        vec![
            NotifierSettingSpec {
                key: "enabled",
                label: "Enabled",
                kind: NotifierSettingKind::Boolean,
                required: false,
            },
            NotifierSettingSpec {
                key: "webhook",
                label: "Webhook",
                kind: NotifierSettingKind::Secret,
                required: true,
            },
            NotifierSettingSpec {
                key: "secret",
                label: "Signing secret",
                kind: NotifierSettingKind::Secret,
                required: false,
            },
        ]
    }
    fn create(
        &self,
        config: WebhookNotifierConfig,
        network: &NetworkSettings,
    ) -> Result<Arc<dyn Notifier>>;
}

pub struct NotificationWorker {
    state: StateStore,
    notifiers: BTreeMap<String, Arc<dyn Notifier>>,
}

impl NotificationWorker {
    pub fn new(state: StateStore, notifiers: Vec<Arc<dyn Notifier>>) -> Self {
        Self {
            state,
            notifiers: notifiers
                .into_iter()
                .map(|notifier| (notifier.id().to_owned(), notifier))
                .collect(),
        }
    }

    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = self.run_once().await;
                }
                _ = &mut shutdown => break,
            }
        }
    }

    pub async fn run_once(&self) -> Result<usize> {
        let items = self
            .state
            .load_due_notifications(Utc::now().timestamp_millis(), 32)?;
        let count = items.len();
        for item in items {
            let Some(notifier) = self.notifiers.get(&item.notifier_id) else {
                self.state
                    .dead_letter_notification(item.id, "configured notifier is unavailable")?;
                continue;
            };
            match notifier.send(&item.event).await {
                Ok(()) => self.state.mark_notification_sent(item.id)?,
                Err(error) if error.retryable && item.attempts < 7 => {
                    let backoff = error
                        .retry_after
                        .unwrap_or_else(|| retry_delay(item.id, item.attempts));
                    let delay_ms = i64::try_from(backoff.as_millis()).unwrap_or(i64::MAX);
                    self.state.retry_notification(
                        item.id,
                        Utc::now().timestamp_millis().saturating_add(delay_ms),
                        &error.message,
                    )?;
                }
                Err(error) => self
                    .state
                    .dead_letter_notification(item.id, &error.message)?,
            }
        }
        Ok(count)
    }
}

fn retry_delay(id: i64, attempts: u32) -> Duration {
    let exponent = attempts.min(10);
    let base = 2_u64.saturating_pow(exponent).min(3600);
    let jitter_ms = id.unsigned_abs().wrapping_mul(97) % 1000;
    Duration::from_secs(base) + Duration::from_millis(jitter_ms)
}

#[derive(Default)]
pub struct NotifierRegistry {
    factories: BTreeMap<String, Arc<dyn NotifierFactory>>,
}

pub struct NotifierLoadOutcome {
    pub notifiers: Vec<Arc<dyn Notifier>>,
    pub failed_ids: Vec<String>,
}

impl NotifierRegistry {
    pub fn register<F>(&mut self, factory: F) -> Result<()>
    where
        F: NotifierFactory + 'static,
    {
        let id = factory.id().to_owned();
        if self.factories.contains_key(&id) {
            bail!("duplicate notifier ID: {id}");
        }
        self.factories.insert(id, Arc::new(factory));
        Ok(())
    }

    pub fn create_enabled(
        &self,
        configs: &BTreeMap<String, WebhookNotifierConfig>,
        network: &NetworkSettings,
    ) -> Result<Vec<Arc<dyn Notifier>>> {
        let mut notifiers = Vec::new();
        for (id, config) in configs {
            if !config.enabled {
                continue;
            }
            let factory = self
                .factories
                .get(id)
                .with_context(|| format!("notifier {id} is not registered"))?;
            notifiers.push(factory.create(config.clone(), network)?);
        }
        Ok(notifiers)
    }

    pub fn create_enabled_isolated(
        &self,
        configs: &BTreeMap<String, WebhookNotifierConfig>,
        network: &NetworkSettings,
    ) -> NotifierLoadOutcome {
        let mut notifiers = Vec::new();
        let mut failed_ids = Vec::new();
        for (id, config) in configs {
            if !config.enabled {
                continue;
            }
            let created = self
                .factories
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("notifier is not registered"))
                .and_then(|factory| factory.create(config.clone(), network));
            match created {
                Ok(notifier) => notifiers.push(notifier),
                Err(_) => failed_ids.push(id.clone()),
            }
        }
        NotifierLoadOutcome {
            notifiers,
            failed_ids,
        }
    }
}

pub struct DingTalkNotifierFactory;

impl NotifierFactory for DingTalkNotifierFactory {
    fn id(&self) -> &'static str {
        "dingtalk"
    }

    fn display_name(&self) -> &'static str {
        "DingTalk"
    }

    fn create(
        &self,
        config: WebhookNotifierConfig,
        network: &NetworkSettings,
    ) -> Result<Arc<dyn Notifier>> {
        Ok(Arc::new(WebhookNotifier::new(
            WebhookKind::DingTalk,
            config,
            network,
        )?))
    }
}

pub struct FeishuNotifierFactory;

impl NotifierFactory for FeishuNotifierFactory {
    fn id(&self) -> &'static str {
        "feishu"
    }

    fn display_name(&self) -> &'static str {
        "Feishu"
    }

    fn create(
        &self,
        config: WebhookNotifierConfig,
        network: &NetworkSettings,
    ) -> Result<Arc<dyn Notifier>> {
        Ok(Arc::new(WebhookNotifier::new(
            WebhookKind::Feishu,
            config,
            network,
        )?))
    }
}

#[derive(Clone, Copy)]
enum WebhookKind {
    DingTalk,
    Feishu,
}

struct WebhookNotifier {
    id: &'static str,
    kind: WebhookKind,
    webhook: Url,
    secret: String,
    client: Client,
}

impl WebhookNotifier {
    fn new(
        kind: WebhookKind,
        config: WebhookNotifierConfig,
        network: &NetworkSettings,
    ) -> Result<Self> {
        if config.webhook.trim().is_empty() {
            bail!("notifier webhook cannot be blank");
        }
        let webhook = Url::parse(&config.webhook).context("invalid notifier webhook URL")?;
        let builder =
            network.configure_client(Client::builder().timeout(Duration::from_secs(15)))?;
        Ok(Self {
            id: match kind {
                WebhookKind::DingTalk => "dingtalk",
                WebhookKind::Feishu => "feishu",
            },
            kind,
            webhook,
            secret: config.secret,
            client: builder.build()?,
        })
    }
}

#[async_trait]
impl Notifier for WebhookNotifier {
    fn id(&self) -> &str {
        self.id
    }

    async fn send(&self, event: &NotificationEvent) -> NotifierResult<()> {
        let timestamp = Utc::now().timestamp_millis();
        let text = event.text();
        let (url, payload) = match self.kind {
            WebhookKind::DingTalk => {
                let mut url = self.webhook.clone();
                if !self.secret.is_empty() {
                    let signature = dingtalk_signature(timestamp, &self.secret)?;
                    url.query_pairs_mut()
                        .append_pair("timestamp", &timestamp.to_string())
                        .append_pair("sign", &signature);
                }
                (
                    url,
                    serde_json::json!({"msgtype": "text", "text": {"content": text}}),
                )
            }
            WebhookKind::Feishu => {
                let mut payload = serde_json::json!({
                    "msg_type": "text",
                    "content": {"text": text}
                });
                if !self.secret.is_empty() {
                    payload["timestamp"] =
                        serde_json::Value::String((timestamp / 1000).to_string());
                    payload["sign"] = serde_json::Value::String(feishu_signature(
                        timestamp / 1000,
                        &self.secret,
                    )?);
                }
                (self.webhook.clone(), payload)
            }
        };
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|_| NotifierError {
                kind: NotifierErrorKind::Network,
                message: "notifier request failed".to_owned(),
                retryable: true,
                retry_after: None,
            })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(NotifierError {
                kind: NotifierErrorKind::RateLimit,
                message: "notifier rate limit exceeded".to_owned(),
                retryable: true,
                retry_after,
            });
        }
        if status.is_server_error() {
            return Err(NotifierError {
                kind: NotifierErrorKind::Remote,
                message: format!("notifier service returned HTTP {status}"),
                retryable: true,
                retry_after,
            });
        }
        if !status.is_success() {
            return Err(NotifierError {
                kind: NotifierErrorKind::Remote,
                message: format!("notifier rejected the request with HTTP {status}"),
                retryable: false,
                retry_after: None,
            });
        }
        let body: serde_json::Value = response.json().await.map_err(|_| NotifierError {
            kind: NotifierErrorKind::Remote,
            message: "notifier returned an invalid response".to_owned(),
            retryable: true,
            retry_after: None,
        })?;
        let accepted = match self.kind {
            WebhookKind::DingTalk => {
                body.get("errcode").and_then(|value| value.as_i64()) == Some(0)
            }
            WebhookKind::Feishu => {
                body.get("code").and_then(|value| value.as_i64()) == Some(0)
                    || body.get("StatusCode").and_then(|value| value.as_i64()) == Some(0)
            }
        };
        if !accepted {
            return Err(NotifierError {
                kind: NotifierErrorKind::Remote,
                message: "notifier rejected the message".to_owned(),
                retryable: false,
                retry_after: None,
            });
        }
        Ok(())
    }
}

fn dingtalk_signature(timestamp: i64, secret: &str) -> NotifierResult<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| NotifierError {
        kind: NotifierErrorKind::Config,
        message: "invalid DingTalk signing secret".to_owned(),
        retryable: false,
        retry_after: None,
    })?;
    mac.update(format!("{timestamp}\n{secret}").as_bytes());
    Ok(STANDARD.encode(mac.finalize().into_bytes()))
}

fn feishu_signature(timestamp: i64, secret: &str) -> NotifierResult<String> {
    let signing_key = format!("{timestamp}\n{secret}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(signing_key.as_bytes()).map_err(|_| NotifierError {
            kind: NotifierErrorKind::Config,
            message: "invalid Feishu signing secret".to_owned(),
            retryable: false,
            retry_after: None,
        })?;
    mac.update(&[]);
    Ok(STANDARD.encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        download::{DownloadEvent, DownloadOptions, DownloadSummary},
        job::DownloadJobRequest,
        provider::{
            DownloadCapability, ProfileId, ProviderId, RemoteItem, RemoteItemKind, RemoteObjectId,
            RemoteRef, SourceId,
        },
    };

    struct CountingNotifier {
        sent: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Notifier for CountingNotifier {
        fn id(&self) -> &str {
            "counting"
        }

        async fn send(&self, _event: &NotificationEvent) -> NotifierResult<()> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn target() -> RemoteItem {
        RemoteItem {
            reference: RemoteRef {
                identity: ProviderIdentity::new(
                    ProviderId::new("test").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
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

    #[test]
    fn produces_protocol_compatible_signatures() {
        assert_eq!(
            dingtalk_signature(1_700_000_000_000, "SECtest").unwrap(),
            "aZLLrriXgn05YbwaGR7knYsLeJADjr9NwLaNNKpxh4g="
        );
        assert_eq!(
            feishu_signature(1_700_000_000, "SECtest").unwrap(),
            "G7XpBpG8NgG02fJOAhX6FRAObIljmFoxVReo8I62pEk="
        );
    }

    #[tokio::test]
    async fn worker_delivers_each_idempotent_terminal_event_once() {
        let temporary = tempfile::tempdir().unwrap();
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        state.set_notification_targets(vec!["counting".to_owned()]);
        let request = DownloadJobRequest::new(
            target(),
            temporary.path().join("downloads"),
            DownloadOptions {
                overwrite: false,
                resume: true,
                concurrency: 1,
            },
        );
        state.record_submission(JobId(1), &request).unwrap();
        let event = DownloadEvent::Finished {
            job_id: JobId(1),
            provider: target().reference.identity,
            summary: DownloadSummary {
                downloaded: 1,
                skipped: 0,
                failed: 0,
                bytes_written: 4,
                partial_bytes: 0,
            },
        };
        state.record_event(&event).unwrap();
        state.record_event(&event).unwrap();
        let sent = Arc::new(AtomicUsize::new(0));
        let worker = NotificationWorker::new(
            state,
            vec![Arc::new(CountingNotifier {
                sent: Arc::clone(&sent),
            })],
        );

        assert_eq!(worker.run_once().await.unwrap(), 1);
        assert_eq!(worker.run_once().await.unwrap(), 0);
        assert_eq!(sent.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn registry_rejects_duplicate_notifier_ids() {
        let mut registry = NotifierRegistry::default();
        registry.register(DingTalkNotifierFactory).unwrap();
        assert!(registry.register(DingTalkNotifierFactory).is_err());
        let outcome = registry.create_enabled_isolated(
            &BTreeMap::from([(
                "dingtalk".to_owned(),
                WebhookNotifierConfig {
                    enabled: true,
                    webhook: "not-a-url".to_owned(),
                    secret: String::new(),
                },
            )]),
            &NetworkSettings::default(),
        );
        assert!(outcome.notifiers.is_empty());
        assert_eq!(outcome.failed_ids, vec!["dingtalk"]);
    }

    #[tokio::test]
    async fn dingtalk_classifies_retry_after_without_exposing_webhook() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let bytes = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("POST /hook?timestamp="));
            assert!(request.contains("&sign="));
            assert!(!request.contains("SECtest"));
            stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
        });
        let webhook = format!("http://{address}/hook");
        let notifier = DingTalkNotifierFactory
            .create(
                WebhookNotifierConfig {
                    enabled: true,
                    webhook: webhook.clone(),
                    secret: "SECtest".to_owned(),
                },
                &NetworkSettings::default(),
            )
            .unwrap();
        let error = notifier
            .send(&NotificationEvent::ReauthRequired {
                provider: target().reference.identity,
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, NotifierErrorKind::RateLimit);
        assert_eq!(error.retry_after, Some(Duration::from_secs(3)));
        assert!(!error.message.contains(&webhook));
        assert!(!error.message.contains("SECtest"));
        server.await.unwrap();
    }
}
