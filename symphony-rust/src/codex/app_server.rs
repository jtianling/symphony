use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::{self, Instant};
use tracing::{debug, info, warn};

use crate::domain::RunOutcome;
use crate::error::SymphonyError;
use crate::ssh;

use super::events::{parse_event, CodexEvent};
use super::tools::LinearGraphqlTool;

const DEFAULT_READ_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_APPROVAL_ACTIONS: [&str; 2] = ["command_execution", "file_changes"];

pub struct AppServer {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    read_timeout_ms: u64,
    turn_timeout_ms: u64,
    next_request_id: u64,
    approval_policy: Value,
    sandbox: String,
    current_thread_id: Option<String>,
    current_turn_id: Option<String>,
    tool_executor: Option<Arc<LinearGraphqlTool>>,
}

impl fmt::Debug for AppServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppServer")
            .field("child_id", &self.child.id())
            .field("read_timeout_ms", &self.read_timeout_ms)
            .field("turn_timeout_ms", &self.turn_timeout_ms)
            .field("next_request_id", &self.next_request_id)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox", &self.sandbox)
            .field("current_thread_id", &self.current_thread_id)
            .field("current_turn_id", &self.current_turn_id)
            .field("has_tool_executor", &self.tool_executor.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub last_reported_total: u64,
}

impl SessionTokens {
    fn update_from_thread_totals(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    ) {
        let _delta = total_tokens.saturating_sub(self.last_reported_total);
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.total_tokens = total_tokens;
        self.last_reported_total = total_tokens;
    }
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub outcome: RunOutcome,
    pub tokens: SessionTokens,
    pub rate_limit: Option<Value>,
}

enum TurnAction {
    Continue,
    Complete(RunOutcome),
    AutoApprove(Value),
    ExecuteToolCall { id: Value, name: String, params: Value },
}

impl AppServer {
    pub async fn launch(
        command: &str,
        cwd: &Path,
        worker_host: Option<&str>,
        read_timeout_ms: u64,
        turn_timeout_ms: u64,
    ) -> Result<Self, SymphonyError> {
        let child = match worker_host {
            Some(worker_host) => launch_remote(command, cwd, worker_host)?,
            None => launch_local(command, cwd)?,
        };

        Self::from_child(child, read_timeout_ms, turn_timeout_ms)
    }

    fn from_child(
        mut child: Child,
        read_timeout_ms: u64,
        turn_timeout_ms: u64,
    ) -> Result<Self, SymphonyError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SymphonyError::Codex("missing_child_stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SymphonyError::Codex("missing_child_stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SymphonyError::Codex("missing_child_stderr".into()))?;

