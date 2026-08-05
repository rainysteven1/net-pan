use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Response, StatusCode, header::CONTENT_RANGE};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;

use crate::{
    config::ProviderConfigStore,
    provider::{
        CloudProvider, DownloadCapability, ProfileId, ProviderCapabilities, ProviderContext,
        ProviderDriveInfo, ProviderError, ProviderErrorKind, ProviderFactory, ProviderId,
        ProviderIdentity, ProviderReadiness, ProviderResult, ProviderSettingKind,
        ProviderSettingSpec, ProviderSettingsSnapshot, ProviderUserInfo, RemoteChecksum,
        RemoteDownload, RemoteItem, RemoteItemKind, RemoteObjectId, RemoteRef, RemoteSource,
        SourceId,
    },
};

const PROVIDER_ID: &str = "aliyun";
const DEFAULT_API_BASE: &str = "https://openapi.alipan.com";
const TOKEN_STATE_FILE: &str = "token.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AliyunProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default = "default_drive")]
    pub drive: String,
    #[serde(default = "default_root")]
    pub root_file_id: String,
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default)]
    pub sources: Vec<AliyunSourceConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AliyunSourceConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_drive")]
    pub drive: String,
    #[serde(default = "default_root")]
    pub root_file_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredToken {
    access_token: String,
    refresh_token: String,
    expires_at_unix: i64,
}

struct TokenStore {
    state: Mutex<StoredToken>,
    client: reqwest::Client,
    api_base: String,
    client_id: String,
    client_secret: Option<String>,
    bootstrap_refresh_token: Option<String>,
    config_store: ProviderConfigStore,
    provider_id: ProviderId,
}

impl TokenStore {
    fn new(
        config_store: ProviderConfigStore,
        provider_id: ProviderId,
        config: &AliyunProviderConfig,
        client: reqwest::Client,
    ) -> ProviderResult<Self> {
        let state = config_store
            .read_private_file(&provider_id, TOKEN_STATE_FILE)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
            .map(|contents| {
                serde_json::from_slice(&contents).map_err(|error| {
                    provider_error(
                        ProviderErrorKind::Config,
                        format!("Aliyun token state is invalid: {error}"),
                    )
                })
            })
            .transpose()?
            .unwrap_or_else(|| StoredToken {
                access_token: String::new(),
                refresh_token: config.refresh_token.clone().unwrap_or_default(),
                expires_at_unix: 0,
            });
        if state.refresh_token.trim().is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Aliyun provider requires a refresh token or provider-private token state",
            ));
        }
        Ok(Self {
            state: Mutex::new(state),
            client,
            api_base: config.api_base.trim_end_matches('/').to_owned(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            bootstrap_refresh_token: config.refresh_token.clone(),
            config_store,
            provider_id,
        })
    }

    async fn access_token(&self) -> ProviderResult<String> {
        self.access_token_inner(false).await
    }

    async fn force_refresh(&self) -> ProviderResult<String> {
        self.access_token_inner(true).await
    }

    async fn access_token_inner(&self, force: bool) -> ProviderResult<String> {
        let mut state = self.state.lock().await;
        let now = chrono::Utc::now().timestamp();
        if !force && !state.access_token.is_empty() && state.expires_at_unix > now + 60 {
            return Ok(state.access_token.clone());
        }
        let refresh_token = if state.refresh_token.is_empty() {
            self.bootstrap_refresh_token.clone().unwrap_or_default()
        } else {
            state.refresh_token.clone()
        };
        let response = self
            .client
            .post(format!("{}/oauth/access_token", self.api_base))
            .form(&TokenRequest {
                client_id: &self.client_id,
                client_secret: self.client_secret.as_deref(),
                grant_type: "refresh_token",
                refresh_token: &refresh_token,
            })
            .send()
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(api_response_error(status, response).await);
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Auth, error))?;
        if token.access_token.trim().is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Auth,
                "Aliyun token response did not contain an access token",
            ));
        }
        let next_refresh_token = token
            .refresh_token
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| state.refresh_token.clone());
        if next_refresh_token.trim().is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Auth,
                "Aliyun token response did not contain a refresh token",
            ));
        }
        let next = StoredToken {
            access_token: token.access_token,
            refresh_token: next_refresh_token,
            expires_at_unix: now.saturating_add(token.expires_in.max(60)),
        };
        let contents = serde_json::to_vec_pretty(&next)
            .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
        self.config_store
            .write_private_file_atomic(&self.provider_id, TOKEN_STATE_FILE, &contents)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        *state = next;
        Ok(state.access_token.clone())
    }
}

#[derive(Serialize)]
struct TokenRequest<'a> {
    client_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<&'a str>,
    grant_type: &'a str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

