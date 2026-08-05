use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{provider::ProviderId, settings::NetworkSettings};

const DEFAULT_LOG_FILE_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub download: DownloadSettings,
    #[serde(default)]
    pub network: NetworkSettings,
    #[serde(default)]
    pub logging: LoggingSettings,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DownloadSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_true")]
    pub resume: bool,
    #[serde(default)]
    pub overwrite: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            destination: None,
            concurrency: default_concurrency(),
            resume: true,
            overwrite: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct LoggingSettings {
    #[serde(default = "default_log_file_size")]
    pub max_file_size: u64,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            max_file_size: default_log_file_size(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkOverride {
    Inherit,
    Direct,
    Proxy(String),
}

impl NetworkOverride {
    pub fn effective(&self, global: &NetworkSettings) -> NetworkSettings {
        match self {
            Self::Inherit => global.clone(),
            Self::Direct => NetworkSettings::default(),
            Self::Proxy(proxy) => NetworkSettings::from_proxy_input(proxy),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read app config: {}", path.display()));
            }
        };

        let value: toml::Value = toml::from_str(&contents)
            .with_context(|| format!("invalid app config TOML: {}", path.display()))?;
        let config = if value.get("network").is_none() && value.get("proxy").is_some() {
            let legacy: NetworkSettings = toml::from_str(&contents).with_context(|| {
                format!("invalid legacy network config TOML: {}", path.display())
            })?;
            Self {
                network: legacy,
                ..Self::default()
            }
        } else {
            value
                .try_into()
                .with_context(|| format!("invalid structured app config: {}", path.display()))?
        };
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let contents = toml::to_string_pretty(self).context("failed to serialize app config")?;
        write_private_file(path, contents.as_bytes(), true)
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=5).contains(&self.download.concurrency) {
            bail!("download concurrency must be between 1 and 5");
        }
        if let Some(destination) = &self.download.destination
            && !destination.is_absolute()
        {
            bail!("download destination must be absolute");
        }
        if self.logging.max_file_size == 0 {
            bail!("logging max_file_size must be greater than zero");
        }
        self.network.validate()
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfigStore {
    root: PathBuf,
}

impl ProviderConfigStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn providers_dir(&self) -> PathBuf {
        self.root.join("providers")
    }

    pub fn config_path(&self, provider_id: &ProviderId) -> PathBuf {
        self.providers_dir()
            .join(format!("{}.toml", provider_id.as_str()))
    }

    pub fn private_dir(&self, provider_id: &ProviderId) -> PathBuf {
        self.providers_dir().join(provider_id.as_str())
    }

    pub fn read_config(&self, provider_id: &ProviderId) -> Result<Option<Vec<u8>>> {
        let path = self.config_path(provider_id);
        match fs::read(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read provider config: {}", path.display())),
        }
    }

    pub fn write_config(&self, provider_id: &ProviderId, contents: &[u8]) -> Result<()> {
        write_private_file(&self.config_path(provider_id), contents, true)
    }

    pub fn write_config_if_absent(
        &self,
        provider_id: &ProviderId,
        contents: &[u8],
    ) -> Result<bool> {
        let path = self.config_path(provider_id);
        if path.exists() {
            return Ok(false);
        }
        write_private_file(&path, contents, false)?;
        Ok(true)
    }

    pub fn read_private_file(
        &self,
        provider_id: &ProviderId,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        let path = self.private_file_path(provider_id, name)?;
        match fs::read(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read provider state: {}", path.display())),
        }
    }

    pub fn write_private_file(
        &self,
        provider_id: &ProviderId,
        name: &str,
        contents: &[u8],
    ) -> Result<()> {
        let path = self.private_file_path(provider_id, name)?;
        write_private_file(&path, contents, true)
    }

    pub fn write_private_file_atomic(
        &self,
        provider_id: &ProviderId,
        name: &str,
        contents: &[u8],
    ) -> Result<()> {
        let path = self.private_file_path(provider_id, name)?;
        let temporary_name = format!(
            "{name}.tmp-{}-{}",
            std::process::id(),
            NEXT_PRIVATE_FILE_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = self.private_file_path(provider_id, &temporary_name)?;
        write_private_file(&temporary, contents, true)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| {
                format!(
                    "failed to atomically replace private file: {}",
                    path.display()
                )
            });
        }
        sync_directory(
            path.parent()
                .expect("private file path always has a parent directory"),
        )?;
        Ok(())
    }

    pub fn remove_private_file(&self, provider_id: &ProviderId, name: &str) -> Result<bool> {
        let path = self.private_file_path(provider_id, name)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                sync_directory(
                    path.parent()
                        .expect("private file path always has a parent directory"),
                )?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove provider state: {}", path.display())),
        }
    }

    fn private_file_path(&self, provider_id: &ProviderId, name: &str) -> Result<PathBuf> {
        let candidate = Path::new(name);
        if candidate.components().count() != 1
            || candidate.file_name() != Some(OsStr::new(name))
            || matches!(name, "" | "." | "..")
        {
            bail!("provider private filename must be a single safe path component");
        }
        Ok(self.private_dir(provider_id).join(candidate))
    }
}