        spawn_stderr_drain(stderr);

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            read_timeout_ms: normalize_timeout(read_timeout_ms, DEFAULT_READ_TIMEOUT_MS),
            turn_timeout_ms: normalize_timeout(turn_timeout_ms, DEFAULT_TURN_TIMEOUT_MS),
            next_request_id: 1,
            approval_policy: default_approval_policy(),
            sandbox: "workspace-write".into(),
            current_thread_id: None,
            current_turn_id: None,
            tool_executor: None,
        })
    }

    pub fn set_tool_executor(&mut self, tool: Arc<LinearGraphqlTool>) {
        self.tool_executor = Some(tool);
    }

    pub async fn initialize(&mut self) -> Result<String, SymphonyError> {
        let request_id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "symphony",
                    "version": "0.1.0"
                },
                "capabilities": {}
            }
        });

        self.send_message(&request).await?;
        let response = self.read_response(request_id).await?;

        self.send_message(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }))
        .await?;

        extract_initialize_info(&response)
    }

    pub async fn start_thread(
        &mut self,
        cwd: &str,
        approval_policy: &Value,
        sandbox: &str,
    ) -> Result<String, SymphonyError> {
        let request_id = self.next_request_id();
        self.approval_policy = approval_policy.clone();
        self.sandbox = normalize_sandbox(sandbox);

        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "thread/start",
            "params": {
                "approvalPolicy": self.approval_policy.clone(),
                "sandbox": self.sandbox,
                "cwd": cwd
            }
        });

        self.send_message(&request).await?;
        let response = self.read_response(request_id).await?;
        let thread_id = extract_nested_id(&response, &["thread", "id"])?;
        self.current_thread_id = Some(thread_id.clone());

        Ok(thread_id)
    }

    pub async fn start_turn(
        &mut self,
        thread_id: &str,
        prompt: &str,
        cwd: &str,
        sandbox_policy: &Value,
    ) -> Result<String, SymphonyError> {
        let request_id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": prompt,
                "cwd": cwd,
                "title": "symphony-turn",
                "approvalPolicy": self.approval_policy.clone(),
                "sandboxPolicy": sandbox_policy.clone()
            }
        });

        self.send_message(&request).await?;
        let response = self.read_response(request_id).await?;
        let turn_id = extract_nested_id(&response, &["turn", "id"])?;
        self.current_turn_id = Some(turn_id.clone());

        Ok(turn_id)
    }

    pub async fn process_turn(&mut self) -> Result<TurnResult, SymphonyError> {
        let started_at = Instant::now();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;

        loop {
            let remaining =
                Duration::from_millis(self.turn_timeout_ms).checked_sub(started_at.elapsed());

            let Some(remaining) = remaining else {
                self.current_turn_id = None;
                return Ok(TurnResult {
                    outcome: RunOutcome::Timeout,
                    tokens,
                    rate_limit,
                });
            };

            let message = match time::timeout(remaining, self.read_message()).await {
                Ok(result) => result?,
                Err(_) => {
                    self.current_turn_id = None;
                    return Ok(TurnResult {
                        outcome: RunOutcome::Timeout,
                        tokens,
                        rate_limit,
                    });
                }
            };

            let event = parse_event(&message);
            match evaluate_event(
                event,
                self.current_turn_id.as_deref(),
                &self.approval_policy,
                &mut tokens,
                &mut rate_limit,
            )? {
                TurnAction::Continue => {}
                TurnAction::AutoApprove(id) => {
                    self.handle_approval(&id).await?;
                }
                TurnAction::ExecuteToolCall { id, name, params } => {
                    self.handle_tool_call(&id, &name, params).await?;
                }
                TurnAction::Complete(outcome) => {
                    self.current_turn_id = None;
                    return Ok(TurnResult {
                        outcome,
                        tokens,
                        rate_limit,
                    });
                }
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), SymphonyError> {
        match self.child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                self.child
                    .start_kill()
                    .map_err(|error| SymphonyError::Codex(format!("shutdown_failed: {error}")))?;
                self.child
                    .wait()
                    .await
                    .map_err(|error| SymphonyError::Codex(format!("wait_failed: {error}")))?;
                Ok(())
            }
            Err(error) => Err(SymphonyError::Codex(format!("try_wait_failed: {error}"))),
        }
    }

    async fn read_message(&mut self) -> Result<Value, SymphonyError> {
        let mut line = String::new();

        loop {
            line.clear();
            let read_result = time::timeout(
                Duration::from_millis(self.read_timeout_ms),
                self.stdout.read_line(&mut line),
            )
            .await;

            let bytes_read = match read_result {
                Ok(result) => {
                    result.map_err(|error| SymphonyError::Codex(format!("read_failed: {error}")))?
                }
                Err(_) => {
                    return Err(SymphonyError::Codex("response_timeout".into()));
                }
            };

            if bytes_read == 0 {
                return Err(SymphonyError::Codex("app_server_stdout_closed".into()));
            }

            if line.trim().is_empty() {
                continue;
            }

            return serde_json::from_str::<Value>(line.trim_end())
                .map_err(|error| SymphonyError::Codex(format!("invalid_json_message: {error}")));
        }
    }

    async fn handle_approval(&mut self, id: &Value) -> Result<(), SymphonyError> {
        info!(id = %id, "auto-approving approval request");
        self.send_message(&build_approval_response(id)).await
    }

    async fn handle_tool_call(
        &mut self,
        id: &Value,
        name: &str,
        params: Value,
    ) -> Result<(), SymphonyError> {
        let result = match &self.tool_executor {
            Some(tool) if name == "linear_graphql" => {
                info!(tool = %name, "executing dynamic tool call");
                tool.handle(params).await
            }
            _ => {
                warn!(tool = %name, "unsupported dynamic tool call");
                build_unsupported_tool_result(name)
            }
        };

        self.send_message(&build_tool_call_response(id, result))
            .await
    }

    async fn send_message(&mut self, payload: &Value) -> Result<(), SymphonyError> {
        let serialized = serde_json::to_vec(payload)
            .map_err(|error| SymphonyError::Codex(format!("message_serialize_failed: {error}")))?;

        self.stdin
            .write_all(&serialized)
            .await
            .map_err(|error| SymphonyError::Codex(format!("stdin_write_failed: {error}")))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|error| SymphonyError::Codex(format!("stdin_write_failed: {error}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| SymphonyError::Codex(format!("stdin_flush_failed: {error}")))
    }

    async fn read_response(&mut self, request_id: u64) -> Result<Value, SymphonyError> {
        loop {
            let message = self.read_message().await?;

            match message.get("id").and_then(Value::as_u64) {
                Some(id) if id == request_id => {
                    if let Some(result) = message.get("result") {
                        return Ok(result.clone());
                    }

                    if let Some(error) = message.get("error") {
                        return Err(SymphonyError::Codex(format!("request_failed: {error}")));
                    }

                    return Err(SymphonyError::Codex("missing_response_result".into()));
                }
                _ => {
                    debug!(message = %message, "ignoring non-matching message");
                }
            }
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        request_id
    }
}

fn spawn_stderr_drain(stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();

        loop {
            line.clear();

            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    debug!(target: "codex.stderr", line = %line.trim_end());
                }
                Err(error) => {
                    debug!(
                        target: "codex.stderr",
                        error = %error,
                        "stderr_drain_failed"
                    );
                    break;
                }
            }
        }
    });
}

