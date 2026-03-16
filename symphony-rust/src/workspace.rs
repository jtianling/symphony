use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tracing::warn;

use crate::config::HooksConfig;
use crate::domain::sanitize_workspace_key;
use crate::error::SymphonyError;
use crate::ssh;

const REMOTE_WORKSPACE_MARKER: &str = "__SYMPHONY_WORKSPACE__";

#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    root: PathBuf,
    root_raw: String,
    hooks: HooksConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub path: PathBuf,
    pub created_now: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    AfterCreate,
    BeforeRun,
    AfterRun,
    BeforeRemove,
}

impl WorkspaceManager {
    pub fn new(root: PathBuf, hooks: HooksConfig) -> Result<Self, SymphonyError> {
        let root_raw = root.to_string_lossy().into_owned();
        let root = absolute_normalized_path(&root)?;
        Ok(Self {
            root,
            root_raw,
            hooks,
        })
    }

    pub fn workspace_path(&self, identifier: &str) -> PathBuf {
        self.root.join(sanitize_workspace_key(identifier))
    }

    pub async fn ensure_workspace(
        &self,
        identifier: &str,
        worker_host: Option<&str>,
    ) -> Result<WorkspaceInfo, SymphonyError> {
        match worker_host {
            Some(worker_host) => {
                let path = self.remote_workspace_path(identifier);
                validate_remote_workspace_path(&path)?;
                self.ensure_remote_workspace(worker_host, &path).await
            }
            None => self.ensure_local_workspace(identifier).await,
        }
    }

