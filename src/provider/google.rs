use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{StatusCode, header::CONTENT_RANGE};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{
        CredentialStore, GoogleAuthorizationSession, GoogleInstalledTokenProvider,
        ServiceAccountTokenProvider,
    },
    config::{NetworkOverride, ProviderConfigStore},
    drive::{DriveClient, DriveReference, parse_drive_reference, response_error},
    model::DriveItem,
    provider::{
        CloudProvider, DownloadCapability, ProviderAuthState, ProviderAuthStatus,
        ProviderCapabilities, ProviderContext, ProviderError, ProviderErrorKind, ProviderFactory,
        ProviderId, ProviderIdentity, ProviderLoginRequest, ProviderReadiness, ProviderResult,
        ProviderSettingKind, ProviderSettingSpec, ProviderSettingsSnapshot, RemoteChecksum,
        RemoteDownload, RemoteItem, RemoteItemKind, RemoteObjectId, RemoteRef, RemoteSource,
        SourceId,
    },
    sources::parse_sources,
};

const PROVIDER_ID: &str = "google";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleProviderConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auth_mode: GoogleAuthMode,
    pub credentials_file: PathBuf,
    #[serde(default = "default_token_file")]
    pub token_file: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources_file: Option<PathBuf>,
    #[serde(default)]
    pub sources: Vec<GoogleSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoogleAuthMode {
    #[default]
    ServiceAccount,
    InstalledApp,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleSourceConfig {
    pub id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug)]
struct ResolvedGoogleSource {
    id: SourceId,
    name: Option<String>,
    reference: DriveReference,
}

pub struct GoogleProviderFactory;

#[async_trait]
impl ProviderFactory for GoogleProviderFactory {
    fn id(&self) -> ProviderId {
        google_id()
    }