#[derive(Clone, Debug, Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    code: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DriveInfoResponse {
    #[serde(default)]
    default_drive_id: String,
    #[serde(default)]
    resource_drive_id: Option<String>,
    #[serde(default)]
    backup_drive_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct UserInfoResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct FileResponse {
    drive_id: String,
    file_id: String,
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    content_hash: Option<String>,
    #[serde(default)]
    content_hash_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ListResponse {
    #[serde(default)]
    items: Vec<FileResponse>,
    #[serde(default)]
    next_marker: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct DownloadUrlResponse {
    url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AliyunObjectRef {
    drive_id: String,
    file_id: String,
}

#[derive(Clone, Debug)]
struct SourceConfig {
    id: SourceId,
    name: String,
    drive: String,
    root_file_id: String,
}

pub struct AliyunProviderFactory;

pub fn aliyun_id() -> ProviderId {
    ProviderId::new(PROVIDER_ID).expect("static Aliyun provider id is valid")
}

#[async_trait]
impl ProviderFactory for AliyunProviderFactory {
    fn id(&self) -> ProviderId {
        aliyun_id()
    }

    fn display_name(&self) -> &'static str {
        "Aliyun Drive"
    }

    fn settings_schema(&self) -> Vec<ProviderSettingSpec> {
        vec![
            ProviderSettingSpec {
                key: "enabled",
                label: "Enabled",
                kind: ProviderSettingKind::Boolean,
                required: true,
            },
            ProviderSettingSpec {
                key: "client_id",
                label: "OpenAPI client ID",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "client_secret",
                label: "OpenAPI client secret",
                kind: ProviderSettingKind::Secret,
                required: false,
            },
            ProviderSettingSpec {
                key: "refresh_token",
                label: "OpenAPI refresh token",
                kind: ProviderSettingKind::Secret,
                required: true,
            },
            ProviderSettingSpec {
                key: "drive",
                label: "Drive kind",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "root_file_id",
                label: "Default root file ID",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "sources",
                label: "Saved folders",
                kind: ProviderSettingKind::Action,
                required: false,
            },
            ProviderSettingSpec {
                key: "proxy",
                label: "Provider proxy",
                kind: ProviderSettingKind::Text,
                required: false,
            },
            ProviderSettingSpec {
                key: "api_base",
                label: "OpenAPI base URL",
                kind: ProviderSettingKind::Text,
                required: true,
            },
        ]
    }

    fn load_settings(&self, context: &ProviderContext) -> ProviderResult<ProviderSettingsSnapshot> {
        let config = load_config(context)?.unwrap_or_else(default_editable_config);
        let store = ProviderConfigStore::new(context.config_root.clone());
        let token_state_exists = store
            .read_private_file(&aliyun_id(), TOKEN_STATE_FILE)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
            .is_some();
        let mut configured_secrets = std::collections::BTreeSet::new();
        if config
            .client_secret
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            configured_secrets.insert("client_secret".to_owned());
        }
        if token_state_exists
            || config
                .refresh_token
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        {
            configured_secrets.insert("refresh_token".to_owned());
        }
        let values = BTreeMap::from([
            ("enabled".to_owned(), config.enabled.to_string()),
            ("client_id".to_owned(), config.client_id),
            ("client_secret".to_owned(), String::new()),
            ("refresh_token".to_owned(), String::new()),
            ("drive".to_owned(), config.drive),
            ("root_file_id".to_owned(), config.root_file_id),
            (
                "sources".to_owned(),
                format!("{} configured source(s)", config.sources.len()),
            ),
            ("proxy".to_owned(), config.proxy.unwrap_or_default()),
            ("api_base".to_owned(), config.api_base),
        ]);
        Ok(ProviderSettingsSnapshot {
            values,
            configured_secrets,
            read_only_reason: context
                .config_overrides
                .contains_key(&aliyun_id())
                .then(|| "Aliyun settings are overridden for this run".to_owned()),
        })
    }

    fn save_settings(
        &self,
        context: &ProviderContext,
        changes: &BTreeMap<String, String>,
    ) -> ProviderResult<()> {
        if context.config_overrides.contains_key(&aliyun_id()) {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Aliyun settings are read-only while runtime overrides are active",
            ));
        }
        let mut config = load_config(context)?.unwrap_or_else(default_editable_config);
        if let Some(value) = changes.get("enabled") {
            config.enabled = parse_bool_setting("enabled", value)?;
        }
        if let Some(value) = changes.get("client_id") {
            config.client_id = value.trim().to_owned();
        }
        if let Some(value) = changes
            .get("client_secret")
            .filter(|value| !value.trim().is_empty())
        {
            config.client_secret = Some(value.trim().to_owned());
        }
        let refresh_changed = changes
            .get("refresh_token")
            .is_some_and(|value| !value.trim().is_empty());
        if let Some(value) = changes
            .get("refresh_token")
            .filter(|value| !value.trim().is_empty())
        {
            config.refresh_token = Some(value.trim().to_owned());
        }
        if let Some(value) = changes.get("drive") {
            config.drive = value.trim().to_owned();
        }
        if let Some(value) = changes.get("root_file_id") {
            config.root_file_id = value.trim().to_owned();
        }
        if let Some(value) = changes.get("proxy") {
            config.proxy = (!value.trim().is_empty()).then(|| value.trim().to_owned());
        }
        if let Some(value) = changes.get("api_base") {
            config.api_base = value.trim().to_owned();
        }
        let contents = toml::to_string_pretty(&config)
            .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
        let mut validation_context = context.clone();
        validation_context
            .config_overrides
            .insert(aliyun_id(), contents.as_bytes().to_vec());
        load_config(&validation_context)?;
        let store = ProviderConfigStore::new(context.config_root.clone());
        store
            .write_config(&aliyun_id(), contents.as_bytes())
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        if refresh_changed {
            store
                .remove_private_file(&aliyun_id(), TOKEN_STATE_FILE)
                .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        }
        Ok(())
    }

    fn readiness(&self, context: &ProviderContext) -> ProviderReadiness {
        match load_config(context) {
            Ok(Some(config)) if !config.enabled => ProviderReadiness::Disabled,
            Ok(Some(_)) | Err(_) => ProviderReadiness::ReadyToConnect,
            Ok(None) => ProviderReadiness::NeedsConfig,
        }
    }

    async fn create(&self, context: ProviderContext) -> ProviderResult<Arc<dyn CloudProvider>> {
        let config = load_config(&context)?.ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::Config,
                "Aliyun provider is not configured",
            )
        })?;
        if !config.enabled {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Aliyun provider is disabled",
            ));
        }
        let network = effective_network(&context, &config)?;
        let http = network
            .configure_client(reqwest::Client::builder())
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
            .build()
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        let store = ProviderConfigStore::new(context.config_root.clone());
        let token = Arc::new(TokenStore::new(store, aliyun_id(), &config, http.clone())?);
        let provider = AliyunProvider {
            identity: ProviderIdentity::new(aliyun_id(), ProfileId::new("default").unwrap()),
            http,
            api_base: config.api_base.trim_end_matches('/').to_owned(),
            token,
            config,
            drives: BTreeMap::new(),
            sources: Vec::new(),
        };
        let mut provider = provider;
        provider.refresh_drive_info().await?;
        provider.sources = provider.source_configs()?;
        Ok(Arc::new(provider))
    }
}