    async fn ensure_local_workspace(
        &self,
        identifier: &str,
    ) -> Result<WorkspaceInfo, SymphonyError> {
        let path = self.workspace_path(identifier);
        self.validate_containment(&path)?;

        let created_now = match tokio::fs::metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(SymphonyError::Workspace(format!(
                        "workspace path is not a directory: {}",
                        path.display()
                    )));
                }
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(|source| workspace_io_error("create workspace", &path, source))?;
                true
            }
            Err(error) => {
                return Err(workspace_io_error("inspect workspace", &path, error));
            }
        };

        self.validate_symlink_escape(&path)?;

        Ok(WorkspaceInfo { path, created_now })
    }

    async fn ensure_remote_workspace(
        &self,
        worker_host: &str,
        workspace: &str,
    ) -> Result<WorkspaceInfo, SymphonyError> {
        let script = [
            "set -eu".to_owned(),
            remote_shell_assign("workspace", workspace),
            "if [ -d \"$workspace\" ]; then".into(),
            "  created=0".into(),
            "elif [ -e \"$workspace\" ]; then".into(),
            "  rm -rf \"$workspace\"".into(),
            "  mkdir -p \"$workspace\"".into(),
            "  created=1".into(),
            "else".into(),
            "  mkdir -p \"$workspace\"".into(),
            "  created=1".into(),
            "fi".into(),
            "cd \"$workspace\"".into(),
            format!(
                "printf '%s\\t%s\\t%s\\n' '{}' \"$created\" \"$(pwd -P)\"",
                REMOTE_WORKSPACE_MARKER
            ),
        ]
        .join("\n");

        let (output, status) = self
            .run_remote_command(
                worker_host,
                &script,
                "prepare remote workspace",
                self.hooks.timeout_ms,
            )
            .await?;

        if status != 0 {
            return Err(SymphonyError::Workspace(format!(
                "prepare remote workspace failed on {worker_host} with exit code {status}: {}",
                output.trim()
            )));
        }

        parse_remote_workspace_output(&output)
    }

    pub async fn run_lifecycle_hooks(
        &self,
        workspace: &WorkspaceInfo,
        phase: HookPhase,
        worker_host: Option<&str>,
    ) -> Result<(), SymphonyError> {
        let script = match phase {
            HookPhase::AfterCreate if workspace.created_now => self.hooks.after_create.as_deref(),
            HookPhase::BeforeRun => self.hooks.before_run.as_deref(),
            HookPhase::AfterRun => self.hooks.after_run.as_deref(),
            HookPhase::BeforeRemove => self.hooks.before_remove.as_deref(),
            HookPhase::AfterCreate => None,
        };

        let Some(script) = script else {
            return Ok(());
        };

        let result = match worker_host {
            Some(worker_host) => {
                self.run_remote_hook(script, &workspace.path, worker_host, self.hooks.timeout_ms)
                    .await
            }
            None => {
                self.run_hook(script, &workspace.path, self.hooks.timeout_ms)
                    .await
            }
        };

        match phase {
            HookPhase::AfterCreate | HookPhase::BeforeRun => result,
            HookPhase::AfterRun | HookPhase::BeforeRemove => {
                if let Err(error) = result {
                    warn!(
                        phase = ?phase,
                        workspace = %workspace.path.display(),
                        error = %error,
                        "workspace lifecycle hook failed"
                    );
                }
                Ok(())
            }
        }
    }

    pub async fn cleanup_workspace(
        &self,
        identifier: &str,
        worker_host: Option<&str>,
    ) -> Result<(), SymphonyError> {
        match worker_host {
            Some(worker_host) => self.cleanup_remote_workspace(identifier, worker_host).await,
            None => self.cleanup_local_workspace(identifier).await,
        }
    }

    async fn cleanup_local_workspace(&self, identifier: &str) -> Result<(), SymphonyError> {
        let path = self.workspace_path(identifier);
        self.validate_containment(&path)?;

        match tokio::fs::metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(SymphonyError::Workspace(format!(
                        "workspace path is not a directory: {}",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(workspace_io_error("inspect workspace", &path, error));
            }
        }

        let workspace = WorkspaceInfo {
            path: path.clone(),
            created_now: false,
        };
        let _ = self
            .run_lifecycle_hooks(&workspace, HookPhase::BeforeRemove, None)
            .await;

        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|source| workspace_io_error("remove workspace", &path, source))?;

        Ok(())
    }

    async fn cleanup_remote_workspace(
        &self,
        identifier: &str,
        worker_host: &str,
    ) -> Result<(), SymphonyError> {
        let path = self.remote_workspace_path(identifier);
        validate_remote_workspace_path(&path)?;

        let workspace = WorkspaceInfo {
            path: PathBuf::from(&path),
            created_now: false,
        };
        let _ = self
            .run_lifecycle_hooks(&workspace, HookPhase::BeforeRemove, Some(worker_host))
            .await;

        let script = [
            remote_shell_assign("workspace", &path),
            "rm -rf \"$workspace\"".into(),
        ]
        .join("\n");
        let (output, status) = self
            .run_remote_command(
                worker_host,
                &script,
                "remove remote workspace",
                self.hooks.timeout_ms,
            )
            .await?;

        if status != 0 {
            return Err(SymphonyError::Workspace(format!(
                "remove remote workspace failed on {worker_host} with exit code {status}: {}",
                output.trim()
            )));
        }

        Ok(())
    }

    fn validate_containment(&self, workspace_path: &Path) -> Result<(), SymphonyError> {
        let canonical_root = resolve_path_for_containment(&self.root)?;
        let canonical_workspace = resolve_path_for_containment(workspace_path)?;

        if !canonical_workspace.starts_with(&canonical_root) {
            return Err(SymphonyError::Workspace(format!(
                "path outside workspace root: {}",
                workspace_path.display()
            )));
        }

        Ok(())
    }

    fn validate_symlink_escape(&self, workspace_path: &Path) -> Result<(), SymphonyError> {
        if !workspace_path.exists() {
            return Ok(());
        }

        let canonical = std::fs::canonicalize(workspace_path)
            .map_err(|source| workspace_io_error("canonicalize workspace", workspace_path, source))?;
        let canonical_root = std::fs::canonicalize(&self.root)
            .map_err(|source| workspace_io_error("canonicalize workspace root", &self.root, source))?;

        if !canonical.starts_with(&canonical_root) {
            return Err(SymphonyError::Workspace(format!(
                "symlink escape detected: {} resolves to {} which is outside workspace root {}",
                workspace_path.display(),
                canonical.display(),
                canonical_root.display(),
            )));
        }

        Ok(())
    }

    async fn run_hook(
        &self,
        script: &str,
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<(), SymphonyError> {
        let child = Command::new("bash")
            .args(["-lc", script])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| workspace_io_error("spawn hook", cwd, source))?;

        match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
            .await
        {
            Ok(Ok(output)) if output.status.success() => Ok(()),
            Ok(Ok(output)) => Err(SymphonyError::Workspace(format!(
                "hook failed with exit code: {:?}",
                output.status.code()
            ))),
            Ok(Err(error)) => Err(SymphonyError::Workspace(format!("hook error: {error}"))),
            Err(_) => Err(SymphonyError::Workspace("hook timed out".into())),
        }
    }

    async fn run_remote_hook(
        &self,
        script: &str,
        cwd: &Path,
        worker_host: &str,
        timeout_ms: u64,
    ) -> Result<(), SymphonyError> {
        let workspace = cwd.to_string_lossy().into_owned();
        let remote_script = [
            remote_shell_assign("workspace", &workspace),
            format!("cd \"$workspace\" && {script}"),
        ]
        .join("\n");

        let (output, status) = self
            .run_remote_command(worker_host, &remote_script, "workspace hook", timeout_ms)
            .await?;

        if status == 0 {
            return Ok(());
        }

        Err(SymphonyError::Workspace(format!(
            "hook failed on {worker_host} with exit code {status}: {}",
            output.trim()
        )))
    }

    async fn run_remote_command(
        &self,
        worker_host: &str,
        script: &str,
        action: &str,
        timeout_ms: u64,
    ) -> Result<(String, i32), SymphonyError> {
        match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            ssh::run(worker_host, script),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(SymphonyError::Workspace(format!(
                "{action} failed on {worker_host}: {error}"
            ))),
            Err(_) => Err(SymphonyError::Workspace(format!(
                "{action} timed out on {worker_host} after {timeout_ms}ms"
            ))),
        }
    }

    fn remote_workspace_path(&self, identifier: &str) -> String {
        Path::new(&self.root_raw)
            .join(sanitize_workspace_key(identifier))
            .to_string_lossy()
            .into_owned()
    }
}

