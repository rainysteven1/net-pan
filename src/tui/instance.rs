use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};

use crate::paths::{ensure_private_dir, state_root};

const TUI_LOCK_FILE: &str = "tui.lock";

#[derive(Debug)]
pub struct TuiInstanceGuard {
    file: File,
}

impl TuiInstanceGuard {
    pub fn acquire_default() -> Result<Self> {
        let state = state_root()?;
        ensure_private_dir(&state)?;
        Self::acquire(&state.join(TUI_LOCK_FILE))
    }

    fn acquire(path: &Path) -> Result<Self> {
        reject_non_file_lock_path(path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("failed to open TUI instance lock: {}", path.display()))?;
        restrict_lock_file(path)?;

        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let owner = fs::read_to_string(path).unwrap_or_default();
                let owner = owner.trim();
                if owner.is_empty() {
                    bail!("another net-pan TUI is already running");
                }
                bail!("another net-pan TUI is already running (PID {owner})");
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("failed to lock TUI instance file: {}", path.display())
                });
            }
        }

        file.set_len(0)
            .context("failed to reset TUI instance lock contents")?;
        writeln!(file, "{}", std::process::id()).context("failed to record the TUI process ID")?;
        file.sync_data()
            .context("failed to persist the TUI process ID")?;
        Ok(Self { file })
    }
}

impl Drop for TuiInstanceGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn reject_non_file_lock_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect TUI lock: {}", path.display()));
        }
    };
    if !metadata.file_type().is_file() {
        bail!("TUI lock path is not a regular file: {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_lock_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict TUI lock: {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_lock_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_one_tui_instance_and_releases_on_drop() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("tui.lock");

        let first = TuiInstanceGuard::acquire(&path).unwrap();
        let error = TuiInstanceGuard::acquire(&path).unwrap_err();
        assert!(error.to_string().contains("already running"));

        drop(first);
        TuiInstanceGuard::acquire(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creates_a_private_lock_file() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("tui.lock");
        let _guard = TuiInstanceGuard::acquire(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn refuses_a_symbolic_link_lock_path() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        fs::write(&target, "not a lock").unwrap();
        let path = temporary.path().join("tui.lock");

        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, &path).unwrap();

        let error = TuiInstanceGuard::acquire(&path).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }
}
