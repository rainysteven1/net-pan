use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};

pub const DEFAULT_MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Error,
}

impl LogLevel {
    fn padded_name(self) -> &'static str {
        match self {
            Self::Info => "INFO    ",
            Self::Error => "ERROR   ",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    pub fn render(&self) -> String {
        format!(
            "{} | {} | {} - {}",
            self.timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            self.level.padded_name(),
            self.target,
            sanitize_message(&self.message)
        )
    }
}

pub struct SessionLogger {
    directory: PathBuf,
    file_prefix: String,
    part: u32,
    max_bytes: u64,
    written: u64,
    writer: BufWriter<File>,
    current_path: PathBuf,
}

impl SessionLogger {
    pub fn from_home() -> Result<Self> {
        Self::new(crate::paths::logs_root()?, DEFAULT_MAX_LOG_BYTES)
    }

    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self> {
        let now = Local::now();
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create log root {}", root.display()))?;
        restrict_directory(&root)?;
        let directory = root.join(now.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create log directory {}", directory.display()))?;
        restrict_directory(&directory)?;
        let file_prefix = format!("net-pan_{}", now.format("%H-%M-%S"));
        let (writer, current_path, part) = open_log_file(&directory, &file_prefix, 0)?;
        Ok(Self {
            directory,
            file_prefix,
            part,
            max_bytes: max_bytes.max(1),
            written: 0,
            writer,
            current_path,
        })
    }

    pub fn write(
        &mut self,
        level: LogLevel,
        target: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<LogEntry> {
        let entry = LogEntry {
            timestamp: Local::now(),
            level,
            target: target.into(),
            message: message.into(),
        };
        let mut line = entry.render();
        line.push('\n');
        let line_bytes = line.len() as u64;
        if self.written > 0 && self.written.saturating_add(line_bytes) > self.max_bytes {
            self.rotate()?;
        }
        self.writer
            .write_all(line.as_bytes())
            .with_context(|| format!("failed to write log file {}", self.current_path.display()))?;
        self.writer
            .flush()
            .with_context(|| format!("failed to flush log file {}", self.current_path.display()))?;
        self.written = self.written.saturating_add(line_bytes);
        Ok(entry)
    }

    pub fn current_path(&self) -> &Path {
        &self.current_path
    }

    fn rotate(&mut self) -> Result<()> {
        self.part = self.part.saturating_add(1);
        let (writer, path, part) = open_log_file(&self.directory, &self.file_prefix, self.part)?;
        self.writer = writer;
        self.current_path = path;
        self.part = part;
        self.written = 0;
        Ok(())
    }
}

fn open_log_file(
    directory: &Path,
    prefix: &str,
    first_part: u32,
) -> Result<(BufWriter<File>, PathBuf, u32)> {
    let mut part = first_part;
    loop {
        let path = directory.join(format!("{prefix}_{part:03}.log"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => {
                restrict_file(&path)?;
                return Ok((BufWriter::new(file), path, part));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                part = part.saturating_add(1);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create log file {}", path.display()));
            }
        }
    }
}

fn sanitize_message(message: &str) -> String {
    message.replace('\r', "\\r").replace('\n', "\\n")
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict log directory {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict log file {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_loguru_style_lines_and_rotates_by_size() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("logs");
        let mut logger = SessionLogger::new(root.clone(), 100).unwrap();
        logger
            .write(LogLevel::Info, "download", "completed path=model.bin")
            .unwrap();
        logger
            .write(
                LogLevel::Error,
                "download",
                "failed path=large.bin reason=network",
            )
            .unwrap();

        let date_directory = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        let files: Vec<_> = fs::read_dir(&date_directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 2);
        let content = files
            .iter()
            .map(|path| fs::read_to_string(path).unwrap())
            .collect::<String>();
        assert!(content.contains(" | INFO     | download - completed path=model.bin"));
        assert!(content.contains(" | ERROR    | download - failed path=large.bin"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&date_directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for path in files {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn writes_all_runtime_targets_to_one_session_file() {
        let temporary = tempfile::tempdir().unwrap();
        let mut logger = SessionLogger::new(temporary.path().join("logs"), 4096).unwrap();

        for target in [
            "runtime", "provider", "source", "daemon", "settings", "download",
        ] {
            logger
                .write(LogLevel::Info, target, format!("event target={target}"))
                .unwrap();
        }

        let content = fs::read_to_string(logger.current_path()).unwrap();
        for target in [
            "runtime", "provider", "source", "daemon", "settings", "download",
        ] {
            assert!(content.contains(&format!(" | INFO     | {target} - event target={target}")));
        }
    }
}