    fn display_name(&self) -> &'static str {
        "Google Drive"
    }

    fn settings_schema(&self) -> Vec<ProviderSettingSpec> {
        vec![
            ProviderSettingSpec {
                key: "name",
                label: "Account name",
                description: "User-defined Account name shown in net-pan; changing it does not change the stable ProfileId or stored Jobs.",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "enabled",
                label: "Enabled",
                description: "Enables this Google Drive Provider; disable it to stop connecting without deleting its configuration.",
                kind: ProviderSettingKind::Boolean,
                required: true,
            },
            ProviderSettingSpec {
                key: "auth_mode",
                label: "Authentication mode",
                description: "Use installed_app for interactive user OAuth or service_account for a service-account credentials file.",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "credentials_file",
                label: "Credentials JSON",
                description: "Path to the Google OAuth desktop-client or service-account credentials JSON selected by Authentication mode.",
                kind: ProviderSettingKind::Path,
                required: true,
            },
            ProviderSettingSpec {
                key: "token_file",
                label: "OAuth token state",
                description: "Private Installed App OAuth token cache; refreshes are persisted atomically with 0600 permissions.",
                kind: ProviderSettingKind::Path,
                required: false,
            },
            ProviderSettingSpec {
                key: "sources_file",
                label: "Sources JSON",
                description: "JSON file containing saved Google Drive links shown in Files before a Source is opened.",
                kind: ProviderSettingKind::Path,
                required: false,
            },
            ProviderSettingSpec {
                key: "sources",
                label: "Saved folders",
                description: "Summary of configured links; select a saved link in Files and press Enter to open it.",
                kind: ProviderSettingKind::Action,
                required: true,
            },
            ProviderSettingSpec {
                key: "proxy",
                label: "Provider proxy",
                description: "Overrides the global application proxy for Google Drive; blank inherits it. System proxy environment variables are ignored.",
                kind: ProviderSettingKind::Text,
                required: false,
            },
        ]
    }

    fn load_settings(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
    ) -> ProviderResult<ProviderSettingsSnapshot> {
        let config = load_config(context, identity)?.unwrap_or_else(default_editable_config);
        let values = BTreeMap::from([
            (
                "name".to_owned(),
                if config.name.trim().is_empty() {
                    identity.profile_id.to_string()
                } else {
                    config.name
                },
            ),
            ("enabled".to_owned(), config.enabled.to_string()),
            (
                "auth_mode".to_owned(),
                match config.auth_mode {
                    GoogleAuthMode::ServiceAccount => "service_account",
                    GoogleAuthMode::InstalledApp => "installed_app",
                }
                .to_owned(),
            ),
            (
                "credentials_file".to_owned(),
                config.credentials_file.display().to_string(),
            ),
            (
                "token_file".to_owned(),
                config.token_file.display().to_string(),
            ),
            (
                "sources_file".to_owned(),
                config
                    .sources_file
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            ),
            (
                "sources".to_owned(),
                format!("{} inline source(s)", config.sources.len()),
            ),
            ("proxy".to_owned(), config.proxy.unwrap_or_default()),
        ]);
        Ok(ProviderSettingsSnapshot {
            values,
            configured_secrets: Default::default(),
            read_only_reason: None,
        })
    }

    fn save_settings(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
        changes: &BTreeMap<String, String>,
    ) -> ProviderResult<()> {
        let mut config = load_config(context, identity)?.unwrap_or_else(default_editable_config);
        if let Some(value) = changes.get("name") {
            config.name = validate_account_name(value)?;
        }
        if let Some(value) = changes.get("enabled") {
            config.enabled = parse_bool_setting("enabled", value)?;
        }
        if let Some(value) = changes.get("auth_mode") {
            config.auth_mode = parse_auth_mode(value)?;
        }
        if let Some(value) = changes.get("credentials_file") {
            if value.trim().is_empty() {
                return Err(ProviderError::new(
                    ProviderErrorKind::Config,
                    "Google credentials_file cannot be blank",
                ));
            }
            config.credentials_file = PathBuf::from(value.trim());
        }
        if let Some(value) = changes.get("sources_file") {
            config.sources_file = (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()));
        }
        if let Some(value) = changes.get("token_file") {
            if value.trim().is_empty() && config.auth_mode == GoogleAuthMode::InstalledApp {
                return Err(ProviderError::new(
                    ProviderErrorKind::Config,
                    "Google token_file cannot be blank in installed_app mode",
                ));
            }
            config.token_file = PathBuf::from(value.trim());
        }
        if let Some(value) = changes.get("proxy") {
            config.proxy = (!value.trim().is_empty()).then(|| value.trim().to_owned());
        }
        if config.sources_file.is_none() && config.sources.is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Google provider requires sources_file or at least one inline source",
            ));
        }
        let contents = toml::to_string_pretty(&config)
            .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
        ProviderConfigStore::new(context.config_root.clone())
            .write_config(identity, contents.as_bytes())
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))
    }

    fn readiness(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
    ) -> ProviderReadiness {
        match load_config(context, identity) {
            Ok(Some(config)) if !config.enabled => ProviderReadiness::Disabled,
            Ok(Some(_)) | Err(_) => ProviderReadiness::ReadyToConnect,
            Ok(None) => ProviderReadiness::NeedsConfig,
        }
    }

    async fn create(
        &self,
        context: ProviderContext,
        identity: ProviderIdentity,
    ) -> ProviderResult<Arc<dyn CloudProvider>> {
        let config = load_config(&context, &identity)?.ok_or_else(|| {
            ProviderError::new(ProviderErrorKind::Config, "Google Drive is not configured")
        })?;
        if !config.enabled {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "Google Drive provider is disabled",
            ));
        }

        let store = ProviderConfigStore::new(context.config_root.clone());
        let base = store.profile_dir(&identity);
        let credentials = resolve_path(&base, &config.credentials_file);
        let network_override = match config.proxy.as_deref() {
            None => NetworkOverride::Inherit,
            Some(value) if value.trim().is_empty() => NetworkOverride::Inherit,
            Some(value) => NetworkOverride::Proxy(value.to_owned()),
        };
        let network = network_override.effective(&context.network);
        network
            .validate()
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        let tokens = match config.auth_mode {
            GoogleAuthMode::ServiceAccount => {
                ServiceAccountTokenProvider::from_file(&credentials, &network).await
            }
            GoogleAuthMode::InstalledApp => {
                let token_path = resolve_path(&base, &config.token_file);
                GoogleInstalledTokenProvider::from_files(&credentials, token_path, &network).await
            }
        }
        .map_err(|error| provider_error(ProviderErrorKind::Auth, error))?;
        let drive = DriveClient::new(tokens, &network)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        let sources = resolve_sources(&base, &config).await?;

        Ok(Arc::new(GoogleProvider {
            identity,
            drive,
            sources,
        }))
    }

    fn auth_status(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
    ) -> ProviderResult<ProviderAuthStatus> {
        let Some(config) = load_config(context, identity)? else {
            return Ok(ProviderAuthStatus::SignedOut);
        };
        if config.auth_mode == GoogleAuthMode::ServiceAccount {
            return Ok(ProviderAuthStatus::Ready);
        }
        let base = ProviderConfigStore::new(context.config_root.clone()).profile_dir(identity);
        Ok(
            if CredentialStore::new(resolve_path(&base, &config.token_file)).exists() {
                ProviderAuthStatus::Ready
            } else {
                ProviderAuthStatus::SignedOut
            },
        )
    }

    async fn login(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
        request: ProviderLoginRequest,
    ) -> ProviderResult<ProviderAuthState> {
        if !matches!(request, ProviderLoginRequest::Interactive) {
            return Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                "Google only supports interactive login through this command",
            ));
        }
        let config = load_config(context, identity)?.ok_or_else(|| {
            ProviderError::new(ProviderErrorKind::Config, "Google Drive is not configured")
        })?;
        if config.auth_mode != GoogleAuthMode::InstalledApp {
            return Err(ProviderError::new(
                ProviderErrorKind::Config,
                "set Google auth_mode to installed_app before interactive login",
            ));
        }
        let base = ProviderConfigStore::new(context.config_root.clone()).profile_dir(identity);
        let network = match config.proxy.as_deref() {
            None => NetworkOverride::Inherit,
            Some(value) if value.trim().is_empty() => NetworkOverride::Inherit,
            Some(value) => NetworkOverride::Proxy(value.to_owned()),
        }
        .effective(&context.network);
        let session = GoogleAuthorizationSession::begin(
            &resolve_path(&base, &config.credentials_file),
            resolve_path(&base, &config.token_file),
            &network,
        )
        .await
        .map_err(|error| provider_error(ProviderErrorKind::Auth, error))?;
        eprintln!(
            "Open this URL to authorize Google Drive:\n{}",
            session.authorization_url
        );
        let expires_at_unix = session
            .complete()
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Auth, error))?;
        Ok(ProviderAuthState {
            authenticated: true,
            user: None,
            expires_at_unix: Some(expires_at_unix),
        })
    }

    async fn revoke(
        &self,
        context: &ProviderContext,
        identity: &ProviderIdentity,
    ) -> ProviderResult<()> {
        let config = load_config(context, identity)?.ok_or_else(|| {
            ProviderError::new(ProviderErrorKind::Config, "Google Drive is not configured")
        })?;
        if config.auth_mode != GoogleAuthMode::InstalledApp {
            return Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                "service-account profiles do not have a local OAuth token to revoke",
            ));
        }
        let base = ProviderConfigStore::new(context.config_root.clone()).profile_dir(identity);
        CredentialStore::new(resolve_path(&base, &config.token_file))
            .remove()
            .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct GoogleProvider {
    identity: ProviderIdentity,
    drive: DriveClient,
    sources: Vec<ResolvedGoogleSource>,
}