static NEXT_PRIVATE_FILE_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyGoogleConfig {
    pub credentials_path: PathBuf,
    pub sources_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyMigrationOutcome {
    Migrated(PathBuf),
    AlreadyConfigured(PathBuf),
    LegacyFilesMissing,
}

impl LegacyGoogleConfig {
    pub fn discover(store: &ProviderConfigStore) -> Option<Self> {
        let google = ProviderId::new("google").expect("static provider id is valid");
        if store.config_path(&google).exists() {
            return None;
        }
        let roots = [
            store.root().to_path_buf(),
            store
                .root()
                .parent()
                .map(|parent| parent.join("gdrive-juicefs"))
                .unwrap_or_default(),
        ];
        roots.into_iter().find_map(|root| {
            let credentials_path = root.join("service-account.json");
            let sources_path = root.join("sources.json");
            (credentials_path.is_file() && sources_path.is_file()).then_some(Self {
                credentials_path,
                sources_path,
            })
        })
    }

    pub fn migrate(store: &ProviderConfigStore) -> Result<LegacyMigrationOutcome> {
        let google = ProviderId::new("google").expect("static provider id is valid");
        let target = store.config_path(&google);
        if target.exists() {
            return Ok(LegacyMigrationOutcome::AlreadyConfigured(target));
        }
        let Some(legacy) = Self::discover(store) else {
            return Ok(LegacyMigrationOutcome::LegacyFilesMissing);
        };
        #[derive(Serialize)]
        struct MigratedGoogleConfig<'a> {
            enabled: bool,
            credentials_file: &'a Path,
            sources_file: &'a Path,
        }
        let contents = toml::to_string_pretty(&MigratedGoogleConfig {
            enabled: true,
            credentials_file: &legacy.credentials_path,
            sources_file: &legacy.sources_path,
        })
        .context("failed to serialize migrated Google provider config")?;
        let _: toml::Value = toml::from_str(&contents)
            .context("generated Google provider config is not valid TOML")?;
        if store.write_config_if_absent(&google, contents.as_bytes())? {
            Ok(LegacyMigrationOutcome::Migrated(target))
        } else {
            Ok(LegacyMigrationOutcome::AlreadyConfigured(target))
        }
    }
}

pub fn default_config_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; cannot locate config root")?;
    Ok(PathBuf::from(home).join(".config/pan-tui"))
}