pub struct AliyunProvider {
    identity: ProviderIdentity,
    http: reqwest::Client,
    api_base: String,
    token: Arc<TokenStore>,
    config: AliyunProviderConfig,
    drives: BTreeMap<String, String>,
    sources: Vec<SourceConfig>,
}

#[async_trait]
impl CloudProvider for AliyunProvider {
    fn identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            ranged_downloads: true,
            checksums: true,
        }
    }

    fn operations(&self) -> crate::provider::ProviderOperations {
        crate::provider::ProviderOperations {
            list: true,
            download: true,
            user_info: true,
            drive_info: true,
            file_info: true,
            ..crate::provider::ProviderOperations::default()
        }
    }

    async fn sources(&self) -> ProviderResult<Vec<RemoteSource>> {
        self.sources
            .iter()
            .map(|source| {
                let drive_id = self.drive_id(&source.drive)?;
                Ok(RemoteSource {
                    id: source.id.clone(),
                    name: source.name.clone(),
                    root: self.remote_item(
                        source.id.clone(),
                        AliyunObjectRef {
                            drive_id,
                            file_id: source.root_file_id.clone(),
                        },
                        source.name.clone(),
                        RemoteItemKind::Folder,
                        None,
                        None,
                    )?,
                })
            })
            .collect()
    }

    async fn list_children(&self, parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
        let object = self.validate_ref(parent)?;
        let mut marker = None;
        let mut items = Vec::new();
        loop {
            let response: ListResponse = self
                .post_api(
                    "/adrive/v1.0/openFile/list",
                    &serde_json::json!({
                        "drive_id": object.drive_id,
                        "parent_file_id": object.file_id,
                        "limit": 100,
                        "marker": marker,
                    }),
                )
                .await?;
            let next = response.next_marker.clone();
            items.extend(
                response
                    .items
                    .into_iter()
                    .map(|item| self.map_file(parent.source_id.clone(), item))
                    .collect::<ProviderResult<Vec<_>>>()?,
            );
            match next.filter(|value| !value.is_empty()) {
                Some(next) => marker = Some(next),
                None => break,
            }
        }
        items.sort_by(|left, right| {
            (
                !left.is_folder(),
                left.name.to_lowercase(),
                left.reference.object_id.clone(),
            )
                .cmp(&(
                    !right.is_folder(),
                    right.name.to_lowercase(),
                    right.reference.object_id.clone(),
                ))
        });
        Ok(items)
    }

    async fn refresh_item(&self, reference: &RemoteRef) -> ProviderResult<RemoteItem> {
        let object = self.validate_ref(reference)?;
        let response: FileResponse = self
            .post_api(
                "/adrive/v1.0/openFile/get",
                &serde_json::json!({
                    "drive_id": object.drive_id,
                    "file_id": object.file_id,
                }),
            )
            .await?;
        self.map_file(reference.source_id.clone(), response)
    }

    async fn open_download(
        &self,
        item: &RemoteItem,
        offset: u64,
    ) -> ProviderResult<RemoteDownload> {
        let object = self.validate_ref(&item.reference)?;
        let mut refreshed_url = false;
        loop {
            let download: DownloadUrlResponse = self
                .post_api(
                    "/adrive/v1.0/openFile/getDownloadUrl",
                    &serde_json::json!({
                        "drive_id": object.drive_id,
                        "file_id": object.file_id,
                        "expire_sec": 900,
                    }),
                )
                .await?;
            let mut request = self
                .http
                .get(download.url)
                .header(reqwest::header::REFERER, "https://www.alipan.com/");
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let response = request
                .send()
                .await
                .map_err(|error| download_transport_error(&error))?;
            if matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
            ) && !refreshed_url
            {
                refreshed_url = true;
                continue;
            }
            return normalize_download_response(item.size, offset, response).await;
        }
    }

    async fn user_info(&self) -> ProviderResult<ProviderUserInfo> {
        let response: UserInfoResponse = self.get_api("/oauth/users/info").await?;
        Ok(ProviderUserInfo {
            id: response.id,
            display_name: response.name,
            email: None,
        })
    }

    async fn drive_info(&self) -> ProviderResult<Vec<ProviderDriveInfo>> {
        let response: DriveInfoResponse = self
            .post_api("/adrive/v1.0/user/getDriveInfo", &serde_json::json!({}))
            .await?;
        Ok(self.drive_infos(&response))
    }
}