#[async_trait]
impl CloudProvider for GoogleProvider {
    fn identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            ranged_downloads: true,
            checksums: true,
        }
    }

    async fn sources(&self) -> ProviderResult<Vec<RemoteSource>> {
        let mut roots = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            let item = self
                .drive
                .get_item(&source.reference)
                .await
                .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
            if !item.is_folder() {
                return Err(ProviderError::new(
                    ProviderErrorKind::Config,
                    format!("configured Google source {} is not a folder", source.id),
                ));
            }
            let root = self.remote_item(source.id.clone(), item)?;
            roots.push(RemoteSource {
                id: source.id.clone(),
                name: source.name.clone().unwrap_or_else(|| root.name.clone()),
                root,
            });
        }
        roots.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(roots)
    }

    async fn list_children(&self, parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>> {
        self.validate_ref(parent)?;
        let folder = self
            .drive
            .get_item(&decode_reference(&parent.object_id)?)
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
        let children = self
            .drive
            .list_children(&folder)
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
        children
            .into_iter()
            .map(|item| self.remote_item(parent.source_id.clone(), item))
            .collect()
    }

    async fn refresh_item(&self, item: &RemoteRef) -> ProviderResult<RemoteItem> {
        self.validate_ref(item)?;
        let drive_item = self
            .drive
            .get_item(&decode_reference(&item.object_id)?)
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
        self.remote_item(item.source_id.clone(), drive_item)
    }

    async fn open_download(
        &self,
        item: &RemoteItem,
        offset: u64,
    ) -> ProviderResult<RemoteDownload> {
        self.validate_ref(&item.reference)?;
        if item.is_folder() || !item.can_download() {
            return Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                "Google item cannot be downloaded as media",
            ));
        }

        let response = self
            .drive
            .media_response_ref(&decode_reference(&item.reference.object_id)?, offset)
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Network, error))?;
        let status = response.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            if item.size == Some(offset) {
                return Ok(RemoteDownload::AlreadyComplete {
                    expected_size: offset,
                });
            }
            return Err(ProviderError::new(
                ProviderErrorKind::Network,
                "Google rejected the requested resume offset",
            ));
        }
        if !status.is_success() {
            return Err(provider_error(
                classify_status(status),
                response_error(response, "download Google Drive file").await,
            ));
        }

        if status == StatusCode::PARTIAL_CONTENT && offset > 0 {
            let actual = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(content_range_start);
            if actual != Some(offset) {
                return Err(ProviderError::new(
                    ProviderErrorKind::Network,
                    "Google resume response does not match the requested offset",
                ));
            }
        }
        let stream = response
            .bytes_stream()
            .map(|chunk| {
                chunk.map_err(|error| {
                    let detail = format!("{:#}", anyhow::Error::new(error));
                    provider_error(ProviderErrorKind::Network, detail)
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

impl GoogleProvider {
    fn validate_ref(&self, reference: &RemoteRef) -> ProviderResult<()> {
        if reference.identity != self.identity {
            return Err(ProviderError::new(
                ProviderErrorKind::Internal,
                "remote item belongs to a different provider instance",
            ));
        }
        Ok(())
    }

    fn remote_item(&self, source_id: SourceId, item: DriveItem) -> ProviderResult<RemoteItem> {
        let size = item.size_bytes();
        let kind = if item.is_folder() {
            RemoteItemKind::Folder
        } else {
            RemoteItemKind::File
        };
        let download = if item.is_google_native() {
            DownloadCapability::Unsupported {
                reason: "Google-native file requires export".to_owned(),
            }
        } else if !item.can_download() {
            DownloadCapability::Unsupported {
                reason: "the owner disabled downloads".to_owned(),
            }
        } else {
            DownloadCapability::Supported { resume: true }
        };
        Ok(RemoteItem {
            reference: RemoteRef {
                identity: self.identity.clone(),
                source_id,
                object_id: encode_reference(&DriveReference {
                    file_id: item.id,
                    resource_key: item.resource_key,
                })?,
            },
            name: item.name,
            kind,
            size,
            checksum: item.md5_checksum.map(RemoteChecksum::Md5),
            download,
        })
    }
}

fn load_config(
    context: &ProviderContext,
    identity: &ProviderIdentity,
) -> ProviderResult<Option<GoogleProviderConfig>> {
    let store = ProviderConfigStore::new(context.config_root.clone());
    if let Some(contents) = store
        .read_config(identity)
        .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
    {
        return parse_config(&contents).map(Some);
    }
    Ok(None)
}

fn parse_config(contents: &[u8]) -> ProviderResult<GoogleProviderConfig> {
    let text = std::str::from_utf8(contents).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Config,
            "Google provider config is not valid UTF-8",
        )
    })?;
    let config: GoogleProviderConfig = toml::from_str(text).map_err(|error| {
        ProviderError::new(
            ProviderErrorKind::Config,
            format!("invalid Google provider config: {error}"),
        )
    })?;
    if config.sources_file.is_none() && config.sources.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Google provider requires sources_file or at least one source",
        ));
    }
    Ok(config)
}