pub fn default_workspace_root(configured_root: Option<&str>) -> PathBuf {
    configured_root
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("symphony_workspaces"))
}

fn absolute_normalized_path(path: &Path) -> Result<PathBuf, SymphonyError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| workspace_io_error("resolve current directory", path, source))?
            .join(path)
    };

    Ok(normalize_absolute_path(&absolute))
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut prefix = None;
    let mut has_root = false;
    let mut parts = Vec::<OsString>::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
            std::path::Component::RootDir => {
                has_root = true;
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = parts.pop();
            }
            std::path::Component::Normal(value) => {
                parts.push(value.to_os_string());
            }
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if has_root {
        normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    }
    for part in parts {
        normalized.push(part);
    }

    normalized
}

fn resolve_path_for_containment(path: &Path) -> Result<PathBuf, SymphonyError> {
    let absolute = absolute_normalized_path(path)?;
    let mut existing_ancestor = absolute.clone();
    let mut missing_suffix = Vec::<OsString>::new();

    while !existing_ancestor.exists() {
        let Some(name) = existing_ancestor.file_name() else {
            break;
        };
        missing_suffix.push(name.to_os_string());

        let Some(parent) = existing_ancestor.parent() else {
            break;
        };
        existing_ancestor = parent.to_path_buf();
    }

    let mut resolved = if existing_ancestor.exists() {
        std::fs::canonicalize(&existing_ancestor)
            .map_err(|source| workspace_io_error("canonicalize workspace path", path, source))?
    } else {
        existing_ancestor
    };

    for part in missing_suffix.iter().rev() {
        resolved.push(part);
    }

    Ok(normalize_absolute_path(&resolved))
}

fn workspace_io_error(action: &str, path: &Path, source: std::io::Error) -> SymphonyError {
    SymphonyError::Workspace(format!("{action} failed for {}: {source}", path.display()))
}

