use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::{self, Instant};
use tracing::debug;

use crate::domain::RunOutcome;
use crate::error::SymphonyError;

use super::events::{parse_event, CodexEvent};

const DEFAULT_READ_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_TURN_TIMEOUT_MS: u64 = 1_800_000;
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
    RejectToolCall(Value),
}

impl AppServer {
    pub async fn launch(
        command: &str,
        cwd: &Path,
        read_timeout_ms: u64,
        turn_timeout_ms: u64,
    ) -> Result<Self, SymphonyError> {
        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| SymphonyError::Codex(format!("launch_failed: {error}")))?;

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
        })
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
        approval_policy: &str,
        sandbox: &str,
    ) -> Result<String, SymphonyError> {
        let request_id = self.next_request_id();
        self.approval_policy = parse_approval_policy(approval_policy);
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
                "sandboxPolicy": self.sandbox
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
                &mut tokens,
                &mut rate_limit,
            )? {
                TurnAction::Continue => {}
                TurnAction::AutoApprove(id) => {
                    self.handle_approval(&id).await?;
                }
                TurnAction::RejectToolCall(id) => {
                    self.reject_tool_call(&id).await?;
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
        self.send_message(&build_approval_response(id)).await
    }

    async fn reject_tool_call(&mut self, id: &Value) -> Result<(), SymphonyError> {
        self.send_message(&build_tool_rejection_response(id)).await
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

fn evaluate_event(
    event: CodexEvent,
    current_turn_id: Option<&str>,
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
        CodexEvent::ApprovalRequest { id, .. } => Ok(TurnAction::AutoApprove(id)),
        CodexEvent::ToolCall { id, .. } => Ok(TurnAction::RejectToolCall(id)),
        CodexEvent::UserInputRequired => Err(SymphonyError::Codex("turn_input_required".into())),
        CodexEvent::Unknown(_) => Ok(TurnAction::Continue),
    }
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

fn parse_approval_policy(policy: &str) -> Value {
    let trimmed = policy.trim();

    if trimmed.is_empty() || trimmed == "auto" {
        return default_approval_policy();
    }

    serde_json::from_str(trimmed).unwrap_or_else(|_| json!(trimmed))
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
        build_approval_response, build_tool_rejection_response, evaluate_event, SessionTokens,
        TurnAction,
    };
    use crate::codex::events::CodexEvent;
    use crate::domain::RunOutcome;
    use crate::error::SymphonyError;

    #[test]
    // SPEC 17.5: approval responses follow the JSON-RPC shape required by the app-server.
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
    // SPEC 17.5: unsupported dynamic tool calls are rejected without stalling the session.
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
    // SPEC 17.5: token tracking keeps the latest thread totals across repeated usage events.
    fn token_tracking_uses_latest_thread_totals() -> Result<(), SymphonyError> {
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;

        evaluate_event(
            CodexEvent::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            Some("turn-1"),
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
    // SPEC 17.5: user input requests map to a specific runner-visible error.
    fn user_input_required_returns_specific_error() {
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let result = evaluate_event(
            CodexEvent::UserInputRequired,
            Some("turn-1"),
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
    // SPEC 17.5: matching terminal turn events complete the current turn successfully.
    fn matching_terminal_event_completes_turn() -> Result<(), SymphonyError> {
        let mut tokens = SessionTokens::default();
        let mut rate_limit = None;
        let action = evaluate_event(
            CodexEvent::TurnCompleted {
                turn_id: "turn-1".into(),
            },
            Some("turn-1"),
            &mut tokens,
            &mut rate_limit,
        )?;

        assert!(matches!(action, TurnAction::Complete(RunOutcome::Success)));

        Ok(())
    }
}