async fn resolve_sources(
    base: &Path,
    config: &GoogleProviderConfig,
) -> ProviderResult<Vec<ResolvedGoogleSource>> {
    let mut sources = Vec::new();
    if let Some(path) = &config.sources_file {
        let path = resolve_path(base, path);
        let contents = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?;
        for reference in parse_sources(&contents)
            .map_err(|error| provider_error(ProviderErrorKind::Config, error))?
        {
            sources.push(ResolvedGoogleSource {
                id: SourceId::new(reference.file_id.clone())
                    .map_err(|error| provider_error(ProviderErrorKind::Config, error))?,
                name: None,
                reference,
            });
        }
    }
    for source in &config.sources {
        sources.push(ResolvedGoogleSource {
            id: SourceId::new(source.id.clone())
                .map_err(|error| provider_error(ProviderErrorKind::Config, error))?,
            name: source.name.clone(),
            reference: parse_drive_reference(&source.url)
                .map_err(|error| provider_error(ProviderErrorKind::Config, error))?,
        });
    }
    if sources.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Google provider has no configured sources",
        ));
    }
    let mut seen = HashSet::new();
    sources.retain(|source| seen.insert(source.id.clone()));
    Ok(sources)
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn encode_reference(reference: &DriveReference) -> ProviderResult<RemoteObjectId> {
    let encoded = serde_json::to_string(reference)
        .map_err(|error| provider_error(ProviderErrorKind::Internal, error))?;
    RemoteObjectId::new(encoded).map_err(|error| provider_error(ProviderErrorKind::Internal, error))
}