impl AliyunProvider {
    async fn refresh_drive_info(&mut self) -> ProviderResult<()> {
        let response: DriveInfoResponse = self
            .post_api("/adrive/v1.0/user/getDriveInfo", &serde_json::json!({}))
            .await?;
        self.drives = BTreeMap::from([
            ("default".to_owned(), response.default_drive_id.clone()),
            (
                "resource".to_owned(),
                response.resource_drive_id.clone().unwrap_or_default(),
            ),
            (
                "backup".to_owned(),
                response.backup_drive_id.clone().unwrap_or_default(),
            ),
        ]);
        Ok(())
    }

    fn drive_infos(&self, response: &DriveInfoResponse) -> Vec<ProviderDriveInfo> {
        [
            ("default", response.default_drive_id.clone()),
            (
                "resource",
                response.resource_drive_id.clone().unwrap_or_default(),
            ),
            (
                "backup",
                response.backup_drive_id.clone().unwrap_or_default(),
            ),
        ]
        .into_iter()
        .filter(|(_, id)| !id.is_empty())
        .map(|(kind, id)| ProviderDriveInfo {
            id,
            name: kind.to_owned(),
            kind: kind.to_owned(),
            total_bytes: None,
            used_bytes: None,
        })
        .collect()
    }

    fn source_configs(&self) -> ProviderResult<Vec<SourceConfig>> {
        let configured = if self.config.sources.is_empty() {
            vec![AliyunSourceConfig {
                id: format!("{}-{}", self.config.drive, self.config.root_file_id),
                name: self.config.source_name.clone(),
                drive: self.config.drive.clone(),
                root_file_id: self.config.root_file_id.clone(),
            }]
        } else {
            self.config.sources.clone()
        };
        configured
            .into_iter()
            .map(|source| {
                let source_name = source.name.clone();
                let id = SourceId::new(source.id.clone())
                    .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
                Ok(SourceConfig {
                    id,
                    name: source_name.unwrap_or_else(|| format!("Aliyun {}", source.id)),
                    drive: source.drive,
                    root_file_id: source.root_file_id,
                })
            })
            .collect()
    }

