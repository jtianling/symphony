#![allow(clippy::needless_raw_string_hashes, clippy::too_many_lines)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tempfile::tempdir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, Instant};
use wiremock::matchers::{body_partial_json, header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use symphony::config::SymphonyConfig;
use symphony::workflow::load_workflow;

const ELIXIR_WORKFLOW_PATH: &str = "/Users/jtianling/workspace/symphony/elixir/WORKFLOW.md";
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
// SPEC 17.1: a realistic repository `WORKFLOW.md` example parses end-to-end.
fn loads_realistic_workflow_example() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let workflow_path = if Path::new(ELIXIR_WORKFLOW_PATH).is_file() {
        PathBuf::from(ELIXIR_WORKFLOW_PATH)
    } else {
        let fallback_path = directory.path().join("WORKFLOW.md");
        fs::write(&fallback_path, realistic_workflow_example())?;
        fallback_path
    };

    let workflow = load_workflow(&workflow_path)?;
    let config = SymphonyConfig::from_yaml_value(&workflow.config)?;

    assert_eq!(config.tracker.kind.as_deref(), Some("linear"));
    assert_eq!(
        config.tracker.project_slug.as_deref(),
        Some("symphony-0c79b11b75ea")
    );
    assert!(config
        .workspace
        .root
        .as_deref()
        .is_some_and(|path| { path.ends_with("code/symphony-workspaces") }));
    assert!(config
        .codex
        .command
        .as_deref()
        .is_some_and(|command| command.contains("app-server")));
    assert!(workflow.prompt_template.contains("{{ issue.identifier }}"));
    assert!(workflow
        .prompt_template
        .contains("## Step 0: Determine current ticket state"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
// SPEC 17.5 / 17.8: smoke-test the binary with a mock Linear API and mock Codex app-server.
async fn binary_smoke_test_uses_mock_linear_and_mock_codex(
) -> Result<(), Box<dyn std::error::Error>> {
    let linear_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "linear-test-key"))
        .and(body_partial_json(json!({
            "variables": {
                "projectSlug": "demo-project",
                "states": ["Todo"],
                "after": null
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "issues": {
                    "nodes": [
                        {
                            "id": "issue-1",
                            "identifier": "SYM-1",
                            "title": "Smoke test ticket",
                            "description": "Verify Symphony wiring",
                            "priority": 1,
                            "branchName": "feature/sym-1",
                            "url": "https://example.invalid/SYM-1",
                            "createdAt": "2026-03-14T00:00:00Z",
                            "updatedAt": "2026-03-14T00:00:00Z",
                            "state": { "name": "Todo" },
                            "labels": { "nodes": [{ "name": "Backend" }] },
                            "relations": { "nodes": [] }
                        }
                    ],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        })))
        .mount(&linear_server)
        .await;

    let directory = tempdir()?;
    let workflow_path = directory.path().join("WORKFLOW.md");
    let codex_script_path = directory.path().join("mock-codex.sh");
    let codex_spawn_marker = directory.path().join("codex-spawned");
    let codex_transcript = directory.path().join("codex-transcript.log");
    let workspace_root = directory.path().join("workspaces");

    write_mock_codex_script(&codex_script_path, &codex_spawn_marker, &codex_transcript)?;
    fs::write(
        &workflow_path,
        smoke_workflow(
            &linear_server.uri(),
            &workspace_root,
            &codex_script_path,
            &codex_spawn_marker,
            &codex_transcript,
        ),
    )?;

    let child = spawn_symphony(&workflow_path)?;

    wait_for_linear_request(&linear_server).await?;
    wait_for_file(&codex_spawn_marker).await?;
    sleep(Duration::from_millis(500)).await;

    terminate_child(child).await?;

    let received_requests = linear_server.received_requests().await.unwrap_or_default();
    assert!(!received_requests.is_empty());

    Ok(())
}

fn realistic_workflow_example() -> &'static str {
    r#"---
tracker:
  kind: linear
  project_slug: "symphony-0c79b11b75ea"
  active_states:
    - Todo
    - In Progress
    - Merging
    - Rework
  terminal_states:
    - Closed
    - Cancelled
    - Canceled
    - Duplicate
    - Done
polling:
  interval_ms: 5000
workspace:
  root: ~/code/symphony-workspaces
hooks:
  after_create: |
    git clone --depth 1 https://github.com/openai/symphony .
agent:
  max_concurrent_agents: 10
  max_turns: 20
codex:
  command: codex app-server
  approval_policy: never
---

You are working on a Linear ticket `{{ issue.identifier }}`

## Step 0: Determine current ticket state and route
"#
}

fn smoke_workflow(
    linear_endpoint: &str,
    workspace_root: &Path,
    codex_script_path: &Path,
    codex_spawn_marker: &Path,
    codex_transcript: &Path,
) -> String {
    format!(
        r#"---
tracker:
  kind: linear
  api_key: linear-test-key
  endpoint: {linear_endpoint}
  project_slug: demo-project
  active_states:
    - Todo
  terminal_states: []
polling:
  interval_ms: 60000
workspace:
  root: {workspace_root}
agent:
  max_concurrent_agents: 1
  max_turns: 1
codex:
  command: {codex_command}
  approval_policy: auto
  sandbox: workspace-write
---

Issue {{{{ issue.identifier }}}}
"#,
        linear_endpoint = yaml_string_impl(linear_endpoint),
        workspace_root = yaml_string(workspace_root),
        codex_command = yaml_string_impl(&format!(
            "{} {} {}",
            shell_string(codex_script_path),
            shell_string(codex_spawn_marker),
            shell_string(codex_transcript)
        )),
    )
}

fn write_mock_codex_script(
    script_path: &Path,
    spawn_marker: &Path,
    transcript: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let script = r#"#!/bin/bash
set -eu
marker="$1"
transcript="$2"
touch "$marker"
step=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$transcript"
  case "$step" in
    0)
      printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"sessionId":"mock-session"}}}}'
      ;;
    1)
      ;;
    2)
      printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"thread":{{"id":"thread-1"}}}}}}'
      ;;
    3)
      printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"turn":{{"id":"turn-1"}}}}}}'
      printf '%s\n' '{{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{{"inputTokens":3,"outputTokens":5,"totalTokens":8}}}}'
      printf '%s\n' '{{"jsonrpc":"2.0","method":"codex/rateLimit","params":{{"remaining":99}}}}'
      printf '%s\n' '{{"jsonrpc":"2.0","method":"turn/completed","params":{{"turnId":"turn-1"}}}}'
      exit 0
      ;;
  esac
  step=$((step + 1))
