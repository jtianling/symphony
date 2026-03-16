use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::error::SymphonyError;

const SSH_CONFIG_ENV: &str = "SYMPHONY_SSH_CONFIG";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub destination: String,
    pub port: Option<String>,
}

pub fn parse_target(host: &str) -> SshTarget {
    if let Some(target) = parse_bracketed_target(host) {
        return target;
    }

    if host.matches(':').count() != 1 {
        return plain_target(host);
    }

    let Some((destination, port)) = host.rsplit_once(':') else {
        return plain_target(host);
    };

    if destination.is_empty() || !is_port(port) {
        return plain_target(host);
    }

    SshTarget {
        destination: destination.to_owned(),
        port: Some(port.to_owned()),
    }
}

pub fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn remote_shell_command(command: &str) -> String {
    format!("bash -lc {}", shell_escape(command))
}

pub fn ssh_executable() -> Result<String, SymphonyError> {
    let output = std::process::Command::new("which")
        .arg("ssh")
        .output()
        .map_err(|error| ssh_error(format!("ssh_lookup_failed: {error}")))?;

    if !output.status.success() {
        return Err(ssh_error(format!(
            "ssh_not_found: {}",
            combined_output(&output.stdout, &output.stderr).trim()
        )));
    }

    let executable = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if executable.is_empty() {
        return Err(ssh_error("ssh_not_found: empty lookup result"));
    }

    Ok(executable)
}

pub fn ssh_args(host: &str, command: &str) -> Vec<String> {
    let target = parse_target(host);
    let mut args = Vec::new();

    maybe_put_config(&mut args);
    args.push("-T".into());

    if let Some(port) = target.port {
        args.push("-p".into());
        args.push(port);
    }

    args.push(target.destination);
    args.push(remote_shell_command(command));

    args
}

pub async fn run(host: &str, command: &str) -> Result<(String, i32), SymphonyError> {
    let executable = ssh_executable()?;
    let output = Command::new(executable)
        .args(ssh_args(host, command))
        .output()
        .await
        .map_err(|error| ssh_error(format!("ssh_run_failed: {error}")))?;

    let status = output.status.code().unwrap_or(-1);
    let combined = combined_output(&output.stdout, &output.stderr);

    Ok((combined, status))
}

pub fn start_port(host: &str, command: &str) -> Result<Child, SymphonyError> {
    let executable = ssh_executable()?;

    Command::new(executable)
        .args(ssh_args(host, command))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ssh_error(format!("ssh_start_port_failed: {error}")))
}

fn parse_bracketed_target(host: &str) -> Option<SshTarget> {
    if !host.starts_with('[') {
        return None;
    }

    let end = host.find(']')?;
    let destination = &host[..=end];
    let remainder = &host[end + 1..];

    if let Some(port) = remainder.strip_prefix(':').filter(|value| is_port(value)) {
        return Some(SshTarget {
            destination: destination.to_owned(),
            port: Some(port.to_owned()),
        });
    }

    Some(plain_target(host))
}

fn plain_target(host: &str) -> SshTarget {
    SshTarget {
        destination: host.to_owned(),
        port: None,
    }
}