fn decode_reference(object_id: &RemoteObjectId) -> ProviderResult<DriveReference> {
    serde_json::from_str(object_id.as_str()).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Internal,
            "invalid opaque Google object reference",
        )
    })
}

fn content_range_start(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

fn google_id() -> ProviderId {
    ProviderId::new(PROVIDER_ID).expect("static provider id is valid")
}

fn default_editable_config() -> GoogleProviderConfig {
    GoogleProviderConfig {
        name: String::new(),
        enabled: false,
        auth_mode: GoogleAuthMode::ServiceAccount,
        credentials_file: PathBuf::from("credentials.json"),
        token_file: default_token_file(),
        sources_file: Some(PathBuf::from("sources.json")),
        sources: Vec::new(),
        proxy: None,
    }
}

fn default_token_file() -> PathBuf {
    PathBuf::from("token.json")
}

fn validate_account_name(value: &str) -> ProviderResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Account name cannot be blank",
        ));
    }
    if value.chars().count() > 64 || value.chars().any(char::is_control) {
        return Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Account name must be at most 64 characters and contain no control characters",
        ));
    }
    Ok(value.to_owned())
}

fn parse_auth_mode(value: &str) -> ProviderResult<GoogleAuthMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "service_account" => Ok(GoogleAuthMode::ServiceAccount),
        "installed_app" => Ok(GoogleAuthMode::InstalledApp),
        _ => Err(ProviderError::new(
            ProviderErrorKind::Config,
            "Google auth_mode must be service_account or installed_app",
        )),
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