fn write_private_file(path: &Path, contents: &[u8], truncate: bool) -> Result<()> {
    let parent = path.parent().context("private file path has no parent")?;
    create_private_dir(parent)?;
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(truncate);
    if !truncate {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open private file: {}", path.display()))?;
    restrict_file(&file, path)?;
    file.write_all(contents)
        .with_context(|| format!("failed to write private file: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync private file: {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("failed to open private directory: {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync private directory: {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create private directory: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to restrict private directory: {}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_file(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict private file: {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

const fn default_concurrency() -> usize {
    1
}

const fn default_log_file_size() -> u64 {
    DEFAULT_LOG_FILE_SIZE
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_config_round_trips_and_loads_legacy_proxy() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let config = AppConfig {
            download: DownloadSettings {
                destination: Some(PathBuf::from("/tmp/downloads")),
                concurrency: 3,
                resume: false,
                overwrite: true,
            },
            network: NetworkSettings::from_proxy_input("http://127.0.0.1:7890"),
            logging: LoggingSettings {
                max_file_size: 1024,
            },
        };
        config.save(&path).unwrap();
        assert_eq!(AppConfig::load(&path).unwrap(), config);

        fs::write(&path, "proxy = \"http://127.0.0.1:7891\"\n").unwrap();
        let legacy = AppConfig::load(&path).unwrap();
        assert_eq!(
            legacy.network.proxy.as_deref(),
            Some("http://127.0.0.1:7891")
        );
        assert_eq!(legacy.download, DownloadSettings::default());
    }

    #[test]
    fn app_config_rejects_invalid_global_values() {
        let mut config = AppConfig::default();
        config.download.concurrency = 0;
        assert!(config.validate().is_err());
        config.download.concurrency = 1;
        config.download.destination = Some(PathBuf::from("relative"));
        assert!(config.validate().is_err());
        config.download.destination = None;
        config.logging.max_file_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn provider_store_isolated_and_private() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::new(temporary.path().to_path_buf());
        let google = ProviderId::new("google").unwrap();
        let aliyun = ProviderId::new("aliyun").unwrap();
        store.write_config(&google, b"enabled = true\n").unwrap();
        store.write_config(&aliyun, b"enabled = false\n").unwrap();
        store
            .write_private_file(&google, "token.json", b"secret")
            .unwrap();

        assert_eq!(
            store.read_config(&google).unwrap().unwrap(),
            b"enabled = true\n"
        );
        assert_eq!(
            store.read_config(&aliyun).unwrap().unwrap(),
            b"enabled = false\n"
        );
        assert!(
            store
                .write_private_file(&google, "../escape", b"bad")
                .is_err()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.config_path(&google))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(store.private_dir(&google))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn write_if_absent_preserves_existing_provider_config() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::new(temporary.path().to_path_buf());
        let google = ProviderId::new("google").unwrap();
        assert!(
            store
                .write_config_if_absent(&google, b"version = 1\n")
                .unwrap()
        );
        assert!(
            !store
                .write_config_if_absent(&google, b"version = 2\n")
                .unwrap()
        );
        assert_eq!(
            store.read_config(&google).unwrap().unwrap(),
            b"version = 1\n"
        );
    }

    #[test]
    fn discovers_legacy_google_only_when_new_config_is_absent() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::new(temporary.path().to_path_buf());
        fs::write(temporary.path().join("service-account.json"), "{}").unwrap();
        fs::write(temporary.path().join("sources.json"), "{}").unwrap();
        assert!(LegacyGoogleConfig::discover(&store).is_some());
        store
            .write_config(&ProviderId::new("google").unwrap(), b"enabled = true\n")
            .unwrap();
        assert!(LegacyGoogleConfig::discover(&store).is_none());
    }

    #[test]
    fn discovers_pre_rename_project_config_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let pan_root = temporary.path().join("pan-tui");
        let old_root = temporary.path().join("gdrive-juicefs");
        fs::create_dir_all(&old_root).unwrap();
        fs::write(old_root.join("service-account.json"), "{}").unwrap();
        fs::write(old_root.join("sources.json"), "{}").unwrap();
        let store = ProviderConfigStore::new(pan_root);
        let legacy = LegacyGoogleConfig::discover(&store).unwrap();
        assert_eq!(
            legacy.credentials_path,
            old_root.join("service-account.json")
        );
        assert_eq!(legacy.sources_path, old_root.join("sources.json"));
    }

    #[test]
    fn explicit_legacy_migration_is_idempotent_and_preserves_old_files() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::new(temporary.path().to_path_buf());
        let credentials = temporary.path().join("service-account.json");
        let sources = temporary.path().join("sources.json");
        fs::write(&credentials, "{\"client_email\":\"test@example.com\"}").unwrap();
        fs::write(&sources, "{\"sources\":[]}").unwrap();

        let outcome = LegacyGoogleConfig::migrate(&store).unwrap();
        assert!(matches!(outcome, LegacyMigrationOutcome::Migrated(_)));
        let google = ProviderId::new("google").unwrap();
        let migrated = String::from_utf8(store.read_config(&google).unwrap().unwrap()).unwrap();
        assert!(migrated.contains("enabled = true"));
        assert!(migrated.contains(credentials.to_str().unwrap()));
        assert!(migrated.contains(sources.to_str().unwrap()));
        assert!(credentials.is_file());
        assert!(sources.is_file());

        let original = migrated.clone();
        assert!(matches!(
            LegacyGoogleConfig::migrate(&store).unwrap(),
            LegacyMigrationOutcome::AlreadyConfigured(_)
        ));
        assert_eq!(
            String::from_utf8(store.read_config(&google).unwrap().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn provider_network_override_is_explicit() {
        let global = NetworkSettings::from_proxy_input("http://127.0.0.1:7890");
        assert_eq!(NetworkOverride::Inherit.effective(&global), global);
        assert_eq!(
            NetworkOverride::Direct.effective(&global),
            NetworkSettings::default()
        );
        assert_eq!(
            NetworkOverride::Proxy("http://127.0.0.1:7891".to_owned())
                .effective(&global)
                .proxy
                .as_deref(),
            Some("http://127.0.0.1:7891")
        );
    }
}