fn is_port(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn maybe_put_config(args: &mut Vec<String>) {
    let config = std::env::var(SSH_CONFIG_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());

    if let Some(config) = config {
        args.push("-F".into());
        args.push(config);
    }
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = String::from_utf8_lossy(stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(stderr));
    output
}

fn ssh_error(message: impl Into<String>) -> SymphonyError {
    SymphonyError::Ssh(message.into())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::{parse_target, shell_escape, ssh_args, SshTarget, SSH_CONFIG_ENV};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn parse_target_splits_host_and_port() {
        assert_eq!(
            parse_target("worker1:2222"),
            SshTarget {
                destination: "worker1".into(),
                port: Some("2222".into()),
            }
        );
    }

    #[test]
    fn parse_target_keeps_plain_host() {
        assert_eq!(
            parse_target("worker1"),
            SshTarget {
                destination: "worker1".into(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_target_supports_bracketed_ipv6() {
        assert_eq!(
            parse_target("[::1]:2222"),
            SshTarget {
                destination: "[::1]".into(),
                port: Some("2222".into()),
            }
        );
    }

    #[test]
    fn parse_target_treats_bare_ipv6_as_plain_host() {
        assert_eq!(
            parse_target("::1"),
            SshTarget {
                destination: "::1".into(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_target_treats_invalid_port_targets_as_plain_host() {
        assert_eq!(
            parse_target("user@host:path"),
            SshTarget {
                destination: "user@host:path".into(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_target_handles_empty_port_and_trailing_colon() {
        assert_eq!(
            parse_target("worker1:"),
            SshTarget {
                destination: "worker1:".into(),
                port: None,
            }
        );
        assert_eq!(
            parse_target("[::1]:"),
            SshTarget {
                destination: "[::1]:".into(),
                port: None,
            }
        );
    }

    #[test]
    fn shell_escape_wraps_plain_string() {
        assert_eq!(shell_escape("ls -la"), "'ls -la'");
    }

    #[test]
    fn shell_escape_escapes_single_quotes() {
        assert_eq!(shell_escape("echo 'hello'"), "'echo '\"'\"'hello'\"'\"''");
    }

    #[test]
    fn shell_escape_handles_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn ssh_args_include_t_flag_and_destination() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::remove_var(SSH_CONFIG_ENV);

        assert_eq!(
            ssh_args("worker1", "echo hello"),
            vec![
                "-T".to_string(),
                "worker1".to_string(),
                "bash -lc 'echo hello'".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_args_include_port_when_present() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::remove_var(SSH_CONFIG_ENV);

        assert_eq!(
            ssh_args("worker1:2222", "echo hello"),
            vec![
                "-T".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "worker1".to_string(),
                "bash -lc 'echo hello'".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_args_include_config_when_env_var_is_set() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::set_var(SSH_CONFIG_ENV, "/tmp/test-ssh-config");

        assert_eq!(
            ssh_args("worker1", "echo hello"),
            vec![
                "-F".to_string(),
                "/tmp/test-ssh-config".to_string(),
                "-T".to_string(),
                "worker1".to_string(),
                "bash -lc 'echo hello'".to_string(),
            ]
        );

        std::env::remove_var(SSH_CONFIG_ENV);
    }

    #[test]
    fn ssh_args_for_bracketed_ipv6_with_port() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::remove_var(SSH_CONFIG_ENV);

        assert_eq!(
            ssh_args("[::1]:2222", "echo test"),
            vec![
                "-T".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "[::1]".to_string(),
                "bash -lc 'echo test'".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_args_for_user_at_host() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::remove_var(SSH_CONFIG_ENV);

        assert_eq!(
            ssh_args("user@host", "ls"),
            vec![
                "-T".to_string(),
                "user@host".to_string(),
                "bash -lc 'ls'".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_args_skip_config_when_env_empty() {
        let _guard = env_lock().lock().expect("env lock should succeed");
        std::env::set_var(SSH_CONFIG_ENV, "");

        assert_eq!(
            ssh_args("worker1", "echo hello"),
            vec![
                "-T".to_string(),
                "worker1".to_string(),
                "bash -lc 'echo hello'".to_string(),
            ]
        );

        std::env::remove_var(SSH_CONFIG_ENV);
    }

    #[test]
    fn shell_escape_handles_special_characters() {
        assert_eq!(shell_escape("a\"b"), "'a\"b'");
        assert_eq!(shell_escape("a\\b"), "'a\\b'");
        assert_eq!(shell_escape("a b c"), "'a b c'");
    }

    #[test]
    fn remote_shell_command_wraps_correctly() {
        let cmd = super::remote_shell_command("echo hello");
        assert_eq!(cmd, "bash -lc 'echo hello'");
    }

    #[test]
    fn parse_target_user_at_host_with_port() {
        assert_eq!(
            parse_target("user@host:22"),
            SshTarget {
                destination: "user@host".into(),
                port: Some("22".into()),
            }
        );
    }

    #[test]
    fn parse_target_multiple_colons_treated_as_plain() {
        assert_eq!(
            parse_target("a:b:c"),
            SshTarget {
                destination: "a:b:c".into(),
                port: None,
            }
        );
    }

    #[test]
    fn parse_target_bracketed_ipv6_without_port() {
        assert_eq!(
            parse_target("[::1]"),
            SshTarget {
                destination: "[::1]".into(),
                port: None,
            }
        );
    }
}