    fn drive_id(&self, kind: &str) -> ProviderResult<String> {
        self.drives
            .get(kind)
            .filter(|id| !id.is_empty())
            .cloned()
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Config,
                    format!("Aliyun drive kind is unavailable: {kind}"),
                )
            })
    }

    fn validate_ref(&self, reference: &RemoteRef) -> ProviderResult<AliyunObjectRef> {
        if reference.identity != self.identity {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "remote item belongs to a different provider instance",
            ));
        }
        serde_json::from_str(reference.object_id.as_str()).map_err(|error| {
            provider_error(
                ProviderErrorKind::Internal,
                format!("invalid Aliyun object ID: {error}"),
            )
        })
    }

    fn remote_item(
        &self,
        source_id: SourceId,
        object: AliyunObjectRef,
        name: String,
        kind: RemoteItemKind,
        size: Option<u64>,
        checksum: Option<RemoteChecksum>,
    ) -> ProviderResult<RemoteItem> {
        let object_id = serde_json::to_string(&object)
            .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
        Ok(RemoteItem {
            reference: RemoteRef {
                identity: self.identity.clone(),
                source_id,
                object_id: RemoteObjectId::new(object_id)
                    .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?,
            },
            name,
            kind,
            size,
            checksum,
            download: DownloadCapability::Supported { resume: true },
        })
    }

    fn map_file(&self, source_id: SourceId, file: FileResponse) -> ProviderResult<RemoteItem> {
        let kind = if file.item_type == "folder" {
            RemoteItemKind::Folder
        } else {
            RemoteItemKind::File
        };
        let checksum = file.content_hash.and_then(|value| {
            (!value.is_empty()).then(|| match file.content_hash_name.as_deref() {
                Some("sha1") | Some("SHA-1") => RemoteChecksum::Sha1(value),
                Some("md5") | Some("MD5") => RemoteChecksum::Md5(value),
                _ => RemoteChecksum::Other {
                    algorithm: file
                        .content_hash_name
                        .unwrap_or_else(|| "unknown".to_owned()),
                    value,
                },
            })
        });
        self.remote_item(
            source_id,
            AliyunObjectRef {
                drive_id: file.drive_id,
                file_id: file.file_id,
            },
            file.name,
            kind,
            if kind == RemoteItemKind::File {
                file.size
            } else {
                None
            },
            checksum,
        )
    }

    async fn get_api<T: DeserializeOwned>(&self, path: &str) -> ProviderResult<T> {
        self.request_api(path, |token, client| client.get(token))
            .await
    }

    async fn post_api<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> ProviderResult<T> {
        let body = body.clone();
        self.request_api(path, |token, client| client.post(token).json(&body))
            .await
    }

    async fn request_api<T, F>(&self, path: &str, request: F) -> ProviderResult<T>
    where
        T: DeserializeOwned,
        F: Fn(String, reqwest::Client) -> reqwest::RequestBuilder,
    {
        let mut refreshed = false;
        let mut rate_retries = 0;
        loop {
            let access_token = self.token.access_token().await?;
            let response = request(format!("{}{}", self.api_base, path), self.http.clone())
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS && rate_retries < 2 {
                let delay = retry_after(&response).unwrap_or(Duration::from_millis(100));
                rate_retries += 1;
                tokio::time::sleep(delay.min(Duration::from_secs(30))).await;
                continue;
            }
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
            if !status.is_success() {
                let error = parse_api_error(status, &body);
                if !refreshed && is_expired_token_error(&error) {
                    self.token.force_refresh().await?;
                    refreshed = true;
                    continue;
                }
                return Err(error);
            }
            return serde_json::from_slice(&body)
                .map_err(|error| provider_error(ProviderErrorKind::Network, error));
        }
    }
}

async fn normalize_download_response(
    expected_size: Option<u64>,
    offset: u64,
    response: Response,
) -> ProviderResult<RemoteDownload> {
    let status = response.status();
    if status == StatusCode::RANGE_NOT_SATISFIABLE && expected_size == Some(offset) {
        return Ok(RemoteDownload::AlreadyComplete {
            expected_size: offset,
        });
    }
    if !status.is_success() {
        return Err(api_response_error(status, response).await);
    }
    let total_size = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range_total)
        .or(expected_size);
    let partial = status == StatusCode::PARTIAL_CONTENT && offset > 0;
    if partial {
        let start = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_start)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Network,
                    "Aliyun resume response did not contain a valid Content-Range",
                )
            })?;
        if start != offset {
            return Err(ProviderError::new(
                ProviderErrorKind::Network,
                "Aliyun resume response did not match the requested offset",
            ));
        }
    }
    let stream = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(|error| provider_error(ProviderErrorKind::Network, error)))
        .boxed();
    if partial {
        Ok(RemoteDownload::Partial {
            start: offset,
            stream,
            total_size,
        })
    } else {
        Ok(RemoteDownload::Full { stream, total_size })
    }
}

fn parse_content_range_start(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.parse().ok()
}

fn retry_after(response: &Response) -> Option<Duration> {
    let value = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    date.duration_since(std::time::SystemTime::now()).ok()
}

fn download_transport_error(error: &reqwest::Error) -> ProviderError {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection"
    } else if error.is_body() {
        "response body"
    } else {
        "transport"
    };
    ProviderError::new(
        ProviderErrorKind::Network,
        format!("Aliyun download request failed ({category})"),
    )
}

async fn api_response_error(status: StatusCode, response: Response) -> ProviderError {
    let body = response.bytes().await.unwrap_or_default();
    parse_api_error(status, &body)
}

fn parse_api_error(status: StatusCode, body: &[u8]) -> ProviderError {
    let parsed = serde_json::from_slice::<ApiErrorBody>(body).ok();
    let code = parsed
        .as_ref()
        .map(|error| error.code.as_str())
        .filter(|code| !code.is_empty())
        .unwrap_or("http_error");
    let kind = if status == StatusCode::TOO_MANY_REQUESTS || code == "TooManyRequests" {
        ProviderErrorKind::RateLimit
    } else if is_expired_token_code(code) || status == StatusCode::UNAUTHORIZED {
        ProviderErrorKind::Auth
    } else if status == StatusCode::FORBIDDEN {
        ProviderErrorKind::Permission
    } else if status == StatusCode::NOT_FOUND {
        ProviderErrorKind::NotFound
    } else if status.is_server_error() {
        ProviderErrorKind::Network
    } else {
        ProviderErrorKind::Internal
    };
    ProviderError::new(kind, format!("Aliyun API error {code}"))
}

fn is_expired_token_error(error: &ProviderError) -> bool {
    error.kind == ProviderErrorKind::Auth
}

fn is_expired_token_code(code: &str) -> bool {
    matches!(code, "AccessTokenInvalid" | "AccessTokenExpired" | "I400JD")
}