fn launch_local(command: &str, cwd: &Path) -> Result<Child, SymphonyError> {
    Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| SymphonyError::Codex(format!("launch_failed: {error}")))
}

fn launch_remote(command: &str, cwd: &Path, worker_host: &str) -> Result<Child, SymphonyError> {
    let workspace = cwd.to_string_lossy().into_owned();
    let remote_command = format!("cd {} && exec {command}", ssh::shell_escape(&workspace));

    ssh::start_port(worker_host, &remote_command).map_err(|error| {
        SymphonyError::Codex(format!("remote_launch_failed on {worker_host}: {error}"))
    })
}

fn evaluate_event(
    event: CodexEvent,
    current_turn_id: Option<&str>,
    approval_policy: &Value,
    tokens: &mut SessionTokens,
    rate_limit: &mut Option<Value>,
) -> Result<TurnAction, SymphonyError> {
    match event {
        CodexEvent::TurnCompleted { turn_id } => {
            if is_current_turn(current_turn_id, &turn_id) {
                Ok(TurnAction::Complete(RunOutcome::Success))
            } else {
                Ok(TurnAction::Continue)
            }
        }
        CodexEvent::TurnFailed { turn_id, error } => {
            if is_current_turn(current_turn_id, &turn_id) {
                Ok(TurnAction::Complete(RunOutcome::Failure(error)))
            } else {
                Ok(TurnAction::Continue)
            }
        }
        CodexEvent::TurnCancelled { turn_id } => {
            if is_current_turn(current_turn_id, &turn_id) {
                Ok(TurnAction::Complete(RunOutcome::Failure(
                    "turn_cancelled".into(),
                )))
            } else {
                Ok(TurnAction::Continue)
            }
        }
        CodexEvent::TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens,
        } => {
            tokens.update_from_thread_totals(input_tokens, output_tokens, total_tokens);
            Ok(TurnAction::Continue)
        }
        CodexEvent::RateLimit { payload } => {
            *rate_limit = Some(payload);
            Ok(TurnAction::Continue)
        }
        CodexEvent::ApprovalRequest { id, payload } => {
            evaluate_approval_request(id, &payload, approval_policy)
        }
        CodexEvent::ToolCall { id, name, params } => {
            Ok(TurnAction::ExecuteToolCall { id, name, params })
        }
        CodexEvent::UserInputRequired => {
            Err(SymphonyError::Codex("turn_input_required".into()))
        }
        CodexEvent::Unknown(_) => Ok(TurnAction::Continue),
    }
}

