use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    provider::{ProfileId, ProviderId, ProviderIdentity},
    settings::NetworkSettings,
};

const DEFAULT_LOG_FILE_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub download: DownloadSettings,
    #[serde(default)]
    pub network: NetworkSettings,
    #[serde(default)]
    pub logging: LoggingSettings,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    Proxy(String),
}

impl NetworkOverride {
    pub fn effective(&self, global: &NetworkSettings) -> NetworkSettings {
        match self {
            Self::Inherit => global.clone(),
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

        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("invalid app config TOML: {}", path.display()))?;
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

pub fn validate_download_destination(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("download destination must be an absolute path");
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("download destination does not exist: {}", path.display());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect download destination: {}", path.display())
            });
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "download destination must not be a symbolic link: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "download destination is not a directory: {}",
            path.display()
        );
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ProviderConfigStore {
    root: PathBuf,
}

impl ProviderConfigStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn providers_dir(&self) -> PathBuf {
        self.root.join("providers")
    }

    pub fn provider_dir(&self, provider_id: &ProviderId) -> PathBuf {
        self.providers_dir().join(provider_id.as_str())
    }

    pub fn profile_dir(&self, identity: &ProviderIdentity) -> PathBuf {
        self.provider_dir(&identity.provider_id)
            .join(identity.profile_id.as_str())
    }

    pub fn config_path(&self, identity: &ProviderIdentity) -> PathBuf {
        self.profile_dir(identity).join("config.toml")
    }

    pub fn list_profiles(&self, provider_id: &ProviderId) -> Result<Vec<ProfileId>> {
        let root = self.provider_dir(provider_id);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to list Provider profiles: {}", root.display())
                });
            }
        };
        let mut profiles = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| {
                format!("failed to read Provider profile entry: {}", root.display())
            })?;
            if !entry
                .file_type()
                .with_context(|| format!("failed to inspect profile: {}", entry.path().display()))?
                .is_dir()
            {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if is_generated_profile_id(&name) {
                profiles.push(ProfileId::new(name).expect("generated ProfileId is non-blank"));
            }
        }
        profiles.sort();
        Ok(profiles)
    }

    pub fn create_profile(&self, provider_id: &ProviderId) -> Result<ProviderIdentity> {
        const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
        create_private_dir(&self.root)?;
        create_private_dir(&self.providers_dir())?;
        create_private_dir(&self.provider_dir(provider_id))?;
        for _ in 0..128 {
            let mut random = [0_u8; 8];
            getrandom::fill(&mut random)
                .map_err(|error| anyhow::anyhow!("failed to generate ProfileId: {error}"))?;
            let suffix: String = random
                .into_iter()
                .map(|value| ALPHABET[usize::from(value) % ALPHABET.len()] as char)
                .collect();
            let identity = ProviderIdentity::new(
                provider_id.clone(),
                ProfileId::new(format!("p_{suffix}")).expect("generated ProfileId is non-blank"),
            );
            let path = self.profile_dir(&identity);
            match fs::create_dir(&path) {
                Ok(()) => {
                    create_private_dir(&path)?;
                    return Ok(identity);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to create Provider profile: {}", path.display())
                    });
                }
            }
        }
        bail!("could not allocate a unique ProfileId after 128 attempts")
    }

    pub fn remove_profile(&self, identity: &ProviderIdentity) -> Result<()> {
        let path = self.profile_dir(identity);
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove Provider profile: {}", path.display())),
        }
    }

    pub fn read_config(&self, identity: &ProviderIdentity) -> Result<Option<Vec<u8>>> {
        let path = self.config_path(identity);
        match fs::read(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read provider config: {}", path.display())),
        }
    }

    pub fn write_config(&self, identity: &ProviderIdentity, contents: &[u8]) -> Result<()> {
        write_private_file(&self.config_path(identity), contents, true)
    }

    pub fn read_private_file(
        &self,
        identity: &ProviderIdentity,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        let path = self.private_file_path(identity, name)?;
        match fs::read(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read provider state: {}", path.display())),
        }
    }

    pub fn write_private_file(
        &self,
        identity: &ProviderIdentity,
        name: &str,
        contents: &[u8],
    ) -> Result<()> {
        let path = self.private_file_path(identity, name)?;
        write_private_file(&path, contents, true)
    }

    pub fn write_private_file_atomic(
        &self,
        identity: &ProviderIdentity,
        name: &str,
        contents: &[u8],
    ) -> Result<()> {
        let path = self.private_file_path(identity, name)?;
        let temporary_name = format!(
            "{name}.tmp-{}-{}",
            std::process::id(),
            NEXT_PRIVATE_FILE_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = self.private_file_path(identity, &temporary_name)?;
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

    pub fn remove_private_file(&self, identity: &ProviderIdentity, name: &str) -> Result<bool> {
        let path = self.private_file_path(identity, name)?;
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

    fn private_file_path(&self, identity: &ProviderIdentity, name: &str) -> Result<PathBuf> {
        let candidate = Path::new(name);
        if candidate.components().count() != 1
            || candidate.file_name() != Some(OsStr::new(name))
            || matches!(name, "" | "." | "..")
        {
            bail!("provider private filename must be a single safe path component");
        }
        Ok(self.profile_dir(identity).join(candidate))
    }
}

fn is_generated_profile_id(value: &str) -> bool {
    value.len() == 10
        && value.starts_with("p_")
        && value[2..]
            .bytes()
            .all(|byte| b"abcdefghjkmnpqrstuvwxyz23456789".contains(&byte))
}

static NEXT_PRIVATE_FILE_TEMP: AtomicU64 = AtomicU64::new(1);

pub fn default_config_root() -> Result<PathBuf> {
    crate::paths::app_root()
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
    fn app_config_round_trips_and_rejects_old_top_level_proxy() {
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
        assert!(AppConfig::load(&path).is_err());
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
    fn download_destination_must_be_an_existing_real_directory() {
        let temporary = tempfile::tempdir().unwrap();
        validate_download_destination(temporary.path()).unwrap();

        let relative = Path::new("downloads");
        assert!(validate_download_destination(relative).is_err());
        let missing = temporary.path().join("missing");
        assert!(validate_download_destination(&missing).is_err());
        let file = temporary.path().join("file.bin");
        fs::write(&file, b"data").unwrap();
        assert!(validate_download_destination(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn download_destination_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let link = temporary.path().join("link");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();
        assert!(validate_download_destination(&link).is_err());
    }

    #[test]
    fn provider_store_isolated_and_private() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProviderConfigStore::new(temporary.path().to_path_buf());
        let google = ProviderId::new("google").unwrap();
        let aliyun = ProviderId::new("aliyun").unwrap();
        let google_profile = store.create_profile(&google).unwrap();
        let second_google_profile = store.create_profile(&google).unwrap();
        let aliyun_profile = store.create_profile(&aliyun).unwrap();
        store
            .write_config(&google_profile, b"enabled = true\n")
            .unwrap();
        store
            .write_config(&aliyun_profile, b"enabled = false\n")
            .unwrap();
        store
            .write_private_file(&google_profile, "token.json", b"secret")
            .unwrap();
        store
            .write_config(&second_google_profile, b"enabled = false\n")
            .unwrap();

        assert_eq!(
            store.read_config(&google_profile).unwrap().unwrap(),
            b"enabled = true\n"
        );
        assert_eq!(
            store.read_config(&aliyun_profile).unwrap().unwrap(),
            b"enabled = false\n"
        );
        assert!(
            store
                .write_private_file(&google_profile, "../escape", b"bad")
                .is_err()
        );
        let google_profiles = store.list_profiles(&google).unwrap();
        assert_eq!(google_profiles.len(), 2);
        assert!(google_profiles.contains(&google_profile.profile_id));
        assert!(google_profiles.contains(&second_google_profile.profile_id));
        assert!(is_generated_profile_id(google_profile.profile_id.as_str()));
        assert!(is_generated_profile_id(
            second_google_profile.profile_id.as_str()
        ));
        assert_ne!(google_profile, second_google_profile);
        assert_ne!(
            store.config_path(&google_profile),
            store.config_path(&second_google_profile)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.config_path(&google_profile))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(store.profile_dir(&google_profile))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn provider_network_override_is_explicit() {
        let global = NetworkSettings::from_proxy_input("http://127.0.0.1:7890");
        assert_eq!(NetworkOverride::Inherit.effective(&global), global);
        assert_eq!(
            NetworkOverride::Proxy("http://127.0.0.1:7891".to_owned())
                .effective(&global)
                .proxy
                .as_deref(),
            Some("http://127.0.0.1:7891")
        );
    }
}
