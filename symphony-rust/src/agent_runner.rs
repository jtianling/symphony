use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::codex::tools::LinearGraphqlTool;
use crate::codex::{AppServer, SessionTokens};
use crate::config::{AgentConfig, CodexConfig};
use crate::domain::{Issue, RunOutcome, TokenUsage};
use crate::error::SymphonyError;
use crate::prompt::PromptBuilder;
use crate::tracker::Tracker;
use crate::workspace::{HookPhase, WorkspaceInfo, WorkspaceManager};

const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
const DEFAULT_SANDBOX: &str = "workspace-write";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkerUpdate {
    CodexUpdate {
        issue_id: String,
        tokens: TokenUsage,
        rate_limit: Option<serde_json::Value>,
        timestamp: DateTime<Utc>,
    },
    SessionStarted {
        issue_id: String,
        session_id: String,
    },
    TurnCompleted {
        issue_id: String,
        turn_number: u32,
        outcome: RunOutcome,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
    pub issue_id: String,
    pub issue_identifier: String,
    pub outcome: WorkerOutcome,
    pub total_tokens: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkerOutcome {
    Normal,
    Failure(String),
}

pub async fn run_worker(
    issue: Issue,
    workspace_manager: Arc<WorkspaceManager>,
    prompt_builder: Arc<PromptBuilder>,
    agent_config: AgentConfig,
    codex_config: CodexConfig,
    prompt_template: String,
    update_tx: mpsc::Sender<WorkerUpdate>,
    tracker: Arc<dyn Tracker + Send + Sync>,
    active_states: Vec<String>,
    attempt: Option<u32>,
    worker_host: Option<String>,
    tool_executor: Option<Arc<LinearGraphqlTool>>,
) -> WorkerResult {
    let issue_id = issue.id.clone();
    let issue_identifier = issue.identifier.clone();
    let mut total_tokens = zero_token_usage();
    let mut workspace = None;
    let mut app_server = None;

    let outcome = match prepare_worker(
        &issue,
        workspace_manager.as_ref(),
        &prompt_builder,
        &agent_config,
        &codex_config,
        &prompt_template,
        &update_tx,
        tracker.as_ref(),
        &active_states,
        attempt,
        worker_host.as_deref(),
        tool_executor,
        &mut total_tokens,
        &mut workspace,
        &mut app_server,
    )
    .await
    {
        Ok(()) => WorkerOutcome::Normal,
        Err(error) => {
            error!(
                issue_id = %issue.id,
                issue_identifier = %issue.identifier,
                error = %error,
                "worker failed"
            );
            WorkerOutcome::Failure(error.to_string())
        }
    };

    cleanup_worker(
        workspace_manager.as_ref(),
        workspace.as_ref(),
        app_server.as_mut(),
        &issue,
        worker_host.as_deref(),
    )
    .await;

    WorkerResult {
        issue_id,
        issue_identifier,
        outcome,
        total_tokens,
    }
}

async fn prepare_worker(
    issue: &Issue,
    workspace_manager: &WorkspaceManager,
    prompt_builder: &PromptBuilder,
    agent_config: &AgentConfig,
    codex_config: &CodexConfig,
    prompt_template: &str,
    update_tx: &mpsc::Sender<WorkerUpdate>,
    tracker: &(dyn Tracker + Send + Sync),
    active_states: &[String],
    attempt: Option<u32>,
    worker_host: Option<&str>,
    tool_executor: Option<Arc<LinearGraphqlTool>>,
    total_tokens: &mut TokenUsage,
    workspace: &mut Option<WorkspaceInfo>,
    app_server: &mut Option<AppServer>,
) -> Result<(), SymphonyError> {
    let workspace_info = workspace_manager
        .ensure_workspace(&issue.identifier, worker_host)
        .await?;
    info!(
        issue_id = %issue.id,
        issue_identifier = %issue.identifier,
        workspace = %workspace_info.path.display(),
        created_now = workspace_info.created_now,
        "workspace ready"
    );
    *workspace = Some(workspace_info.clone());

    workspace_manager
        .run_lifecycle_hooks(&workspace_info, HookPhase::AfterCreate, worker_host)
        .await?;
    workspace_manager
        .run_lifecycle_hooks(&workspace_info, HookPhase::BeforeRun, worker_host)
        .await?;

    let first_prompt = prompt_builder.build_prompt(prompt_template, issue, attempt, 1)?;
    let cwd = workspace_info.path.to_string_lossy().into_owned();
    let command = codex_command(codex_config);
    let approval_policy = codex_approval_policy(codex_config.approval_policy.as_ref());
    let sandbox = codex_thread_sandbox(codex_config);
    let turn_sandbox_policy = codex_turn_sandbox_policy(codex_config, &cwd);
    let read_timeout_ms = codex_config.read_timeout_ms.unwrap_or(0);
    let turn_timeout_ms = codex_config.turn_timeout_ms.unwrap_or(0);

    info!(
        issue_id = %issue.id,
        issue_identifier = %issue.identifier,
        command = %command,
        "launching codex app server"
    );
    let mut server = AppServer::launch(
        &command,
        &workspace_info.path,
        worker_host,
        read_timeout_ms,
        turn_timeout_ms,
    )
    .await?;
    if let Some(tool) = tool_executor {
        server.set_tool_executor(tool);
    }
    *app_server = Some(server);
    let session_id = {
        let server = app_server
            .as_mut()
            .ok_or_else(|| SymphonyError::Codex("missing_app_server".into()))?;
        server.initialize().await?
    };
    let thread_id = {
        let server = app_server
            .as_mut()
            .ok_or_else(|| SymphonyError::Codex("missing_app_server".into()))?;
        server
            .start_thread(&cwd, &approval_policy, &sandbox)
            .await?
    };

    send_update(
        update_tx,
        WorkerUpdate::SessionStarted {
            issue_id: issue.id.clone(),
            session_id: session_id.clone(),
        },
    )
    .await;

    info!(
        issue_id = %issue.id,
        issue_identifier = %issue.identifier,
        session_id = %session_id,
        thread_id = %thread_id,
        "codex session started"
    );

    let mut previous_snapshot = SessionTokens::default();

    for turn_number in 1..=agent_config.max_turns {
        let prompt = if turn_number == 1 {
            first_prompt.clone()
        } else {
            prompt_builder.build_prompt(prompt_template, issue, attempt, turn_number)?
        };

        info!(
            issue_id = %issue.id,
            issue_identifier = %issue.identifier,
            turn_number,
            "starting codex turn"
        );

        {
            let server = app_server
                .as_mut()
                .ok_or_else(|| SymphonyError::Codex("missing_app_server".into()))?;
            server
                .start_turn(&thread_id, &prompt, &cwd, &turn_sandbox_policy)
                .await?;
        }
        let turn_result = {
            let server = app_server
                .as_mut()
                .ok_or_else(|| SymphonyError::Codex("missing_app_server".into()))?;
            server.process_turn().await?
        };

        let token_delta = diff_tokens(&previous_snapshot, &turn_result.tokens);
        *total_tokens = session_tokens_to_usage(&turn_result.tokens);
        previous_snapshot = turn_result.tokens.clone();

        let rate_limit = turn_result.rate_limit.clone();
        let timestamp = Utc::now();

        send_update(
            update_tx,
            WorkerUpdate::CodexUpdate {
                issue_id: issue.id.clone(),
                tokens: token_delta,
                rate_limit,
                timestamp,
            },
        )
        .await;
        send_update(
            update_tx,
            WorkerUpdate::TurnCompleted {
                issue_id: issue.id.clone(),
                turn_number,
                outcome: turn_result.outcome.clone(),
            },
        )
        .await;

        info!(
            issue_id = %issue.id,
            issue_identifier = %issue.identifier,
            turn_number,
            outcome = ?turn_result.outcome,
            total_tokens = total_tokens.total_tokens,
            "codex turn completed"
        );

        if !matches!(turn_result.outcome, RunOutcome::Success) {
            return Ok(());
        }

        if turn_number >= agent_config.max_turns {
            break;
        }

        if !issue_is_active(tracker, issue, active_states).await {
            info!(
                issue_id = %issue.id,
                issue_identifier = %issue.identifier,
                turn_number,
                "issue moved out of active states, stopping worker"
            );
            break;
        }
    }

    Ok(())
}

async fn cleanup_worker(
    workspace_manager: &WorkspaceManager,
    workspace: Option<&WorkspaceInfo>,
    app_server: Option<&mut AppServer>,
    issue: &Issue,
    worker_host: Option<&str>,
) {
    if let Some(server) = app_server {
        match server.shutdown().await {
            Ok(()) => {
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    "codex session cleaned up"
                );
            }
            Err(error) => {
                warn!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    error = %error,
                    "failed to clean up codex session"
                );
            }
        }
    }

    if let Some(workspace) = workspace {
        if let Err(error) = workspace_manager
            .run_lifecycle_hooks(workspace, HookPhase::AfterRun, worker_host)
            .await
        {
            warn!(
                issue_id = %issue.id,
                issue_identifier = %issue.identifier,
                error = %error,
                "after_run hook failed"
            );
        }
    }
}