fn parse_remote_workspace_output(output: &str) -> Result<WorkspaceInfo, SymphonyError> {
    for line in output.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(marker) = parts.next() else {
            continue;
        };

        if marker != REMOTE_WORKSPACE_MARKER {
            continue;
        }

        let created = parts.next();
        let path = parts.next();

        return match (created, path) {
            (Some("0"), Some(path)) if !path.is_empty() => Ok(WorkspaceInfo {
                path: PathBuf::from(path),
                created_now: false,
            }),
            (Some("1"), Some(path)) if !path.is_empty() => Ok(WorkspaceInfo {
                path: PathBuf::from(path),
                created_now: true,
            }),
            _ => Err(SymphonyError::Workspace(format!(
                "invalid remote workspace output: {output}"
            ))),
        };
    }

    Err(SymphonyError::Workspace(format!(
        "missing remote workspace marker in output: {output}"
    )))
}

fn validate_remote_workspace_path(path: &str) -> Result<(), SymphonyError> {
    if path.trim().is_empty() {
        return Err(SymphonyError::Workspace(
            "remote workspace path must not be empty".into(),
        ));
    }

    if path.contains(['\n', '\r', '\0']) {
        return Err(SymphonyError::Workspace(
            "remote workspace path contains invalid characters".into(),
        ));
    }

    Ok(())
}