fn provider_error(kind: ProviderErrorKind, error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(kind, error.to_string())
}

fn classify_status(status: StatusCode) -> ProviderErrorKind {
    match status {
        StatusCode::UNAUTHORIZED => ProviderErrorKind::Auth,
        StatusCode::FORBIDDEN => ProviderErrorKind::Permission,
        StatusCode::NOT_FOUND => ProviderErrorKind::NotFound,
        StatusCode::TOO_MANY_REQUESTS => ProviderErrorKind::RateLimit,
        status if status.is_server_error() => ProviderErrorKind::Network,
        _ => ProviderErrorKind::Internal,
    }
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_context(root: &Path) -> ProviderContext {
        ProviderContext {
            config_root: root.to_owned(),
            network: Default::default(),
        }
    }

    #[test]
    fn opaque_drive_reference_round_trips() {
        let reference = DriveReference {
            file_id: "folder-123456789".to_owned(),
            resource_key: Some("secret".to_owned()),
        };
        let encoded = encode_reference(&reference).unwrap();
        assert_eq!(decode_reference(&encoded).unwrap(), reference);
    }

    #[test]
    fn parses_inline_and_file_backed_source_config() {
        let inline = parse_config(
            br#"
                credentials_file = "google/service-account.json"
                [[sources]]
                id = "project"
                url = "https://drive.google.com/drive/folders/folder-123456789"
            "#,
        )
        .unwrap();
        assert_eq!(inline.sources.len(), 1);
        assert!(inline.sources_file.is_none());

        let file_backed = parse_config(
            br#"
                credentials_file = "/tmp/service-account.json"
                sources_file = "/tmp/sources.json"
            "#,
        )
        .unwrap();
        assert_eq!(
            file_backed.sources_file.as_deref(),
            Some(Path::new("/tmp/sources.json"))
        );
    }

    #[test]
    fn rejects_config_without_sources() {
        assert!(parse_config(br#"credentials_file = "/tmp/key.json""#).is_err());
    }

    #[test]
    fn rejects_unknown_provider_config_fields() {
        assert!(
            parse_config(
                br#"
                    credentials_file = "/tmp/key.json"
                    sources_file = "/tmp/sources.json"
                    legacy_credentials = "/tmp/old.json"
                "#,
            )
            .is_err()
        );
    }

    #[test]
    fn parses_content_range_start() {
        assert_eq!(content_range_start("bytes 42-99/100"), Some(42));
        assert_eq!(content_range_start("invalid"), None);
    }

    #[test]
    fn provider_settings_round_trip_through_factory_owned_config() {
        let temporary = tempfile::tempdir().unwrap();
        let context = settings_context(temporary.path());
        let factory = GoogleProviderFactory;
        let identity = ProviderConfigStore::new(context.config_root.clone())
            .create_profile(&google_id())
            .unwrap();
        let snapshot = factory.load_settings(&context, &identity).unwrap();
        assert_eq!(
            snapshot.values.get("credentials_file").unwrap(),
            "credentials.json"
        );
        factory
            .save_settings(
                &context,
                &identity,
                &BTreeMap::from([
                    ("credentials_file".to_owned(), "test-key.json".to_owned()),
                    ("sources_file".to_owned(), "test-sources.json".to_owned()),
                    ("proxy".to_owned(), "http://127.0.0.1:7890".to_owned()),
                ]),
            )
            .unwrap();
        let saved = load_config(&context, &identity).unwrap().unwrap();
        assert_eq!(saved.credentials_file, PathBuf::from("test-key.json"));
        assert_eq!(saved.sources_file, Some(PathBuf::from("test-sources.json")));
        assert_eq!(saved.proxy.as_deref(), Some("http://127.0.0.1:7890"));
    }
}
