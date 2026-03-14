use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::codex::{AppServer, SessionTokens};
use crate::config::{AgentConfig, CodexConfig};
use crate::domain::{Issue, RunOutcome, TokenUsage};
use crate::error::SymphonyError;
use crate::linear::client::LinearClient;
use crate::prompt::PromptBuilder;
use crate::workspace::{HookPhase, WorkspaceInfo, WorkspaceManager};

const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
const DEFAULT_APPROVAL_POLICY: &str = "auto";
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
    tracker_client: Arc<LinearClient>,
    active_states: Vec<String>,
    attempt: Option<u32>,
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
        tracker_client.as_ref(),
        &active_states,
        attempt,
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
    tracker_client: &LinearClient,
    active_states: &[String],
    attempt: Option<u32>,
    total_tokens: &mut TokenUsage,
    workspace: &mut Option<WorkspaceInfo>,
    app_server: &mut Option<AppServer>,
) -> Result<(), SymphonyError> {
    let workspace_info = workspace_manager
        .ensure_workspace(&issue.identifier)
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
        .run_lifecycle_hooks(&workspace_info, HookPhase::AfterCreate)
        .await?;
    workspace_manager
        .run_lifecycle_hooks(&workspace_info, HookPhase::BeforeRun)
        .await?;

    let first_prompt = prompt_builder.build_prompt(prompt_template, issue, attempt, 1)?;
    let cwd = workspace_info.path.to_string_lossy().into_owned();
    let command = codex_command(codex_config);
    let approval_policy = codex_approval_policy(codex_config);
    let sandbox = codex_sandbox(codex_config);

    info!(
        issue_id = %issue.id,
        issue_identifier = %issue.identifier,
        command = %command,
        "launching codex app server"
    );
    *app_server = Some(AppServer::launch(&command, &workspace_info.path, 0, 0).await?);
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
            server.start_turn(&thread_id, &prompt, &cwd).await?;
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

        if !issue_is_active(tracker_client, issue, active_states).await {
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
            .run_lifecycle_hooks(workspace, HookPhase::AfterRun)
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
    tracker_client: &LinearClient,
    issue: &Issue,
    active_states: &[String],
) -> bool {
    match tracker_client
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

fn codex_approval_policy(config: &CodexConfig) -> String {
    config
        .approval_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_APPROVAL_POLICY)
        .to_owned()
}

fn codex_sandbox(config: &CodexConfig) -> String {
    config
        .sandbox
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SANDBOX)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::{WorkerOutcome, WorkerResult, WorkerUpdate};
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
}