async fn send_update(update_tx: &mpsc::Sender<WorkerUpdate>, update: WorkerUpdate) {
    if let Err(error) = update_tx.send(update).await {
        warn!(error = %error, "failed to forward worker update");
    }
}

async fn issue_is_active(
    tracker: &(dyn Tracker + Send + Sync),
    issue: &Issue,
    active_states: &[String],
) -> bool {
    match tracker
        .refresh_issue_states(std::slice::from_ref(&issue.id))
        .await
    {
        Ok(states) => {
            let Some(current_state) = states.first().map(|entry| entry.state.as_str()) else {
                warn!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    "issue state refresh returned no result"
                );
                return false;
            };

            let is_active = state_matches(current_state, active_states);
            info!(
                issue_id = %issue.id,
                issue_identifier = %issue.identifier,
                current_state = %current_state,
                is_active,
                "refreshed issue state"
            );
            is_active
        }
        Err(error) => {
            warn!(
                issue_id = %issue.id,
                issue_identifier = %issue.identifier,
                error = %error,
                "failed to refresh issue state, assuming issue remains active"
            );
            true
        }
    }
}

fn state_matches(current_state: &str, active_states: &[String]) -> bool {
    let normalized_current = current_state.trim().to_ascii_lowercase();
    active_states
        .iter()
        .map(|state| state.trim().to_ascii_lowercase())
        .any(|state| state == normalized_current)
}

