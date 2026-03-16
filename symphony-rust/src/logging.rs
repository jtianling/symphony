use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::anyhow;
use tracing_appender::rolling::{Builder, RollingFileAppender, Rotation};

use crate::config::ObservabilityConfig;
use crate::error::SymphonyError;

const LOG_DIRECTORY_NAME: &str = "log";
const LOG_FILE_BASENAME: &str = "symphony";
const LOG_FILE_EXTENSION: &str = "log";

pub fn build_file_appender(
    observability: &ObservabilityConfig,
    logs_root: Option<&str>,
) -> Result<RollingFileAppender, SymphonyError> {
    let log_directory = resolve_log_directory(logs_root)?;
    fs::create_dir_all(&log_directory)
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))?;

    Builder::new()
        .rotation(Rotation::NEVER)
        .filename_prefix(LOG_FILE_BASENAME)
        .filename_suffix(LOG_FILE_EXTENSION)
        .max_bytes(observability.log_max_bytes)
        .max_log_files(usize::try_from(observability.log_max_files).unwrap_or(usize::MAX))
        .build(log_directory)
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))
}

pub fn resolve_log_directory(logs_root: Option<&str>) -> Result<PathBuf, SymphonyError> {
    let root = match logs_root.filter(|value| !value.trim().is_empty()) {
        Some(value) => PathBuf::from(value),
        None => env::current_dir().map_err(|error| {
            SymphonyError::Internal(anyhow!(format!(
                "failed to read current directory: {error}"
            )))
        })?,
    };

    Ok(root.join(LOG_DIRECTORY_NAME))
}

pub fn resolve_log_file_path(logs_root: Option<&str>) -> Result<PathBuf, SymphonyError> {
    Ok(resolve_log_directory(logs_root)?.join(format!("{LOG_FILE_BASENAME}.{LOG_FILE_EXTENSION}")))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{build_file_appender, resolve_log_file_path};
    use crate::config::ObservabilityConfig;

    #[test]
    fn build_file_appender_creates_log_directory_tree() {
        let directory = tempdir().unwrap();
        let logs_root = directory.path().join("nested").join("logs-root");
        let appender = build_file_appender(
            &ObservabilityConfig::default(),
            Some(logs_root.to_str().unwrap()),
        )
        .unwrap();
        drop(appender);

        assert!(logs_root.join("log").is_dir());
    }

    #[test]
    fn build_file_appender_respects_logs_root_override() {
        let directory = tempdir().unwrap();
        let logs_root = directory.path().join("custom-root");

        let appender = build_file_appender(
            &ObservabilityConfig::default(),
            Some(logs_root.to_str().unwrap()),
        )
        .unwrap();
        drop(appender);

        let expected_path = resolve_log_file_path(Some(logs_root.to_str().unwrap())).unwrap();

        assert!(expected_path.is_file());
    }
}
