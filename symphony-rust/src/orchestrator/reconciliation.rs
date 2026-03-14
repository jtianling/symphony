use std::collections::HashMap;

use chrono::Utc;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::agent_runner::WorkerResult;
use crate::config::SymphonyConfig;
use crate::error::SymphonyError;
use crate::linear::client::LinearClient;
use crate::workspace::WorkspaceManager;

use super::state::OrchestratorState;

#[allow(clippy::implicit_hasher)]
// The orchestrator owns this concrete in-memory map type internally, so a
// generic hasher parameter would add noise without improving call sites.
pub async fn reconcile(
    state: &mut OrchestratorState,
    tracker: &LinearClient,
    config: &SymphonyConfig,
    workspace_manager: &WorkspaceManager,
    worker_handles: &mut HashMap<String, JoinHandle<WorkerResult>>,
) -> Result<(), SymphonyError> {
    let stalled_issue_ids = state
        .running
        .values()
        .filter(|session| is_stalled(session, config.polling.stall_timeout_ms))
        .map(|session| session.issue_id.clone())
        .collect::<Vec<_>>();

    for issue_id in stalled_issue_ids {
        warn!(issue_id = %issue_id, "stalled worker detected");
        terminate_worker(worker_handles, &issue_id);
        if let Some(session) = state.remove_running(&issue_id) {
            state.add_runtime_from_session(&session);
        }
        state.release_claim(&issue_id);
    }

    let running_issue_ids = state.running.keys().cloned().collect::<Vec<_>>();
    if running_issue_ids.is_empty() {
        return Ok(());
    }

    let refreshed_states = match tracker.refresh_issue_states(&running_issue_ids).await {
        Ok(states) => states,
        Err(error) => {
            warn!(error = %error, "failed to refresh running issue states");
            return Ok(());
        }
    };

    for refreshed in refreshed_states {
        if matches_state(&refreshed.state, &config.tracker.terminal_states) {
            terminate_worker(worker_handles, &refreshed.id);
            if let Some(session) = state.remove_running(&refreshed.id) {
                state.add_runtime_from_session(&session);
                if let Err(error) = workspace_manager
                    .cleanup_workspace(&session.issue_identifier)
                    .await
                {
                    warn!(
                        issue_id = %refreshed.id,
                        issue_identifier = %session.issue_identifier,
                        error = %error,
                        "failed to cleanup terminal workspace"
                    );
                }
            }
            state.release_claim(&refreshed.id);
            state.clear_retry_attempt(&refreshed.id);
            continue;
        }

        if matches_state(&refreshed.state, &config.tracker.active_states) {
            state.update_session_state(&refreshed.id, &refreshed.state);
            continue;
        }

        terminate_worker(worker_handles, &refreshed.id);
        if let Some(session) = state.remove_running(&refreshed.id) {
            state.add_runtime_from_session(&session);
        }
        state.release_claim(&refreshed.id);
    }

    Ok(())
}

pub fn is_stalled(session: &crate::domain::LiveSession, stall_timeout_ms: i64) -> bool {
    if stall_timeout_ms <= 0 {
        return false;
    }

    let last_event = session.last_codex_timestamp.unwrap_or(session.started_at);
    let elapsed_ms = Utc::now()
        .signed_duration_since(last_event)
        .num_milliseconds();

    elapsed_ms > stall_timeout_ms
}

fn terminate_worker(
    worker_handles: &mut HashMap<String, JoinHandle<WorkerResult>>,
    issue_id: &str,
) {
    if let Some(handle) = worker_handles.remove(issue_id) {
        handle.abort();
    }
}

fn matches_state(state: &str, configured_states: &[String]) -> bool {
    let normalized = state.trim().to_ascii_lowercase();
    configured_states
        .iter()
        .any(|configured| configured.trim().to_ascii_lowercase() == normalized)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::is_stalled;
    use crate::domain::{LiveSession, TokenUsage};

    fn session(
        started_at: chrono::DateTime<Utc>,
        last_codex_timestamp: Option<chrono::DateTime<Utc>>,
    ) -> LiveSession {
        LiveSession {
            session_id: "session-1".into(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-1".into(),
            issue_state: "Todo".into(),
            workspace_path: "/tmp/SYM-1".into(),
            started_at,
            turn_count: 1,
            last_codex_timestamp,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        }
    }

    #[test]
    // SPEC 17.4: stalled sessions are detected when the last Codex update exceeds the timeout.
    fn is_stalled_returns_true_when_last_update_exceeds_timeout() {
        let live_session = session(
            Utc::now() - Duration::minutes(30),
            Some(Utc::now() - Duration::minutes(20)),
        );

        assert!(is_stalled(&live_session, 60_000));
    }

    #[test]
    // SPEC 17.4: recent Codex activity prevents false-positive stall detection.
    fn is_stalled_returns_false_when_recent_update_exists() {
        let live_session = session(
            Utc::now() - Duration::minutes(30),
            Some(Utc::now() - Duration::seconds(30)),
        );

        assert!(!is_stalled(&live_session, 60_000));
    }

    #[test]
    // SPEC 17.4: stall detection can be disabled with a zero timeout.
    fn is_stalled_returns_false_when_disabled() {
        let live_session = session(Utc::now() - Duration::hours(1), None);

        assert!(!is_stalled(&live_session, 0));
    }
}
