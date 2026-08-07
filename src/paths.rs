use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub const APP_DIR_NAME: &str = ".net-pan";
pub const SETTINGS_FILE_NAME: &str = "settings.toml";

pub fn app_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; cannot locate net-pan root")?;
    let root = PathBuf::from(home).join(APP_DIR_NAME);
    ensure_private_dir(&root)?;
    Ok(root)
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(app_root()?.join(SETTINGS_FILE_NAME))
}

pub fn state_root() -> Result<PathBuf> {
    Ok(app_root()?.join("state"))
}

pub fn logs_root() -> Result<PathBuf> {
    Ok(app_root()?.join("logs"))
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
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
