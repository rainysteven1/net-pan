pub mod aliyun;
pub mod google;
mod manager;
mod registry;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    hash::Hash,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::settings::NetworkSettings;

pub use manager::{
    ProviderCatalogEntry, ProviderManager, ProviderRuntime, ProviderSettingsSaveResult,
    ProviderSlot, ProviderStatus,
};
pub use registry::{ProviderRegistry, RegistryError};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(InvalidId(stringify!($name)));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = InvalidId;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidId(&'static str);

impl fmt::Display for InvalidId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} cannot be blank", self.0)
    }
}

impl std::error::Error for InvalidId {}

string_id!(ProviderId);
string_id!(ProfileId);
string_id!(SourceId);
string_id!(RemoteObjectId);
string_id!(ProviderTaskId);

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ProviderIdentity {
    pub provider_id: ProviderId,
    pub profile_id: ProfileId,
}

impl ProviderIdentity {
    pub fn new(provider_id: ProviderId, profile_id: ProfileId) -> Self {
        Self {
            provider_id,
            profile_id,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct RemoteRef {
    pub identity: ProviderIdentity,
    pub source_id: SourceId,
    pub object_id: RemoteObjectId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RemoteItemKind {
    Folder,
    File,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RemoteChecksum {
    Md5(String),
    Sha1(String),
    Sha256(String),
    Other { algorithm: String, value: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DownloadCapability {
    Supported { resume: bool },
    Unsupported { reason: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteItem {
    pub reference: RemoteRef,
    pub name: String,
    pub kind: RemoteItemKind,
    pub size: Option<u64>,
    pub checksum: Option<RemoteChecksum>,
    pub download: DownloadCapability,
}

impl RemoteItem {
    pub fn is_folder(&self) -> bool {
        self.kind == RemoteItemKind::Folder
    }

    pub fn can_download(&self) -> bool {
        matches!(self.download, DownloadCapability::Supported { .. })
    }

    pub fn supports_resume(&self) -> bool {
        matches!(
            self.download,
            DownloadCapability::Supported { resume: true }
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSource {
    pub id: SourceId,
    pub name: String,
    pub root: RemoteItem,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilities {
    pub ranged_downloads: bool,
    pub checksums: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderOperations {
    pub list: bool,
    pub download: bool,
    pub user_info: bool,
    pub drive_info: bool,
    pub file_info: bool,
    pub create_folder: bool,
    pub upload: bool,
    pub delete: bool,
    pub tasks: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderUserInfo {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDriveInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderTaskState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderTask {
    pub id: ProviderTaskId,
    pub state: ProviderTaskState,
    pub progress_percent: Option<u8>,
    pub message: Option<String>,
}

pub struct UploadRequest {
    pub parent: RemoteRef,
    pub name: String,
    pub size: Option<u64>,
    pub checksum: Option<RemoteChecksum>,
    pub stream: RemoteByteStream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadResult {
    pub item: Option<RemoteItem>,
    pub task: Option<ProviderTaskId>,
}

pub enum ProviderLoginRequest {
    Interactive,
    RefreshToken {
        refresh_token: String,
    },
    AuthorizationCode {
        code: String,
        code_verifier: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAuthStatus {
    SignedOut,
    Ready,
    ReauthRequired,
    EnterpriseAppRequired,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderAuthState {
    pub authenticated: bool,
    pub user: Option<ProviderUserInfo>,
    pub expires_at_unix: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    Config,
    Auth,
    Permission,
    RateLimit,
    Network,
    NotFound,
    Unsupported,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: matches!(
                kind,
                ProviderErrorKind::RateLimit
                    | ProviderErrorKind::Network
                    | ProviderErrorKind::Internal
            ),
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

pub type ProviderResult<T> = Result<T, ProviderError>;
pub type RemoteByteStream = Pin<Box<dyn Stream<Item = ProviderResult<Bytes>> + Send + 'static>>;

pub enum RemoteDownload {
    Full {
        stream: RemoteByteStream,
        total_size: Option<u64>,
    },
    Partial {
        start: u64,
        stream: RemoteByteStream,
        total_size: Option<u64>,
    },
    AlreadyComplete {
        expected_size: u64,
    },
}

#[derive(Clone, Debug)]
pub struct ProviderContext {
    pub config_root: PathBuf,
    pub network: NetworkSettings,
    pub config_overrides: BTreeMap<ProviderId, Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderReadiness {
    Disabled,
    NeedsConfig,
    ReadyToConnect,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderSettingKind {
    Text,
    Secret,
    Boolean,
    Path,
    Action,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSettingSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ProviderSettingKind,
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSetting {
    pub key: String,
    pub label: String,
    pub kind: ProviderSettingKind,
    pub required: bool,
}

impl From<ProviderSettingSpec> for ProviderSetting {
    fn from(value: ProviderSettingSpec) -> Self {
        Self {
            key: value.key.to_owned(),
            label: value.label.to_owned(),
            kind: value.kind,
            required: value.required,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSettingsSnapshot {
    pub values: BTreeMap<String, String>,
    pub configured_secrets: BTreeSet<String>,
    pub read_only_reason: Option<String>,
}

#[async_trait]
pub trait ProviderFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &'static str;
    fn settings_schema(&self) -> Vec<ProviderSettingSpec> {
        Vec::new()
    }
    fn load_settings(
        &self,
        _context: &ProviderContext,
    ) -> ProviderResult<ProviderSettingsSnapshot> {
        Ok(ProviderSettingsSnapshot::default())
    }
    fn save_settings(
        &self,
        _context: &ProviderContext,
        _changes: &BTreeMap<String, String>,
    ) -> ProviderResult<()> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "this provider does not expose editable settings",
        ))
    }
    fn readiness(&self, context: &ProviderContext) -> ProviderReadiness;
    async fn create(&self, context: ProviderContext) -> ProviderResult<Arc<dyn CloudProvider>>;
    fn auth_status(&self, _context: &ProviderContext) -> ProviderResult<ProviderAuthStatus> {
        Ok(ProviderAuthStatus::Unsupported)
    }
    async fn login(
        &self,
        _context: &ProviderContext,
        _request: ProviderLoginRequest,
    ) -> ProviderResult<ProviderAuthState> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "this provider does not expose an interactive login flow",
        ))
    }
    async fn revoke(&self, _context: &ProviderContext) -> ProviderResult<()> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "this provider does not expose a revocation flow",
        ))
    }
}

#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn identity(&self) -> &ProviderIdentity;
    fn capabilities(&self) -> ProviderCapabilities;
    fn operations(&self) -> ProviderOperations {
        ProviderOperations {
            list: true,
            download: true,
            file_info: true,
            ..ProviderOperations::default()
        }
    }
    async fn sources(&self) -> ProviderResult<Vec<RemoteSource>>;
    async fn list_children(&self, parent: &RemoteRef) -> ProviderResult<Vec<RemoteItem>>;
    async fn refresh_item(&self, item: &RemoteRef) -> ProviderResult<RemoteItem>;
    async fn open_download(&self, item: &RemoteItem, offset: u64)
    -> ProviderResult<RemoteDownload>;
    async fn user_info(&self) -> ProviderResult<ProviderUserInfo> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "user information is not supported by this provider",
        ))
    }
    async fn drive_info(&self) -> ProviderResult<Vec<ProviderDriveInfo>> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "drive information is not supported by this provider",
        ))
    }
    async fn create_folder(&self, _parent: &RemoteRef, _name: &str) -> ProviderResult<RemoteItem> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "folder creation is not supported by this provider",
        ))
    }
    async fn upload_file(&self, _request: UploadRequest) -> ProviderResult<UploadResult> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "file upload is not supported by this provider",
        ))
    }
    async fn delete_item(&self, _item: &RemoteRef) -> ProviderResult<()> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "file deletion is not supported by this provider",
        ))
    }
    async fn task_status(&self, _task: &ProviderTaskId) -> ProviderResult<ProviderTask> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            "async task status is not supported by this provider",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_blank_values_and_round_trip() {
        assert!(ProviderId::new("  ").is_err());
        let id = ProviderId::new("google").unwrap();
        assert_eq!(id.as_str(), "google");
        assert_eq!(id.to_string(), "google");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"google\"");
    }

    #[test]
    fn remote_item_capabilities_are_provider_neutral() {
        let item = RemoteItem {
            reference: RemoteRef {
                identity: ProviderIdentity::new(
                    ProviderId::new("fake").unwrap(),
                    ProfileId::new("default").unwrap(),
                ),
                source_id: SourceId::new("source").unwrap(),
                object_id: RemoteObjectId::new("file-1").unwrap(),
            },
            name: "archive.bin".to_owned(),
            kind: RemoteItemKind::File,
            size: Some(42),
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        };
        assert!(!item.is_folder());
        assert!(item.can_download());
        assert!(item.supports_resume());
    }
}
