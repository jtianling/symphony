use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    NEVER,
}

#[derive(Debug, Clone)]
pub struct Builder {
    rotation: Rotation,
    filename_prefix: String,
    filename_suffix: Option<String>,
    max_bytes: u64,
    max_log_files: usize,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            rotation: Rotation::NEVER,
            filename_prefix: String::from("log"),
            filename_suffix: None,
            max_bytes: 10_485_760,
            max_log_files: 5,
        }
    }

    pub fn rotation(mut self, rotation: Rotation) -> Self {
        self.rotation = rotation;
        self
    }

    pub fn filename_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.filename_prefix = prefix.into();
        self
    }

    pub fn filename_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.filename_suffix = Some(suffix.into());
        self
    }

    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn max_log_files(mut self, max_log_files: usize) -> Self {
        self.max_log_files = max_log_files;
        self
    }

    pub fn build(self, directory: impl AsRef<Path>) -> io::Result<RollingFileAppender> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;

        let mut inner = Inner::new(
            directory,
            self.filename_prefix,
            self.filename_suffix,
            self.max_bytes.max(1),
            self.max_log_files.max(1),
            self.rotation,
        )?;
        inner.rotate_if_needed()?;

        Ok(RollingFileAppender {
            inner: Arc::new(Mutex::new(inner)),
        })
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct RollingFileAppender {
    inner: Arc<Mutex<Inner>>,
}

impl RollingFileAppender {
    pub fn path(&self) -> PathBuf {
        self.inner
            .lock()
            .map(|inner| inner.active_path.clone())
            .unwrap_or_default()
    }
}

impl Write for RollingFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("rolling file appender lock poisoned"))?;
        inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("rolling file appender lock poisoned"))?;
        inner.file.flush()
    }
}

#[derive(Debug)]
struct Inner {
    max_bytes: u64,
    max_log_files: usize,
    rotation: Rotation,
    active_path: PathBuf,
    file: File,
    current_size: u64,
}

impl Inner {
    fn new(
        directory: PathBuf,
        filename_prefix: String,
        filename_suffix: Option<String>,
        max_bytes: u64,
        max_log_files: usize,
        rotation: Rotation,
    ) -> io::Result<Self> {
        let active_path =
            build_active_path(&directory, &filename_prefix, filename_suffix.as_deref());
        let file = open_file(&active_path)?;
        let current_size = file.metadata()?.len();

        Ok(Self {
            max_bytes,
            max_log_files,
            rotation,
            active_path,
            file,
            current_size,
        })
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rotate_before_write(buf.len())?;
        let written = self.file.write(buf)?;
        self.current_size = self
            .current_size
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn rotate_if_needed(&mut self) -> io::Result<()> {
        if self.current_size >= self.max_bytes {
            self.rotate()?;
        }

        Ok(())
    }

    fn rotate_before_write(&mut self, incoming_len: usize) -> io::Result<()> {
        if self.rotation == Rotation::NEVER
            && self.current_size > 0
            && self.current_size.saturating_add(incoming_len as u64) > self.max_bytes
        {
            self.rotate()?;
        }

        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;

        let oldest_path = rotated_path(&self.active_path, self.max_log_files);
        if oldest_path.exists() {
            fs::remove_file(oldest_path)?;
        }

        for index in (1..self.max_log_files).rev() {
            let source = rotated_path(&self.active_path, index);
            let destination = rotated_path(&self.active_path, index + 1);
            if source.exists() {
                if destination.exists() {
                    fs::remove_file(&destination)?;
                }
                fs::rename(source, destination)?;
            }
        }

        if self.active_path.exists() && self.current_size > 0 {
            let first_rotated = rotated_path(&self.active_path, 1);
            if first_rotated.exists() {
                fs::remove_file(&first_rotated)?;
            }
            fs::rename(&self.active_path, first_rotated)?;
        }

        self.file = open_truncated_file(&self.active_path)?;
        self.current_size = 0;

        Ok(())
    }
}

fn build_active_path(directory: &Path, prefix: &str, suffix: Option<&str>) -> PathBuf {
    let file_name = match suffix {
        Some(suffix) if !suffix.is_empty() => format!("{prefix}.{suffix}"),
        _ => prefix.to_owned(),
    };

    directory.join(file_name)
}

fn rotated_path(active_path: &Path, index: usize) -> PathBuf {
    let file_name = active_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| format!("{value}.{index}"))
        .unwrap_or_else(|| format!("rotated.{index}"));

    active_path.with_file_name(file_name)
}

fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn open_truncated_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}