fn load_config(context: &ProviderContext) -> ProviderResult<Option<AliyunProviderConfig>> {
    let id = aliyun_id();
    let store = ProviderConfigStore::new(context.config_root.clone());
    let contents = if let Some(contents) = context.config_overrides.get(&id) {
        contents.clone()
    } else {
        let Some(contents) = store
            .read_config(&id)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
        else {
            return Ok(None);
        };
        contents
    };
    if contents.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(&contents).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Config,
            "Aliyun provider config is not UTF-8",
        )
    })?;
    let config: AliyunProviderConfig = toml::from_str(text).map_err(|error| {
        ProviderError::new(
            ProviderErrorKind::Config,
            format!("invalid Aliyun provider config: {error}"),
        )
    })?;
    if config.client_id.trim().is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Aliyun provider client_id cannot be blank",
        ));
    }
    if config.api_base.trim().is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Aliyun provider api_base cannot be blank",
        ));
    }
    let api_url = url::Url::parse(config.api_base.trim()).map_err(|error| {
        ProviderError::new(
            ProviderErrorKind::Config,
            format!("Aliyun provider api_base is invalid: {error}"),
        )
    })?;
    if !matches!(api_url.scheme(), "http" | "https") || api_url.host_str().is_none() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Aliyun provider api_base must be an absolute HTTP(S) URL",
        ));
    }
    if config.drive.trim().is_empty() || config.root_file_id.trim().is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Aliyun provider drive and root_file_id cannot be blank",
        ));
    }
    for source in &config.sources {
        if source.id.trim().is_empty()
            || source.drive.trim().is_empty()
            || source.root_file_id.trim().is_empty()
        {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Aliyun source id, drive and root_file_id cannot be blank",
            ));
        }
    }
    Ok(Some(config))
}

fn effective_network(
    context: &ProviderContext,
    config: &AliyunProviderConfig,
) -> ProviderResult<crate::settings::NetworkSettings> {
    let network = match config.proxy.as_deref() {
        None => context.network.clone(),
        Some(value) => crate::settings::NetworkSettings::from_proxy_input(value),
    };
    network
        .validate()
        .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
    Ok(network)
}

fn provider_error(kind: ProviderErrorKind, error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(kind, error.to_string())
}

fn default_true() -> bool {
    true
}

fn default_drive() -> String {
    "default".to_owned()
}

fn default_root() -> String {
    "root".to_owned()
}

fn default_api_base() -> String {
    DEFAULT_API_BASE.to_owned()
}

fn default_editable_config() -> AliyunProviderConfig {
    AliyunProviderConfig {
        enabled: true,
        client_id: String::new(),
        client_secret: None,
        refresh_token: None,
        drive: default_drive(),
        root_file_id: default_root(),
        source_name: None,
        proxy: None,
        api_base: default_api_base(),
        sources: Vec::new(),
    }
}

fn parse_bool_setting(key: &str, value: &str) -> ProviderResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ProviderError::new(
            ProviderErrorKind::Config,
            format!("{key} must be true or false"),
        )),
    }
}