fn diff_tokens(previous: &SessionTokens, current: &SessionTokens) -> TokenUsage {
    TokenUsage {
        input_tokens: current.input_tokens.saturating_sub(previous.input_tokens),
        output_tokens: current.output_tokens.saturating_sub(previous.output_tokens),
        total_tokens: current.total_tokens.saturating_sub(previous.total_tokens),
    }
}

fn session_tokens_to_usage(tokens: &SessionTokens) -> TokenUsage {
    TokenUsage {
        input_tokens: tokens.input_tokens,
        output_tokens: tokens.output_tokens,
        total_tokens: tokens.total_tokens,
    }
}

fn zero_token_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
    }
}

fn codex_command(config: &CodexConfig) -> String {
    config
        .command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CODEX_COMMAND)
        .to_owned()
}

fn default_approval_policy() -> Value {
    json!({ "autoApprove": ["command_execution", "file_changes"] })
}

fn codex_approval_policy(approval_policy: Option<&Value>) -> Value {
    match approval_policy {
        None => default_approval_policy(),
        Some(Value::String(policy)) if policy.trim().is_empty() || policy.trim() == "auto" => {
            default_approval_policy()
        }
        Some(policy) => policy.clone(),
    }
}

fn codex_thread_sandbox(config: &CodexConfig) -> String {
    config
        .thread_sandbox
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            config
                .sandbox
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(DEFAULT_SANDBOX)
        .to_owned()
}