fn evaluate_approval_request(
    id: Value,
    payload: &Value,
    approval_policy: &Value,
) -> Result<TurnAction, SymphonyError> {
    if is_reject_policy(approval_policy) {
        let request_type = extract_approval_request_type(payload);
        if should_auto_approve_for_reject_policy(approval_policy, &request_type) {
            info!(
                request_type = %request_type,
                "auto-approving via reject policy match"
            );
            return Ok(TurnAction::AutoApprove(id));
        }

        warn!(
            request_type = %request_type,
            "rejecting unknown approval type per reject policy"
        );
        return Err(SymphonyError::Codex(format!(
            "approval_required: {request_type}"
        )));
    }

    Ok(TurnAction::AutoApprove(id))
}

fn is_reject_policy(policy: &Value) -> bool {
    policy.get("reject").is_some()
}

fn extract_approval_request_type(payload: &Value) -> String {
    payload
        .get("kind")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

fn should_auto_approve_for_reject_policy(
    policy: &Value,
    request_type: &str,
) -> bool {
    let Some(reject_map) = policy.get("reject") else {
        return false;
    };

    if reject_map.get(request_type).and_then(Value::as_bool) == Some(true) {
        return true;
    }

    matches!(
        request_type,
        "command_execution" | "file_changes" | "sandbox_approval"
            | "rules" | "mcp_elicitations"
    )
}

fn is_current_turn(current_turn_id: Option<&str>, turn_id: &str) -> bool {
    match current_turn_id {
        Some(current_turn_id) if !turn_id.is_empty() => current_turn_id == turn_id,
        Some(_) => true,
        None => true,
    }
}

fn default_approval_policy() -> Value {
    json!({ "autoApprove": DEFAULT_APPROVAL_ACTIONS })
}

fn normalize_sandbox(sandbox: &str) -> String {
    let trimmed = sandbox.trim();

    if trimmed.is_empty() {
        "workspace-write".into()
    } else {
        trimmed.to_owned()
    }
}

fn normalize_timeout(value: u64, default: u64) -> u64 {
    if value == 0 {
        default
    } else {
        value
    }
}

fn extract_initialize_info(result: &Value) -> Result<String, SymphonyError> {
    if let Some(user_agent) = result.get("userAgent").and_then(Value::as_str) {
        return Ok(user_agent.to_owned());
    }

    if let Some(session_id) = result.get("sessionId").and_then(Value::as_str) {
        return Ok(session_id.to_owned());
    }

    serde_json::to_string(result).map_err(|error| {
        SymphonyError::Codex(format!("initialize_response_serialize_failed: {error}"))
    })
}

fn extract_nested_id(value: &Value, path: &[&str]) -> Result<String, SymphonyError> {
    let id = path
        .iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .map(str::to_owned);

    id.ok_or_else(|| SymphonyError::Codex(format!("missing_id_at_path: {}", path.join("."))))
}

fn build_approval_response(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "approved": true
        }
    })
}

fn build_tool_call_response(id: &Value, result: Value) -> Value {
    json!({
        "id": id,
        "result": result,
    })
}

fn build_unsupported_tool_result(tool_name: &str) -> Value {
    let output = serde_json::to_string_pretty(&json!({
        "error": {
            "message": format!("Unsupported dynamic tool: {:?}.", tool_name),
            "supportedTools": ["linear_graphql"],
        }
    }))
    .unwrap_or_default();

    json!({
        "success": false,
        "output": output,
        "contentItems": [{
            "type": "inputText",
            "text": output,
        }],
    })
}

