use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

impl NetworkSettings {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings: {}", path.display()));
            }
        };
        let contents = std::str::from_utf8(&contents)
            .with_context(|| format!("settings are not valid UTF-8: {}", path.display()))?;
        let mut settings: Self = toml::from_str(contents)
            .with_context(|| format!("invalid settings TOML: {}", path.display()))?;
        settings.proxy = normalize_proxy(settings.proxy.as_deref());
        settings.validate()?;
        Ok(settings)
    }

    pub fn from_proxy_input(value: &str) -> Self {
        Self {
            proxy: normalize_proxy(Some(value)),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let Some(proxy) = self.proxy.as_deref() else {
            return Ok(());
        };
        let url = Url::parse(proxy).context("proxy must be an absolute URL")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("proxy scheme must be http or https");
        }
        if url.host_str().is_none() {
            bail!("proxy URL must include a host");
        }
        reqwest::Proxy::all(proxy).context("invalid proxy URL")?;
        Ok(())
    }

    pub fn configure_client(&self, builder: ClientBuilder) -> Result<ClientBuilder> {
        self.validate()?;
        let builder = builder.no_proxy();
        match self.proxy.as_deref() {
            Some(proxy) => Ok(builder.proxy(
                reqwest::Proxy::all(proxy).context("failed to configure application proxy")?,
            )),
            None => Ok(builder),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .context("settings path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create settings directory: {}", parent.display())
        })?;
        let contents = toml::to_string_pretty(self).context("failed to serialize settings")?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("failed to open settings: {}", path.display()))?;
        restrict_file(&file, path)?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("failed to write settings: {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync settings: {}", path.display()))?;
        Ok(())
    }

    pub fn proxy_input(&self) -> &str {
        self.proxy.as_deref().unwrap_or_default()
    }
}

pub fn default_settings_path() -> Result<PathBuf> {
    crate::paths::settings_path()
}

fn normalize_proxy(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(unix)]
fn restrict_file(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!(
                "failed to restrict settings permissions: {}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn restrict_file(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_proxy_means_direct_and_settings_round_trip() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("config.toml");
        let direct = NetworkSettings::from_proxy_input("   ");
        assert_eq!(direct.proxy, None);
        direct.save(&path).unwrap();
        assert_eq!(NetworkSettings::load(&path).unwrap(), direct);

        let configured = NetworkSettings::from_proxy_input(" http://127.0.0.1:7890 ");
        configured.save(&path).unwrap();
        assert_eq!(NetworkSettings::load(&path).unwrap(), configured);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "proxy = \"http://127.0.0.1:7890\"\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rejects_relative_and_unsupported_proxy_urls() {
        for value in ["127.0.0.1:7890", "socks5://127.0.0.1:7890"] {
            assert!(NetworkSettings::from_proxy_input(value).validate().is_err());
        }
    }
}
