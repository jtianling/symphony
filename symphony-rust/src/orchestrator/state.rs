use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::domain::{LiveSession, RetryEntry};

#[derive(Debug, Clone, Default, Serialize)]
pub struct AggregateTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

#[derive(Debug, Clone, Default)]
pub struct OrchestratorState {
    pub running: HashMap<String, LiveSession>,
    pub claimed: HashSet<String>,
    pub retry_attempts: HashMap<String, u32>,
    pub retrying: HashMap<String, RetryEntry>,
    pub completed: HashSet<String>,
    pub codex_totals: AggregateTokens,
    pub codex_rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub generated_at: DateTime<Utc>,
    pub counts: SnapshotCounts,
    pub running: Vec<RunningSnapshot>,
    pub retrying: Vec<RetryEntry>,
    pub codex_totals: AggregateTokens,
    pub rate_limits: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotCounts {
    pub running: usize,
    pub claimed: usize,
    pub completed: usize,
    pub retrying: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningSnapshot {
    pub issue_id: String,
    pub issue_identifier: String,
    pub state: String,
    pub session_id: String,
    pub turn_count: u32,
    pub workspace_path: String,
    pub started_at: DateTime<Utc>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub tokens: crate::domain::TokenUsage,
}

impl OrchestratorState {
    pub fn claim_issue(&mut self, issue_id: &str) {
        self.claimed.insert(issue_id.to_owned());
    }

    pub fn release_claim(&mut self, issue_id: &str) {
        self.claimed.remove(issue_id);
    }

    pub fn add_running(&mut self, session: LiveSession) {
        self.running.insert(session.issue_id.clone(), session);
    }

    pub fn remove_running(&mut self, issue_id: &str) -> Option<LiveSession> {
        self.running.remove(issue_id)
    }

    pub fn is_claimed(&self, issue_id: &str) -> bool {
        self.claimed.contains(issue_id)
    }

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn running_count_by_state(&self, state: &str) -> usize {
        let normalized = normalize_state(state);
        self.running
            .values()
            .filter(|session| normalize_state(&session.issue_state) == normalized)
            .count()
    }

    pub fn set_retry_attempt(&mut self, issue_id: &str, attempt: u32) {
        self.retry_attempts.insert(issue_id.to_owned(), attempt);
    }

    pub fn set_retry_entry(&mut self, entry: RetryEntry) {
        self.retrying.insert(entry.issue_id.clone(), entry);
    }

    pub fn retry_attempt(&self, issue_id: &str) -> u32 {
        self.retry_attempts.get(issue_id).copied().unwrap_or(0)
    }

    pub fn retry_entry(&self, issue_id: &str) -> Option<RetryEntry> {
        self.retrying.get(issue_id).cloned()
    }

    pub fn clear_retry_entry(&mut self, issue_id: &str) {
        self.retrying.remove(issue_id);
    }

    pub fn clear_retry_attempt(&mut self, issue_id: &str) {
        self.retry_attempts.remove(issue_id);
        self.retrying.remove(issue_id);
    }

    pub fn mark_completed(&mut self, issue_id: &str) {
        self.completed.insert(issue_id.to_owned());
    }

    pub fn add_runtime_from_session(&mut self, session: &LiveSession) {
        let elapsed = Utc::now().signed_duration_since(session.started_at);
        let seconds = elapsed.num_milliseconds() as f64 / 1000.0;
        self.codex_totals.seconds_running += seconds.max(0.0);
    }

    pub fn update_session_state(&mut self, issue_id: &str, issue_state: &str) {
        if let Some(session) = self.running.get_mut(issue_id) {
            session.issue_state = issue_state.to_owned();
        }
    }

    pub fn update_session_started(&mut self, issue_id: &str, session_id: &str) {
        if let Some(session) = self.running.get_mut(issue_id) {
            session.session_id = session_id.to_owned();
        }
    }

    pub fn increment_turn_count(&mut self, issue_id: &str) {
        if let Some(session) = self.running.get_mut(issue_id) {
            session.turn_count = session.turn_count.saturating_add(1);
        }
    }

    pub fn update_session_timestamp(&mut self, issue_id: &str, timestamp: DateTime<Utc>) {
        if let Some(session) = self.running.get_mut(issue_id) {
            session.last_codex_timestamp = Some(timestamp);
        }
    }

    pub fn add_session_tokens(&mut self, issue_id: &str, tokens: &crate::domain::TokenUsage) {
        if let Some(session) = self.running.get_mut(issue_id) {
            session.tokens.input_tokens = session
                .tokens
                .input_tokens
                .saturating_add(tokens.input_tokens);
            session.tokens.output_tokens = session
                .tokens
                .output_tokens
                .saturating_add(tokens.output_tokens);
            session.tokens.total_tokens = session
                .tokens
                .total_tokens
                .saturating_add(tokens.total_tokens);
        }
    }

    pub fn add_aggregate_tokens(&mut self, tokens: &crate::domain::TokenUsage) {
        self.codex_totals.input_tokens = self
            .codex_totals
            .input_tokens
            .saturating_add(tokens.input_tokens);
        self.codex_totals.output_tokens = self
            .codex_totals
            .output_tokens
            .saturating_add(tokens.output_tokens);
        self.codex_totals.total_tokens = self
            .codex_totals
            .total_tokens
            .saturating_add(tokens.total_tokens);
    }

    pub fn set_rate_limits(&mut self, value: Option<serde_json::Value>) {
        self.codex_rate_limits = value;
    }

    pub fn snapshot(&self) -> StateSnapshot {
        let generated_at = Utc::now();
        let mut codex_totals = self.codex_totals.clone();
        codex_totals.seconds_running += self
            .running
            .values()
            .map(|session| {
                generated_at
                    .signed_duration_since(session.started_at)
                    .num_milliseconds() as f64
                    / 1000.0
            })
            .map(|seconds| seconds.max(0.0))
            .sum::<f64>();

        let mut running = self
            .running
            .values()
            .cloned()
            .map(|session| RunningSnapshot {
                issue_id: session.issue_id,
                issue_identifier: session.issue_identifier,
                state: session.issue_state,
                session_id: session.session_id,
                turn_count: session.turn_count,
                workspace_path: session.workspace_path,
                started_at: session.started_at,
                last_event_at: session.last_codex_timestamp,
                tokens: session.tokens,
            })
            .collect::<Vec<_>>();
        running.sort_by(|left, right| left.issue_identifier.cmp(&right.issue_identifier));

        let mut retrying = self.retrying.values().cloned().collect::<Vec<_>>();
        retrying.sort_by(|left, right| left.issue_identifier.cmp(&right.issue_identifier));

        StateSnapshot {
            generated_at,
            counts: SnapshotCounts {
                running: self.running.len(),
                claimed: self.claimed.len(),
                completed: self.completed.len(),
                retrying: self.retrying.len(),
            },
            running,
            retrying,
            codex_totals,
            rate_limits: self.codex_rate_limits.clone(),
        }
    }
}

fn normalize_state(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::OrchestratorState;
    use crate::domain::{LiveSession, RetryEntry, TokenUsage};

    fn sample_session(issue_id: &str, issue_state: &str) -> LiveSession {
        LiveSession {
            session_id: "session-1".into(),
            issue_id: issue_id.into(),
            issue_identifier: format!("SYM-{issue_id}"),
            issue_state: issue_state.into(),
            workspace_path: format!("/tmp/{issue_id}"),
            started_at: Utc::now() - Duration::seconds(5),
            turn_count: 1,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
            },
        }
    }

    #[test]
    // SPEC 17.4: claimed issue bookkeeping reflects claim and release transitions.
    fn claim_and_release_issue_updates_state() {
        let mut state = OrchestratorState::default();

        state.claim_issue("issue-1");
        assert!(state.is_claimed("issue-1"));

        state.release_claim("issue-1");
        assert!(!state.is_claimed("issue-1"));
    }

    #[test]
    // SPEC 17.4: running session bookkeeping updates counts as workers start and stop.
    fn add_and_remove_running_session_updates_counts() {
        let mut state = OrchestratorState::default();
        let session = sample_session("issue-1", "Todo");

        state.add_running(session.clone());
        assert_eq!(state.running_count(), 1);

        let removed = state.remove_running("issue-1");
        assert!(removed.is_some());
        assert_eq!(state.running_count(), 0);
    }

    #[test]
    // SPEC 17.4: running counts by state are normalized case-insensitively.
    fn running_count_by_state_is_case_insensitive() {
        let mut state = OrchestratorState::default();
        state.add_running(sample_session("issue-1", "Todo"));
        state.add_running(sample_session("issue-2", "todo"));
        state.add_running(sample_session("issue-3", "In Progress"));

        assert_eq!(state.running_count_by_state("TODO"), 2);
        assert_eq!(state.running_count_by_state("in progress"), 1);
    }

    #[test]
    // SPEC 17.6: snapshot output includes aggregate runtime seconds for active sessions.
    fn snapshot_includes_active_runtime_seconds() {
        let mut state = OrchestratorState::default();
        state.codex_totals.seconds_running = 10.0;
        state.add_running(sample_session("issue-1", "Todo"));

        let snapshot = state.snapshot();

        assert_eq!(snapshot.counts.running, 1);
        assert!(snapshot.codex_totals.seconds_running >= 15.0);
    }

    #[test]
    // SPEC 17.4: retry queue entries are exposed in snapshots with identifier and attempt.
    fn snapshot_includes_retry_entries() {
        let mut state = OrchestratorState::default();

        state.set_retry_attempt("issue-1", 2);
        state.set_retry_entry(RetryEntry {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-1".into(),
            attempt: 2,
            scheduled_at: Utc::now() + Duration::seconds(30),
            reason: Some("retry".into()),
        });

        let snapshot = state.snapshot();

        assert_eq!(snapshot.counts.retrying, 1);
        assert_eq!(snapshot.retrying.len(), 1);
        assert_eq!(snapshot.retrying[0].issue_identifier, "SYM-1");
    }
}