#[cfg(test)]
fn build_tool_rejection_response(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "success": false,
            "error": "unsupported tool call"
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        build_approval_response, build_tool_call_response, build_tool_rejection_response,
        build_unsupported_tool_result, evaluate_event, SessionTokens, TurnAction,
    };
    use crate::codex::events::CodexEvent;
    use crate::domain::RunOutcome;
    use crate::error::SymphonyError;

    fn default_policy() -> serde_json::Value {
        json!({ "autoApprove": ["command_execution", "file_changes"] })
    }

    #[test]
    fn approval_response_matches_protocol_shape() {
        let payload = build_approval_response(&json!(7));

        assert_eq!(
            payload,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "result": {
                    "approved": true
                }
            })
        );
    }

    #[test]
    fn tool_rejection_matches_protocol_shape() {
        let payload = build_tool_rejection_response(&json!(9));

        assert_eq!(
            payload,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "result": {
                    "success": false,
                    "error": "unsupported tool call"
                }
            })
        );
    }

    #[test]
    fn tool_call_response_wraps_result() {
        let result = json!({"success": true, "output": "ok"});
        let payload = build_tool_call_response(&json!(12), result.clone());

        assert_eq!(payload["id"], json!(12));
        assert_eq!(payload["result"], result);
    }

    #[test]
    fn unsupported_tool_result_contains_error() {
        let result = build_unsupported_tool_result("unknown_tool");

        assert_eq!(result["success"], json!(false));
        assert!(result["output"].as_str().unwrap().contains("Unsupported"));
    }

    #[test]
    fn token_tracking_uses_latest_thread_totals() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;

        evaluate_event(
            CodexEvent::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;
        evaluate_event(
            CodexEvent::TokenUsage {
                input_tokens: 14,
                output_tokens: 9,
                total_tokens: 23,
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert_eq!(
            tokens,
            SessionTokens {
                input_tokens: 14,
                output_tokens: 9,
                total_tokens: 23,
                last_reported_total: 23,
            }
        );

        Ok(())
    }

    #[test]
    fn user_input_required_returns_specific_error() {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let result = evaluate_event(
            CodexEvent::UserInputRequired,
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        );

        assert!(matches!(
            result,
            Err(SymphonyError::Codex(message))
                if message == "turn_input_required"
        ));
    }

    #[test]
    fn matching_terminal_event_completes_turn() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCompleted {
                turn_id: "turn-1".into(),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Complete(RunOutcome::Success)));

        Ok(())
    }

    #[test]
    fn tool_call_event_produces_execute_action() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ToolCall {
                id: json!(15),
                name: "linear_graphql".into(),
                params: json!({"query": "query { viewer { id } }"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(
            action,
            TurnAction::ExecuteToolCall { name, .. } if name == "linear_graphql"
        ));

        Ok(())
    }

    #[test]
    fn reject_policy_auto_approves_known_types() -> Result<(), SymphonyError> {
        let policy = json!({
            "reject": {
                "sandbox_approval": true,
                "rules": true,
            }
        });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(20),
                payload: json!({"kind": "sandbox_approval"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::AutoApprove(_)));

        Ok(())
    }

    #[test]
    fn reject_policy_rejects_unknown_approval_type() {
        let policy = json!({
            "reject": {
                "sandbox_approval": true,
            }
        });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let result = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(21),
                payload: json!({"kind": "some_unknown_type"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        );

        assert!(matches!(result, Err(SymphonyError::Codex(msg)) if msg.contains("approval_required")));
    }

    #[test]
    fn auto_policy_approves_all() -> Result<(), SymphonyError> {
        let policy = json!({ "autoApprove": ["command_execution"] });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(22),
                payload: json!({"kind": "anything"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::AutoApprove(_)));

        Ok(())
    }

    #[test]
    fn turn_failed_produces_failure_outcome() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnFailed {
                turn_id: "turn-1".into(),
                error: "catastrophe".into(),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(
            matches!(action, TurnAction::Complete(RunOutcome::Failure(msg)) if msg == "catastrophe")
        );

        Ok(())
    }

    #[test]
    fn turn_cancelled_produces_failure_outcome() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCancelled {
                turn_id: "turn-1".into(),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(
            action,
            TurnAction::Complete(RunOutcome::Failure(msg)) if msg == "turn_cancelled"
        ));

        Ok(())
    }

    #[test]
    fn mismatched_turn_id_continues() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCompleted {
                turn_id: "turn-OTHER".into(),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Continue));

        Ok(())
    }

    #[test]
    fn empty_turn_id_matches_any_current() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCompleted {
                turn_id: String::new(),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Complete(RunOutcome::Success)));

        Ok(())
    }

    #[test]
    fn no_current_turn_id_completes_on_any_event() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCompleted {
                turn_id: "turn-X".into(),
            },
            None,
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Complete(RunOutcome::Success)));

        Ok(())
    }

    #[test]
    fn rate_limit_event_stores_payload() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::RateLimit {
                payload: json!({"remaining": 10}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Continue));
        assert_eq!(rate_limit, Some(json!({"remaining": 10})));

        Ok(())
    }

    #[test]
    fn unknown_event_continues() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::Unknown(json!({"method": "something"})),
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Continue));

        Ok(())
    }

    #[test]
    fn tool_call_with_unknown_tool_produces_execute_action() -> Result<(), SymphonyError> {
        let policy = default_policy();
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ToolCall {
                id: json!(30),
                name: "unknown_tool".into(),
                params: json!({}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(
            action,
            TurnAction::ExecuteToolCall { name, .. } if name == "unknown_tool"
        ));

        Ok(())
    }

    #[test]
    fn reject_policy_auto_approves_default_known_actions() -> Result<(), SymphonyError> {
        let policy = json!({ "reject": {} });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;

        for action_type in &[
            "command_execution",
            "file_changes",
            "sandbox_approval",
            "rules",
            "mcp_elicitations",
        ] {
            let action = evaluate_event(
                CodexEvent::ApprovalRequest {
                    id: json!(40),
                    payload: json!({"kind": action_type}),
                },
                Some("turn-1"),
                &policy,
                &mut tokens,
                &mut rate_limit,
            )?;

            assert!(
                matches!(action, TurnAction::AutoApprove(_)),
                "expected auto-approve for {action_type}"
            );
        }

        Ok(())
    }

    #[test]
    fn reject_policy_approves_explicitly_listed_type() -> Result<(), SymphonyError> {
        let policy = json!({
            "reject": {
                "custom_action": true,
            }
        });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(41),
                payload: json!({"kind": "custom_action"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::AutoApprove(_)));

        Ok(())
    }

    #[test]
    fn no_policy_approves_all_requests() -> Result<(), SymphonyError> {
        let policy = json!({});
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(42),
                payload: json!({"kind": "anything_goes"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::AutoApprove(_)));

        Ok(())
    }

    #[test]
    fn unsupported_tool_result_mentions_supported_tools() {
        let result = build_unsupported_tool_result("magic_tool");
        let output_str = result["output"].as_str().unwrap();

        assert!(output_str.contains("linear_graphql"));
        assert!(output_str.contains("magic_tool"));
    }

    #[test]
    fn session_tokens_update_from_thread_totals_tracks_delta() {
        let mut tokens = SessionTokens::default();
        tokens.update_from_thread_totals(10, 5, 15);

        assert_eq!(tokens.input_tokens, 10);
        assert_eq!(tokens.output_tokens, 5);
        assert_eq!(tokens.total_tokens, 15);
        assert_eq!(tokens.last_reported_total, 15);

        tokens.update_from_thread_totals(20, 10, 30);

        assert_eq!(tokens.input_tokens, 20);
        assert_eq!(tokens.output_tokens, 10);
        assert_eq!(tokens.total_tokens, 30);
        assert_eq!(tokens.last_reported_total, 30);
    }

    #[test]
    fn approval_request_with_type_field_extracts_correctly() -> Result<(), SymphonyError> {
        let policy = json!({ "reject": { "sandbox_approval": true } });
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::ApprovalRequest {
                id: json!(50),
                payload: json!({"type": "sandbox_approval"}),
            },
            Some("turn-1"),
            &policy,
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::AutoApprove(_)));

        Ok(())
    }
}