done
"#
    .to_string();

    fs::write(script_path, script)?;

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(script_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(script_path, permissions)?;
    }

    let _ = spawn_marker;
    let _ = transcript;

    Ok(())
}

fn spawn_symphony(workflow_path: &Path) -> Result<Child, Box<dyn std::error::Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_symphony"));
    command
        .arg(workflow_path)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    Ok(command.spawn()?)
}

async fn wait_for_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    wait_for("codex spawn marker", || async { path.is_file() }).await
}

async fn wait_for_linear_request(server: &MockServer) -> Result<(), Box<dyn std::error::Error>> {
    wait_for("Linear request", || async {
        server
            .received_requests()
            .await
            .is_some_and(|requests| !requests.is_empty())
    })
    .await
}

async fn wait_for<F, Fut>(label: &str, mut predicate: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();

    while start.elapsed() < WAIT_TIMEOUT {
        if predicate().await {
            return Ok(());
        }

        sleep(WAIT_POLL_INTERVAL).await;
    }

    Err(io::Error::other(format!("timed out waiting for {label}")).into())
}

async fn terminate_child(child: Child) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            let status = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()
                .await?;
            if !status.success() {
                return Err(io::Error::other("failed to send SIGTERM to symphony child").into());
            }
        }

        #[cfg(not(unix))]
        child.start_kill()?;
    }

    let output = child.wait_with_output().await?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!("symphony exited unsuccessfully: {stderr}")).into())
}

fn yaml_string(value: &Path) -> String {
    yaml_string_impl(&value.display().to_string())
}

fn shell_string(value: &Path) -> String {
    shell_string_impl(&value.display().to_string())
}

fn yaml_string_impl(value: &str) -> String {
    format!("{value:?}")
}

fn shell_string_impl(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