fn remote_shell_assign(variable_name: &str, raw_path: &str) -> String {
    [
        format!("{variable_name}={}", ssh::shell_escape(raw_path)),
        format!("case \"${variable_name}\" in"),
        format!("  '~') {variable_name}=\"$HOME\" ;;"),
        format!("  '~/'*) {variable_name}=\"$HOME/${{{variable_name}#~/}}\" ;;"),
        "esac".into(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{
        parse_remote_workspace_output, remote_shell_assign, validate_remote_workspace_path,
        HookPhase, WorkspaceManager, REMOTE_WORKSPACE_MARKER,
    };
    use crate::config::HooksConfig;
    use crate::error::SymphonyError;

    fn create_manager(root: &Path) -> WorkspaceManager {
        WorkspaceManager::new(root.to_path_buf(), HooksConfig::default())
            .expect("workspace manager should initialize")
    }

    #[test]
    // SPEC 17.2: workspace path is deterministic per issue identifier.
    fn workspace_path_uses_identifier() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let path = manager.workspace_path("ABC-123");

        assert_eq!(path, temp_dir.path().join("ABC-123"));
    }

    #[test]
    // SPEC 17.2: workspace keys are sanitized before path construction.
    fn workspace_path_sanitizes_special_characters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let path = manager.workspace_path("ABC/123:feature");

        assert_eq!(path, temp_dir.path().join("ABC_123_feature"));
    }

    #[tokio::test]
    // SPEC 17.2: missing workspace directories are created on demand.
    async fn ensure_workspace_creates_directory() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be created");

        assert!(workspace.created_now);
        assert!(workspace.path.is_dir());
    }

    #[tokio::test]
    // SPEC 17.2: existing workspace directories are reused.
    async fn ensure_workspace_reuses_existing_directory() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());
        let path = temp_dir.path().join("ABC-123");
        fs::create_dir_all(&path).expect("workspace dir should be created");

        let workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be reused");

        assert!(!workspace.created_now);
        assert_eq!(workspace.path, path);
    }

    #[test]
    // SPEC 17.2: root containment accepts in-root workspace paths.
    fn validate_containment_accepts_paths_inside_root() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let result = manager.validate_containment(&temp_dir.path().join("ABC-123"));

        assert!(result.is_ok());
    }

    #[test]
    fn validate_containment_rejects_paths_outside_root() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let result = manager.validate_containment(&temp_dir.path().join("../escape"));

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_symlink_escape_rejects_symlink_outside_root() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let outside_dir = TempDir::new().expect("outside dir should be created");
        let manager = create_manager(temp_dir.path());

        let symlink_path = temp_dir.path().join("escape-link");
        std::os::unix::fs::symlink(outside_dir.path(), &symlink_path)
            .expect("symlink should be created");

        let result = manager.validate_symlink_escape(&symlink_path);

        assert!(matches!(result, Err(SymphonyError::Workspace(ref msg)) if msg.contains("symlink escape")));
    }

    #[test]
    fn validate_symlink_escape_accepts_path_within_root() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());
        let workspace = temp_dir.path().join("valid-workspace");
        fs::create_dir_all(&workspace).expect("workspace should be created");

        let result = manager.validate_symlink_escape(&workspace);

        assert!(result.is_ok());
    }

    #[tokio::test]
    // SPEC 17.2: lifecycle hooks execute in the workspace cwd when configured.
    async fn run_hook_succeeds() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        manager
            .run_hook("touch hook-success.txt", temp_dir.path(), 1_000)
            .await
            .expect("hook should succeed");

        assert!(temp_dir.path().join("hook-success.txt").exists());
    }

    #[tokio::test]
    // SPEC 17.2: hook failures are surfaced for blocking phases.
    async fn run_hook_fails() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let result = manager.run_hook("exit 42", temp_dir.path(), 1_000).await;

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[tokio::test]
    // SPEC 17.2: `after_create` hooks only run for newly created workspaces.
    async fn after_create_hook_runs_only_for_new_workspace() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let hooks = HooksConfig {
            after_create: Some("touch after-create.txt".into()),
            ..HooksConfig::default()
        };
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks)
            .expect("workspace manager should initialize");

        let new_workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be created");
        manager
            .run_lifecycle_hooks(&new_workspace, HookPhase::AfterCreate, None)
            .await
            .expect("after_create should succeed");
        assert!(new_workspace.path.join("after-create.txt").exists());

        fs::remove_file(new_workspace.path.join("after-create.txt"))
            .expect("marker file should be removed");

        let existing_workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be reused");
        manager
            .run_lifecycle_hooks(&existing_workspace, HookPhase::AfterCreate, None)
            .await
            .expect("after_create should be skipped");

        assert!(!existing_workspace.path.join("after-create.txt").exists());
    }

    #[tokio::test]
    // SPEC 17.2: `before_remove` hooks run during cleanup and the workspace is deleted.
    async fn cleanup_workspace_runs_before_remove_and_deletes_directory() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path().to_path_buf();
        let workspace_path = root.join("ABC-123");
        fs::create_dir_all(&workspace_path).expect("workspace dir should be created");

        let marker_dir = root.join("markers");
        fs::create_dir_all(&marker_dir).expect("marker dir should be created");

        let hooks = HooksConfig {
            before_remove: Some(format!(
                "touch {}",
                marker_dir.join("removed.txt").display()
            )),
            ..HooksConfig::default()
        };
        let manager =
            WorkspaceManager::new(root, hooks).expect("workspace manager should initialize");

        manager
            .cleanup_workspace("ABC-123", None)
            .await
            .expect("workspace should be removed");

        assert!(!workspace_path.exists());
        assert!(marker_dir.join("removed.txt").exists());
    }

    #[test]
    fn parse_remote_workspace_output_accepts_new_workspace_marker() {
        let output = format!("noise\n{REMOTE_WORKSPACE_MARKER}\t1\t/remote/workspaces/ABC-123\n");

        let info =
            parse_remote_workspace_output(&output).expect("remote workspace output should parse");

        assert!(info.created_now);
        assert_eq!(info.path, Path::new("/remote/workspaces/ABC-123"));
    }

    #[test]
    fn parse_remote_workspace_output_accepts_existing_workspace_marker() {
        let output = format!("{REMOTE_WORKSPACE_MARKER}\t0\t/remote/workspaces/ABC-123\n");

        let info =
            parse_remote_workspace_output(&output).expect("remote workspace output should parse");

        assert!(!info.created_now);
        assert_eq!(info.path, Path::new("/remote/workspaces/ABC-123"));
    }

    #[test]
    fn parse_remote_workspace_output_rejects_missing_marker() {
        let result = parse_remote_workspace_output("plain output\nwithout marker\n");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn parse_remote_workspace_output_rejects_malformed_marker() {
        let output = format!("{REMOTE_WORKSPACE_MARKER}\t2\t/remote/workspaces/ABC-123\n");

        let result = parse_remote_workspace_output(&output);

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_remote_workspace_path_rejects_empty_input() {
        let result = validate_remote_workspace_path("");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_remote_workspace_path_rejects_whitespace_only_input() {
        let result = validate_remote_workspace_path("   ");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_remote_workspace_path_rejects_newlines() {
        let result = validate_remote_workspace_path("/tmp/abc\n123");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_remote_workspace_path_rejects_null_byte() {
        let result = validate_remote_workspace_path("/tmp/abc\0");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn validate_remote_workspace_path_accepts_valid_path() {
        let result = validate_remote_workspace_path("/home/user/workspaces/ABC-123");

        assert!(result.is_ok());
    }

    #[test]
    fn remote_shell_assign_builds_assignment_for_plain_path() {
        let script = remote_shell_assign("workspace", "/tmp/workspaces/ABC-123");

        assert!(script.contains("workspace='/tmp/workspaces/ABC-123'"));
        assert!(script.contains("case \"$workspace\" in"));
    }

    #[test]
    fn remote_shell_assign_expands_tilde_paths() {
        let script = remote_shell_assign("workspace", "~/.symphony/workspaces");

        assert!(script.contains("workspace='~/.symphony/workspaces'"));
        assert!(script.contains("${workspace#~/}"));
    }

    #[test]
    fn remote_shell_assign_escapes_single_quotes() {
        let script = remote_shell_assign("workspace", "/tmp/work's/ABC-123");

        assert!(script.contains("work'\"'\"'s"));
    }

    #[tokio::test]
    async fn run_hook_times_out() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let result = manager.run_hook("sleep 10", temp_dir.path(), 100).await;

        assert!(matches!(result, Err(SymphonyError::Workspace(ref msg)) if msg.contains("timed out")));
    }

    #[tokio::test]
    async fn lifecycle_hooks_after_run_swallows_errors() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let hooks = HooksConfig {
            after_run: Some("exit 1".into()),
            ..HooksConfig::default()
        };
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks)
            .expect("workspace manager should initialize");

        let workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be created");

        let result = manager
            .run_lifecycle_hooks(&workspace, HookPhase::AfterRun, None)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn lifecycle_hooks_before_run_propagates_errors() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let hooks = HooksConfig {
            before_run: Some("exit 1".into()),
            ..HooksConfig::default()
        };
        let manager = WorkspaceManager::new(temp_dir.path().to_path_buf(), hooks)
            .expect("workspace manager should initialize");

        let workspace = manager
            .ensure_workspace("ABC-123", None)
            .await
            .expect("workspace should be created");

        let result = manager
            .run_lifecycle_hooks(&workspace, HookPhase::BeforeRun, None)
            .await;

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[tokio::test]
    async fn cleanup_nonexistent_workspace_succeeds() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let result = manager.cleanup_workspace("NONEXISTENT-999", None).await;

        assert!(result.is_ok());
    }

    #[test]
    fn workspace_path_uses_sanitized_key() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let manager = create_manager(temp_dir.path());

        let path = manager.workspace_path("PROJECT/ISSUE#42");

        assert_eq!(
            path,
            temp_dir.path().join("PROJECT_ISSUE_42")
        );
    }

    #[test]
    fn validate_remote_workspace_path_accepts_tilde_path() {
        let result = validate_remote_workspace_path("~/workspaces/ABC-123");

        assert!(result.is_ok());
    }

    #[test]
    fn validate_remote_workspace_path_rejects_carriage_return() {
        let result = validate_remote_workspace_path("/tmp/abc\r123");

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }

    #[test]
    fn parse_remote_workspace_output_rejects_empty_path() {
        let output = format!("{REMOTE_WORKSPACE_MARKER}\t1\t\n");

        let result = parse_remote_workspace_output(&output);

        assert!(matches!(result, Err(SymphonyError::Workspace(_))));
    }
}