fn default_expires_in() -> i64 {
    900
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use futures_util::StreamExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        task::JoinHandle,
    };

    use super::*;

    struct MockResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl MockResponse {
        fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
                body: body.as_bytes().to_vec(),
            }
        }

        fn bytes(status: u16, body: &[u8]) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_vec(),
            }
        }

        fn with_header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_owned(), value.to_owned()));
            self
        }
    }

    async fn mock_server(mut responses: Vec<MockResponse>) -> (String, JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        for response in &mut responses {
            let body = String::from_utf8_lossy(&response.body).replace("{{BASE}}", &base);
            response.body = body.into_bytes();
        }
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4)
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or_default();
                        if request.len() >= header_end + content_length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let reason = match response.status {
                    200 => "OK",
                    206 => "Partial Content",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    416 => "Range Not Satisfiable",
                    429 => "Too Many Requests",
                    _ => "Error",
                };
                let mut headers = format!(
                    "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    response.body.len()
                );
                for (name, value) in response.headers {
                    headers.push_str(&format!("{name}: {value}\r\n"));
                }
                headers.push_str("\r\n");
                socket.write_all(headers.as_bytes()).await.unwrap();
                socket.write_all(&response.body).await.unwrap();
            }
            requests
        });
        (base, task)
    }

    fn token_response(access: &str, refresh: Option<&str>) -> MockResponse {
        let refresh = refresh
            .map(|value| format!(r#", "refresh_token":"{value}""#))
            .unwrap_or_default();
        MockResponse::json(
            200,
            &format!(r#"{{"access_token":"{access}"{refresh},"expires_in":3600}}"#),
        )
    }

    fn drive_response() -> MockResponse {
        MockResponse::json(200, r#"{"default_drive_id":"drive-1"}"#)
    }

    async fn provider_for(
        base: &str,
        temporary: &tempfile::TempDir,
        sources: &str,
    ) -> Arc<dyn CloudProvider> {
        let config = format!(
            "enabled = true\nclient_id = \"client-1\"\nrefresh_token = \"refresh-1\"\napi_base = \"{base}\"\n{sources}"
        );
        let id = aliyun_id();
        let context = ProviderContext {
            config_root: temporary.path().to_owned(),
            network: Default::default(),
            config_overrides: BTreeMap::from([(id, config.into_bytes())]),
        };
        AliyunProviderFactory.create(context).await.unwrap()
    }

    #[test]
    fn opaque_object_ids_round_trip_and_content_ranges_parse() {
        let object = AliyunObjectRef {
            drive_id: "drive-1".to_owned(),
            file_id: "file-1".to_owned(),
        };
        let encoded = serde_json::to_string(&object).unwrap();
        let decoded: AliyunObjectRef = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.drive_id, "drive-1");
        assert_eq!(parse_content_range_start("bytes 10-19/100"), Some(10));
        assert_eq!(parse_content_range_total("bytes 10-19/100"), Some(100));
    }

    #[test]
    fn api_errors_are_classified_without_echoing_secrets() {
        let error = parse_api_error(
            StatusCode::UNAUTHORIZED,
            br#"{"code":"AccessTokenExpired","message":"secret-token"}"#,
        );
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(!error.message.contains("secret-token"));
    }

    #[test]
    fn provider_settings_mask_secrets_and_replace_stale_token_state() {
        let temporary = tempfile::tempdir().unwrap();
        let context = ProviderContext {
            config_root: temporary.path().to_owned(),
            network: Default::default(),
            config_overrides: BTreeMap::new(),
        };
        let store = ProviderConfigStore::new(temporary.path().to_owned());
        store
            .write_private_file(
                &aliyun_id(),
                TOKEN_STATE_FILE,
                br#"{"access_token":"old","refresh_token":"old-refresh","expires_at_unix":1}"#,
            )
            .unwrap();
        let factory = AliyunProviderFactory;
        factory
            .save_settings(
                &context,
                &BTreeMap::from([
                    ("client_id".to_owned(), "client-1".to_owned()),
                    ("refresh_token".to_owned(), "new-refresh".to_owned()),
                ]),
            )
            .unwrap();
        assert!(
            store
                .read_private_file(&aliyun_id(), TOKEN_STATE_FILE)
                .unwrap()
                .is_none()
        );
        let snapshot = factory.load_settings(&context).unwrap();
        assert!(snapshot.configured_secrets.contains("refresh_token"));
        assert_eq!(snapshot.values.get("refresh_token").unwrap(), "");
        let saved = fs::read_to_string(store.config_path(&aliyun_id())).unwrap();
        assert!(saved.contains("new-refresh"));
        assert!(!format!("{snapshot:?}").contains("new-refresh"));
    }

    #[tokio::test]
    async fn oauth_rotation_is_persisted_atomically_with_private_permissions() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-2")),
            drive_response(),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(&base, &temporary, "").await;
        assert_eq!(provider.identity().provider_id, aliyun_id());
        let path = temporary.path().join("providers/aliyun/token.json");
        let state: StoredToken = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(state.refresh_token, "refresh-2");
        assert_eq!(state.access_token, "access-1");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let requests = server.await.unwrap();
        assert!(requests[0].contains("refresh_token=refresh-1"));
    }

    #[tokio::test]
    async fn expired_access_token_refreshes_once_and_does_not_log_secrets() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-1")),
            drive_response(),
            MockResponse::json(401, r#"{"code":"AccessTokenExpired","message":"access-1"}"#),
            token_response("access-2", Some("refresh-2")),
            MockResponse::json(200, r#"{"id":"user-1","name":"Test User"}"#),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(&base, &temporary, "").await;
        let user = provider.user_info().await.unwrap();
        assert_eq!(user.id, "user-1");
        let state =
            fs::read_to_string(temporary.path().join("providers/aliyun/token.json")).unwrap();
        assert!(state.contains("refresh-2"));
        assert!(!user.display_name.contains("access-1"));
        let requests = server.await.unwrap();
        assert!(requests[2].contains("authorization: Bearer access-1"));
        assert!(requests[4].contains("authorization: Bearer access-2"));
    }

    #[tokio::test]
    async fn rate_limit_honors_retry_after_and_paginates_sources() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-1")),
            drive_response(),
            MockResponse::json(429, r#"{"code":"TooManyRequests"}"#)
                .with_header("Retry-After", "0"),
            MockResponse::json(200, r#"{"id":"user-1","name":"Test User"}"#),
            MockResponse::json(
                200,
                r#"{"items":[{"drive_id":"drive-1","file_id":"folder-1","name":"Folder","type":"folder"}],"next_marker":"next-1"}"#,
            ),
            MockResponse::json(
                200,
                r#"{"items":[{"drive_id":"drive-1","file_id":"file-1","name":"file.bin","type":"file","size":3,"content_hash":"abc","content_hash_name":"sha1"}]}"#,
            ),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(
            &base,
            &temporary,
            "[[sources]]\nid = \"source-a\"\nname = \"Archive\"\ndrive = \"default\"\nroot_file_id = \"root\"\n",
        )
        .await;
        let user = provider.user_info().await.unwrap();
        assert_eq!(user.id, "user-1");
        let source = provider.sources().await.unwrap().remove(0);
        let items = provider
            .list_children(&source.root.reference)
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_folder());
        assert_eq!(
            items[1].checksum,
            Some(RemoteChecksum::Sha1("abc".to_owned()))
        );
        let requests = server.await.unwrap();
        assert!(requests[5].contains(r#""marker":"next-1""#));
    }

    #[tokio::test]
    async fn file_metadata_maps_size_checksum_and_identity() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-1")),
            drive_response(),
            MockResponse::json(
                200,
                r#"{"drive_id":"drive-1","file_id":"file-1","name":"archive.bin","type":"file","size":42,"content_hash":"abc","content_hash_name":"sha1"}"#,
            ),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(&base, &temporary, "").await;
        let source = provider.sources().await.unwrap().remove(0);
        let object_id = RemoteObjectId::new(
            serde_json::to_string(&AliyunObjectRef {
                drive_id: "drive-1".to_owned(),
                file_id: "file-1".to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let reference = RemoteRef {
            identity: provider.identity().clone(),
            source_id: source.id,
            object_id,
        };
        let item = provider.refresh_item(&reference).await.unwrap();
        assert_eq!(item.name, "archive.bin");
        assert_eq!(item.size, Some(42));
        assert_eq!(item.checksum, Some(RemoteChecksum::Sha1("abc".to_owned())));
        assert_eq!(item.reference, reference);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_maps_full_partial_and_already_complete_responses() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-1")),
            drive_response(),
            MockResponse::json(200, r#"{"url":"{{BASE}}/download-full"}"#),
            MockResponse::bytes(200, b"abc"),
            MockResponse::json(200, r#"{"url":"{{BASE}}/download-partial"}"#),
            MockResponse::bytes(206, b"cde").with_header("Content-Range", "bytes 2-4/5"),
            MockResponse::json(200, r#"{"url":"{{BASE}}/download-complete"}"#),
            MockResponse::bytes(416, b"").with_header("Content-Range", "bytes */5"),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(&base, &temporary, "").await;
        let source = provider.sources().await.unwrap().remove(0);
        let item = RemoteItem {
            reference: source.root.reference,
            name: "file.bin".to_owned(),
            kind: RemoteItemKind::File,
            size: Some(5),
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        };
        let full = provider.open_download(&item, 0).await.unwrap();
        assert!(matches!(
            &full,
            RemoteDownload::Full {
                total_size: Some(5),
                ..
            }
        ));
        if let RemoteDownload::Full { mut stream, .. } = full {
            assert_eq!(
                stream.next().await.unwrap().unwrap(),
                bytes::Bytes::from_static(b"abc")
            );
        }
        let partial = provider.open_download(&item, 2).await.unwrap();
        assert!(matches!(
            partial,
            RemoteDownload::Partial {
                start: 2,
                total_size: Some(5),
                ..
            }
        ));
        let complete = provider.open_download(&item, 5).await.unwrap();
        assert!(matches!(
            complete,
            RemoteDownload::AlreadyComplete { expected_size: 5 }
        ));
        let requests = server.await.unwrap();
        assert!(requests[5].to_ascii_lowercase().contains("range: bytes=2-"));
    }

    #[tokio::test]
    async fn expired_download_url_is_refetched_without_exposing_url() {
        let (base, server) = mock_server(vec![
            token_response("access-1", Some("refresh-1")),
            drive_response(),
            MockResponse::json(200, r#"{"url":"{{BASE}}/expired-secret-url"}"#),
            MockResponse::bytes(403, b"secret-url"),
            MockResponse::json(200, r#"{"url":"{{BASE}}/fresh-secret-url"}"#),
            MockResponse::bytes(200, b"ok"),
        ])
        .await;
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider_for(&base, &temporary, "").await;
        let source = provider.sources().await.unwrap().remove(0);
        let item = RemoteItem {
            reference: source.root.reference,
            name: "file.bin".to_owned(),
            kind: RemoteItemKind::File,
            size: Some(2),
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        };
        let download = provider.open_download(&item, 0).await.unwrap();
        let RemoteDownload::Full { mut stream, .. } = download else {
            panic!("expected full download")
        };
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"ok")
        );
        let requests = server.await.unwrap();
        assert!(requests[2].contains("/adrive/v1.0/openFile/getDownloadUrl"));
        assert!(requests[4].contains("/adrive/v1.0/openFile/getDownloadUrl"));
    }

    #[tokio::test]
    async fn download_transport_errors_do_not_expose_signed_urls() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let signed_url = format!("http://{address}/private?signature=super-secret");
        let raw_error = reqwest::Client::new()
            .get(&signed_url)
            .send()
            .await
            .unwrap_err();

        let error = download_transport_error(&raw_error).to_string();

        assert_eq!(error, "Aliyun download request failed (connection)");
        assert!(!error.contains("signature"));
        assert!(!error.contains("super-secret"));
        assert!(!error.contains(&address.to_string()));
    }
}