fn codex_turn_sandbox_policy(config: &CodexConfig, workspace_cwd: &str) -> Value {
    config
        .turn_sandbox_policy
        .clone()
        .unwrap_or_else(|| generate_turn_sandbox_policy(config, workspace_cwd))
}

fn generate_turn_sandbox_policy(config: &CodexConfig, workspace_cwd: &str) -> Value {
    let sandbox = codex_thread_sandbox(config);
    match sandbox.as_str() {
        "workspace-write" => {
            json!({
                "type": "workspaceWrite",
                "writableRoots": [workspace_cwd],
                "readOnlyAccess": {"type": "fullAccess"},
                "networkAccess": false,
                "excludeTmpdirEnvVar": false,
                "excludeSlashTmp": false,
            })
        }
        _ => Value::String(sandbox),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::{
        codex_approval_policy, codex_thread_sandbox, codex_turn_sandbox_policy, WorkerOutcome,
        WorkerResult, WorkerUpdate,
    };
    use crate::config::CodexConfig;
    use crate::domain::{RunOutcome, TokenUsage};

    #[test]
    fn agent_runner_worker_update_serializes_codex_update() -> Result<(), serde_json::Error> {
        let update = WorkerUpdate::CodexUpdate {
            issue_id: "issue-1".into(),
            tokens: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            rate_limit: Some(json!({ "remaining": 42 })),
            timestamp: chrono::Utc
                .with_ymd_and_hms(2026, 3, 14, 8, 30, 0)
                .single()
                .ok_or_else(|| serde_json::Error::io(std::io::Error::other("invalid timestamp")))?,
        };

        let serialized = serde_json::to_value(update)?;

        assert_eq!(
            serialized,
            json!({
                "CodexUpdate": {
                    "issue_id": "issue-1",
                    "tokens": {
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "total_tokens": 15
                    },
                    "rate_limit": { "remaining": 42 },
                    "timestamp": "2026-03-14T08:30:00Z"
                }
            })
        );

        Ok(())
    }

    #[test]
    fn agent_runner_worker_result_construction_preserves_fields() {
        let result = WorkerResult {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-1".into(),
            outcome: WorkerOutcome::Failure("boom".into()),
            total_tokens: TokenUsage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
        };

        assert_eq!(result.issue_id, "issue-1");
        assert_eq!(result.issue_identifier, "SYM-1");
        assert_eq!(result.total_tokens.total_tokens, 30);
        assert!(matches!(result.outcome, WorkerOutcome::Failure(ref error) if error == "boom"));
    }

    #[test]
    fn agent_runner_worker_outcome_variants_are_distinct() {
        let normal = WorkerOutcome::Normal;
        let failure = WorkerOutcome::Failure("failed".into());

        assert!(matches!(normal, WorkerOutcome::Normal));
        assert!(matches!(failure, WorkerOutcome::Failure(ref error) if error == "failed"));
    }

    #[test]
    fn agent_runner_worker_update_turn_completed_holds_outcome() {
        let update = WorkerUpdate::TurnCompleted {
            issue_id: "issue-2".into(),
            turn_number: 3,
            outcome: RunOutcome::Timeout,
        };

        assert!(matches!(
            update,
            WorkerUpdate::TurnCompleted {
                issue_id,
                turn_number: 3,
                outcome: RunOutcome::Timeout,
            } if issue_id == "issue-2"
        ));
    }

    #[test]
    fn codex_approval_policy_uses_default_for_auto() {
        assert_eq!(
            codex_approval_policy(Some(&json!("auto"))),
            json!({ "autoApprove": ["command_execution", "file_changes"] })
        );
        assert_eq!(
            codex_approval_policy(None),
            json!({ "autoApprove": ["command_execution", "file_changes"] })
        );
    }

    #[test]
    fn codex_approval_policy_preserves_map_values() {
        let policy = json!({ "reject": { "sandbox_approval": true } });

        assert_eq!(codex_approval_policy(Some(&policy)), policy);
    }

    #[test]
    fn config_thread_sandbox_with_sandbox_fallback() {
        let config = CodexConfig {
            sandbox: Some("container".into()),
            ..CodexConfig::default()
        };

        assert_eq!(codex_thread_sandbox(&config), "container");
    }

    #[test]
    fn codex_thread_sandbox_prefers_thread_sandbox() {
        let config = CodexConfig {
            sandbox: Some("container".into()),
            thread_sandbox: Some("none".into()),
            ..CodexConfig::default()
        };

        assert_eq!(codex_thread_sandbox(&config), "none");
    }

    #[test]
    fn codex_turn_sandbox_policy_falls_back_to_thread_sandbox() {
        let config = CodexConfig {
            sandbox: Some("container".into()),
            ..CodexConfig::default()
        };

        assert_eq!(
            codex_turn_sandbox_policy(&config, "/tmp/ws"),
            json!("container")
        );
    }

    #[test]
    fn codex_turn_sandbox_policy_generates_structured_for_workspace_write() {
        let config = CodexConfig::default();
        let policy = codex_turn_sandbox_policy(&config, "/tmp/ws");

        assert_eq!(policy["type"], json!("workspaceWrite"));
        assert_eq!(policy["writableRoots"], json!(["/tmp/ws"]));
        assert_eq!(
            policy["readOnlyAccess"],
            json!({"type": "fullAccess"})
        );
        assert_eq!(policy["networkAccess"], json!(false));
    }

    #[test]
    fn codex_turn_sandbox_policy_preserves_explicit_policy() {
        let explicit = json!({"type": "custom", "writableRoots": ["/foo"]});
        let config = CodexConfig {
            turn_sandbox_policy: Some(explicit.clone()),
            ..CodexConfig::default()
        };

        assert_eq!(codex_turn_sandbox_policy(&config, "/tmp/ws"), explicit);
    }

    #[test]
    fn diff_tokens_computes_delta_correctly() {
        let previous = super::SessionTokens {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            last_reported_total: 15,
        };
        let current = super::SessionTokens {
            input_tokens: 25,
            output_tokens: 12,
            total_tokens: 37,
            last_reported_total: 37,
        };

        let delta = super::diff_tokens(&previous, &current);

        assert_eq!(delta.input_tokens, 15);
        assert_eq!(delta.output_tokens, 7);
        assert_eq!(delta.total_tokens, 22);
    }

    #[test]
    fn diff_tokens_handles_zero_delta() {
        let tokens = super::SessionTokens {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            last_reported_total: 15,
        };

        let delta = super::diff_tokens(&tokens, &tokens);

        assert_eq!(delta.input_tokens, 0);
        assert_eq!(delta.output_tokens, 0);
        assert_eq!(delta.total_tokens, 0);
    }

    #[test]
    fn session_tokens_to_usage_preserves_all_fields() {
        let tokens = super::SessionTokens {
            input_tokens: 100,
            output_tokens: 200,
            total_tokens: 300,
            last_reported_total: 300,
        };

        let usage = super::session_tokens_to_usage(&tokens);

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.total_tokens, 300);
    }

    #[test]
    fn zero_token_usage_is_all_zeros() {
        let usage = super::zero_token_usage();

        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn codex_command_falls_back_to_default() {
        let config = CodexConfig::default();

        assert_eq!(super::codex_command(&config), "codex app-server");
    }

    #[test]
    fn codex_command_uses_configured_value() {
        let config = CodexConfig {
            command: Some("custom-codex".into()),
            ..CodexConfig::default()
        };

        assert_eq!(super::codex_command(&config), "custom-codex");
    }

    #[test]
    fn codex_command_falls_back_for_empty_string() {
        let config = CodexConfig {
            command: Some("".into()),
            ..CodexConfig::default()
        };

        assert_eq!(super::codex_command(&config), "codex app-server");
    }

    #[test]
    fn codex_command_falls_back_for_whitespace_only() {
        let config = CodexConfig {
            command: Some("   ".into()),
            ..CodexConfig::default()
        };

        assert_eq!(super::codex_command(&config), "codex app-server");
    }

    #[test]
    fn state_matches_is_case_insensitive() {
        assert!(super::state_matches("Todo", &["todo".into(), "in progress".into()]));
        assert!(super::state_matches("IN PROGRESS", &["Todo".into(), "In Progress".into()]));
        assert!(!super::state_matches("Done", &["Todo".into(), "In Progress".into()]));
    }

    #[test]
    fn state_matches_handles_whitespace() {
        assert!(super::state_matches("  Todo  ", &["todo".into()]));
        assert!(super::state_matches("Todo", &["  todo  ".into()]));
    }

    #[tokio::test]
    async fn issue_is_active_returns_true_for_active_state() {
        let tracker = crate::tracker::MemoryTracker::new(vec![crate::domain::Issue {
            id: "issue-1".into(),
            identifier: "SYM-1".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "In Progress".into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }]);
        let issue = crate::domain::Issue {
            id: "issue-1".into(),
            identifier: "SYM-1".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "In Progress".into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        };
        let active_states = vec!["In Progress".into(), "Todo".into()];

        let result = super::issue_is_active(&tracker, &issue, &active_states).await;

        assert!(result);
    }

    #[tokio::test]
    async fn issue_is_active_returns_false_for_terminal_state() {
        let tracker = crate::tracker::MemoryTracker::new(vec![crate::domain::Issue {
            id: "issue-1".into(),
            identifier: "SYM-1".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "Done".into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }]);
        let issue = crate::domain::Issue {
            id: "issue-1".into(),
            identifier: "SYM-1".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "In Progress".into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        };
        let active_states = vec!["In Progress".into(), "Todo".into()];

        let result = super::issue_is_active(&tracker, &issue, &active_states).await;

        assert!(!result);
    }

    #[tokio::test]
    async fn issue_is_active_returns_false_when_issue_not_found() {
        let tracker = crate::tracker::MemoryTracker::new(vec![]);
        let issue = crate::domain::Issue {
            id: "missing".into(),
            identifier: "SYM-X".into(),
            title: "Test".into(),
            description: None,
            priority: None,
            state: "In Progress".into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        };
        let active_states = vec!["In Progress".into()];

        let result = super::issue_is_active(&tracker, &issue, &active_states).await;

        assert!(!result);
    }

    #[test]
    fn codex_approval_policy_empty_string_uses_default() {
        assert_eq!(
            codex_approval_policy(Some(&json!(""))),
            json!({ "autoApprove": ["command_execution", "file_changes"] })
        );
    }

    #[test]
    fn codex_approval_policy_whitespace_only_uses_default() {
        assert_eq!(
            codex_approval_policy(Some(&json!("  "))),
            json!({ "autoApprove": ["command_execution", "file_changes"] })
        );
    }

    #[test]
    fn codex_thread_sandbox_defaults_to_workspace_write() {
        let config = CodexConfig::default();

        assert_eq!(codex_thread_sandbox(&config), "workspace-write");
    }

    #[test]
    fn codex_thread_sandbox_empty_strings_fallback() {
        let config = CodexConfig {
            thread_sandbox: Some("".into()),
            sandbox: Some("  ".into()),
            ..CodexConfig::default()
        };

        assert_eq!(codex_thread_sandbox(&config), "workspace-write");
    }
}
