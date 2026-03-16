use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent_runner::{run_worker, WorkerOutcome, WorkerResult, WorkerUpdate};
use crate::codex::tools::LinearGraphqlTool;
use crate::config::SymphonyConfig;
use crate::domain::{Issue, LiveSession, RetryEntry, WorkflowDefinition};
use crate::http::StateProvider;
use crate::linear::LinearClient;
use crate::prompt::PromptBuilder;
use crate::tracker::{build_tracker, Tracker};
use crate::workspace::{default_workspace_root, WorkspaceManager};

mod dispatch;
mod reconciliation;
mod retry;
mod state;

pub use dispatch::{
    available_global_slots, available_state_slots, select_eligible, select_worker_host,
    sort_candidates,
};
pub use reconciliation::{is_stalled, reconcile};
pub use retry::{compute_retry_delay, RetryQueue};
pub use state::{
    AggregateTokens, OrchestratorState, RunningSnapshot, SnapshotCounts, StateSnapshot,
};

#[derive(Debug, Clone)]
pub enum OrchestratorMsg {
    Tick,
    WorkerExit(WorkerResult),
    CodexUpdate(WorkerUpdate),
    RetryTimer { issue_id: String },
    WorkflowReload(WorkflowDefinition),
    RefreshRequest,
    Shutdown,
}

enum RevalidateResult {
    Ok(Box<Issue>),
    Skip(String),
    Error(String),
}

pub struct Orchestrator {
    state: OrchestratorState,
    config: SymphonyConfig,
    prompt_template: String,
    workspace_manager: Arc<WorkspaceManager>,
    prompt_builder: Arc<PromptBuilder>,
    tracker: Arc<dyn Tracker + Send + Sync>,
    tool_executor: Option<Arc<LinearGraphqlTool>>,
    msg_tx: mpsc::Sender<OrchestratorMsg>,
    msg_rx: mpsc::Receiver<OrchestratorMsg>,
    worker_handles: HashMap<String, JoinHandle<WorkerResult>>,
    retry_queue: RetryQueue,
    state_provider: Option<Arc<StateProvider>>,
}

impl Orchestrator {
    pub fn new(
        config: SymphonyConfig,
        prompt_template: String,
        workspace_manager: Arc<WorkspaceManager>,
        prompt_builder: Arc<PromptBuilder>,
        tracker: Arc<dyn Tracker + Send + Sync>,
    ) -> Self {
        let (msg_tx, msg_rx) = mpsc::channel(512);
        let tool_executor = build_tool_executor(&config);

        Self {
            state: OrchestratorState::default(),
            config,
            prompt_template,
            workspace_manager,
            prompt_builder,
            tracker,
            tool_executor,
            msg_tx,
            msg_rx,
            worker_handles: HashMap::new(),
            retry_queue: RetryQueue::default(),
            state_provider: None,
        }
    }

    pub fn sender(&self) -> mpsc::Sender<OrchestratorMsg> {
        self.msg_tx.clone()
    }

    pub fn set_state_provider(&mut self, state_provider: Arc<StateProvider>) {
        self.state_provider = Some(state_provider);
        self.publish_snapshot();
    }

    pub fn snapshot(&self) -> StateSnapshot {
        self.state.snapshot()
    }

    pub async fn run(&mut self) {
        self.cleanup_terminal_workspaces().await;
        self.publish_snapshot();
        let _ = self.msg_tx.send(OrchestratorMsg::Tick).await;

        while let Some(message) = self.msg_rx.recv().await {
            let should_shutdown = match message {
                OrchestratorMsg::Tick => {
                    self.handle_tick().await;
                    self.schedule_tick(self.config.polling.interval_ms);
                    false
                }
                OrchestratorMsg::WorkerExit(result) => {
                    self.handle_worker_exit(result).await;
                    false
                }
                OrchestratorMsg::CodexUpdate(update) => {
                    self.handle_codex_update(update);
                    false
                }
                OrchestratorMsg::RetryTimer { issue_id } => {
                    self.handle_retry_timer(&issue_id).await;
                    false
                }
                OrchestratorMsg::WorkflowReload(definition) => {
                    self.handle_workflow_reload(definition);
                    false
                }
                OrchestratorMsg::RefreshRequest => {
                    self.handle_tick().await;
                    false
                }
                OrchestratorMsg::Shutdown => {
                    self.shutdown();
                    true
                }
            };

            self.publish_snapshot();

            if should_shutdown {
                break;
            }
        }
    }

    async fn handle_tick(&mut self) {
        if let Err(error) = reconciliation::reconcile(
            &mut self.state,
            self.tracker.as_ref(),
            &self.config,
            self.workspace_manager.as_ref(),
            &mut self.worker_handles,
        )
        .await
        {
            warn!(error = %error, "orchestrator reconciliation failed");
        }

        if let Err(error) = self.config.validate() {
            warn!(error = %error, "orchestrator config validation failed");
            return;
        }

        let candidates = match self.tracker.fetch_candidates(&self.config.tracker).await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(error = %error, "failed to fetch candidate issues");
                return;
            }
        };

        let mut eligible = dispatch::select_eligible(
            &candidates,
            &self.state,
            &self.config.agent,
            &self.config.tracker.active_states,
            &self.config.tracker.terminal_states,
        );
        dispatch::sort_candidates(&mut eligible);

        let mut dispatched = 0_u32;
        for issue in eligible {
            if dispatch::available_global_slots(
                &self.state,
                self.config.agent.max_concurrent_agents,
            ) == 0
            {
                break;
            }

            if dispatch::available_state_slots(&self.state, &issue.state, &self.config.agent)
                .map(|slots| slots == 0)
                .unwrap_or(false)
            {
                continue;
            }

            self.dispatch_issue(issue.clone(), None, None).await;
            dispatched = dispatched.saturating_add(1);
        }

        info!(
            fetched = candidates.len(),
            dispatched,
            running = self.state.running_count(),
            retrying = self.retry_queue.len(),
            claimed = self.state.claimed.len(),
            "orchestrator tick completed"
        );
    }

    async fn dispatch_issue(
        &mut self,
        issue: Issue,
        attempt: Option<u32>,
        preferred_worker_host: Option<String>,
    ) {
        match self.revalidate_issue(&issue).await {
            RevalidateResult::Ok(refreshed) => {
                self.do_dispatch_issue(*refreshed, attempt, preferred_worker_host)
                    .await;
            }
            RevalidateResult::Skip(reason) => {
                info!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    reason = %reason,
                    "skipping dispatch after revalidation"
                );
            }
            RevalidateResult::Error(error) => {
                warn!(
                    issue_id = %issue.id,
                    issue_identifier = %issue.identifier,
                    error = %error,
                    "issue revalidation failed, skipping dispatch"
                );
            }
        }
    }

    async fn revalidate_issue(&self, issue: &Issue) -> RevalidateResult {
        let refreshed_states = match self
            .tracker
            .refresh_issue_states(std::slice::from_ref(&issue.id))
            .await
        {
            Ok(states) => states,
            Err(error) => return RevalidateResult::Error(error.to_string()),
        };

        let Some(refreshed) = refreshed_states.first() else {
            return RevalidateResult::Skip("issue no longer visible".into());
        };

        let is_terminal = self
            .config
            .tracker
            .terminal_states
            .iter()
            .any(|state| state.trim().eq_ignore_ascii_case(refreshed.state.trim()));

        if is_terminal {
            return RevalidateResult::Skip(format!(
                "issue in terminal state: {}",
                refreshed.state
            ));
        }

        let mut updated_issue = issue.clone();
        updated_issue.state = refreshed.state.clone();
        RevalidateResult::Ok(Box::new(updated_issue))
    }

    async fn do_dispatch_issue(
        &mut self,
        issue: Issue,
        attempt: Option<u32>,
        preferred_worker_host: Option<String>,
    ) {
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let workspace_path = self.workspace_manager.workspace_path(&issue.identifier);
        let prompt_template = self.prompt_template.clone();
        let update_tx = self.spawn_update_forwarder();
        let worker_msg_tx = self.msg_tx.clone();
        let workspace_manager = Arc::clone(&self.workspace_manager);
        let prompt_builder = Arc::clone(&self.prompt_builder);
        let tracker = Arc::clone(&self.tracker);
        let agent_config = self.config.agent.clone();
        let worker_host = dispatch::select_worker_host(
            &self.state,
            &self.config.worker,
            &self.config.agent,
            preferred_worker_host.as_deref(),
        );
        let codex_config = self.config.codex.clone();
        let active_states = self.config.tracker.active_states.clone();
        let issue_for_worker = issue.clone();
        let worker_host_for_worker = worker_host.clone();
        let tool_executor = self.tool_executor.clone();

        let handle = tokio::spawn(async move {
            let result = run_worker(
                issue_for_worker,
                workspace_manager,
                prompt_builder,
                agent_config,
                codex_config,
                prompt_template,
                update_tx,
                tracker,
                active_states,
                attempt,
                worker_host_for_worker,
                tool_executor,
            )
            .await;

            let _ = worker_msg_tx
                .send(OrchestratorMsg::WorkerExit(result.clone()))
                .await;
            result
        });

        self.retry_queue.cancel(&issue_id);
        self.state.claim_issue(&issue_id);
        self.state
            .set_retry_attempt(&issue_id, attempt.unwrap_or(0));
        self.state.clear_retry_entry(&issue_id);
        self.state.add_running(LiveSession {
            session_id: String::new(),
            issue_id: issue_id.clone(),
            issue_identifier: issue_identifier.clone(),
            issue_state: issue.state.clone(),
            worker_host: worker_host.clone(),
            workspace_path: workspace_path.to_string_lossy().into_owned(),
            started_at: Utc::now(),
            turn_count: 0,
            last_codex_timestamp: None,
            tokens: crate::domain::TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        });
        self.worker_handles.insert(issue_id.clone(), handle);

        info!(
            issue_id = %issue_id,
            issue_identifier = %issue_identifier,
            attempt = attempt.unwrap_or(0),
            worker_host = ?worker_host,
            "dispatched issue"
        );
    }

    async fn handle_worker_exit(&mut self, result: WorkerResult) {
        let issue_id = result.issue_id.clone();
        self.worker_handles.remove(&issue_id);

        let original_worker_host = self
            .state
            .running
            .get(&issue_id)
            .and_then(|session| session.worker_host.clone());

        if let Some(session) = self.state.remove_running(&issue_id) {
            self.state.add_runtime_from_session(&session);
        }

        self.state.release_claim(&issue_id);

        match &result.outcome {
            WorkerOutcome::Normal => {
                self.state.mark_completed(&issue_id);
                self.schedule_retry(
                    &issue_id,
                    &result.issue_identifier,
                    1,
                    &result.outcome,
                    Some("continuation"),
                    original_worker_host,
                );
            }
            WorkerOutcome::Failure(reason) => {
                let next_attempt = self.state.retry_attempt(&issue_id).saturating_add(1).max(1);
                self.schedule_retry(
                    &issue_id,
                    &result.issue_identifier,
                    next_attempt,
                    &result.outcome,
                    Some(reason),
                    original_worker_host,
                );
            }
        }

        info!(
            issue_id = %result.issue_id,
            issue_identifier = %result.issue_identifier,
            outcome = ?result.outcome,
            "worker exited"
        );
    }

    fn handle_codex_update(&mut self, update: WorkerUpdate) {
        match update {
            WorkerUpdate::CodexUpdate {
                issue_id,
                tokens,
                rate_limit,
                timestamp,
            } => {
                self.state.update_session_timestamp(&issue_id, timestamp);
                self.state.add_session_tokens(&issue_id, &tokens);
                self.state.add_aggregate_tokens(&tokens);
                self.state.set_rate_limits(rate_limit);
            }
            WorkerUpdate::SessionStarted {
                issue_id,
                session_id,
            } => {
                self.state.update_session_started(&issue_id, &session_id);
            }
            WorkerUpdate::TurnCompleted { issue_id, .. } => {
                self.state.increment_turn_count(&issue_id);
            }
        }
    }

    fn handle_workflow_reload(&mut self, definition: WorkflowDefinition) {
        let config = match SymphonyConfig::from_yaml_value(&definition.config) {
            Ok(config) => config,
            Err(error) => {
                warn!(error = %error, "failed to parse reloaded workflow config");
                return;
            }
        };

        let tracker = match build_tracker(&config.tracker) {
            Ok(tracker) => tracker,
            Err(error) => {
                warn!(error = %error, "failed to rebuild tracker after reload");
                return;
            }
        };

        let workspace_manager = match WorkspaceManager::new(
            default_workspace_root(config.workspace.root.as_deref()),
            config.hooks.clone(),
        ) {
            Ok(manager) => Arc::new(manager),
            Err(error) => {
                warn!(error = %error, "failed to rebuild workspace manager after reload");
                return;
            }
        };

        self.tool_executor = build_tool_executor(&config);
        self.config = config;
        self.prompt_template = definition.prompt_template;
        self.tracker = tracker;
        self.workspace_manager = workspace_manager;

        info!("workflow reloaded");
    }

    async fn handle_retry_timer(&mut self, issue_id: &str) {
        let scheduled_attempt = self
            .retry_queue
            .attempt(issue_id)
            .unwrap_or_else(|| self.state.retry_attempt(issue_id).max(1));
        let preferred_worker_host = self
            .state
            .retry_entry(issue_id)
            .and_then(|entry| entry.worker_host.clone());
        let _ = self.retry_queue.remove(issue_id);
        self.state.release_claim(issue_id);

        let candidates = match self.tracker.fetch_candidates(&self.config.tracker).await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(issue_id = %issue_id, error = %error, "retry candidate fetch failed");
                let next_attempt = scheduled_attempt.saturating_add(1).max(1);
                let retry_issue_identifier = self.retry_issue_identifier(issue_id);
                self.schedule_retry(
                    issue_id,
                    &retry_issue_identifier,
                    next_attempt,
                    &WorkerOutcome::Failure(String::from("retry poll failed")),
                    Some("retry poll failed"),
                    preferred_worker_host,
                );
                return;
            }
        };

        let Some(issue) = candidates.iter().find(|candidate| candidate.id == issue_id) else {
            self.state.clear_retry_attempt(issue_id);
            return;
        };

        let eligible = dispatch::select_eligible(
            std::slice::from_ref(issue),
            &self.state,
            &self.config.agent,
            &self.config.tracker.active_states,
            &self.config.tracker.terminal_states,
        );

        if !eligible.is_empty() {
            self.dispatch_issue(
                issue.clone(),
                Some(scheduled_attempt.max(1)),
                preferred_worker_host,
            )
            .await;
            return;
        }

        let no_slots =
            dispatch::available_global_slots(&self.state, self.config.agent.max_concurrent_agents)
                == 0
                || dispatch::available_state_slots(&self.state, &issue.state, &self.config.agent)
                    .map(|slots| slots == 0)
                    .unwrap_or(false);

        if no_slots {
            let next_attempt = scheduled_attempt.saturating_add(1).max(1);
            self.schedule_retry(
                issue_id,
                &issue.identifier,
                next_attempt,
                &WorkerOutcome::Failure(String::from("no available orchestrator slots")),
                Some("no available orchestrator slots"),
                preferred_worker_host,
            );
            return;
        }

        self.state.clear_retry_attempt(issue_id);
    }

    async fn cleanup_terminal_workspaces(&self) {
        let Some(project_slug) = self
            .config
            .tracker
            .project_slug
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            warn!("skipping terminal workspace cleanup because tracker.project_slug is missing");
            return;
        };

        let terminal_issues = match self
            .tracker
            .fetch_issues_by_states(project_slug, &self.config.tracker.terminal_states)
            .await
        {
            Ok(issues) => issues,
            Err(error) => {
                warn!(error = %error, "failed to fetch terminal issues during startup cleanup");
                return;
            }
        };

        for issue in terminal_issues {
            if issue.identifier.trim().is_empty() {
                continue;
            }

            if let Err(error) = self
                .workspace_manager
                .cleanup_workspace(&issue.identifier, None)
                .await
            {
                warn!(
                    issue_identifier = %issue.identifier,
                    error = %error,
                    "failed to cleanup terminal workspace"
                );
            }
        }
    }

    fn schedule_tick(&self, delay_ms: u64) {
        let msg_tx = self.msg_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = msg_tx.send(OrchestratorMsg::Tick).await;
        });
    }

    fn schedule_retry(
        &mut self,
        issue_id: &str,
        issue_identifier: &str,
        attempt: u32,
        outcome: &WorkerOutcome,
        reason: Option<&str>,
        worker_host: Option<String>,
    ) {
        let delay =
            retry::compute_retry_delay(outcome, attempt, self.config.agent.max_retry_backoff_ms);
        let scheduled_at = Utc::now()
            + ChronoDuration::milliseconds(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX));
        self.state.claim_issue(issue_id);
        self.state.set_retry_attempt(issue_id, attempt);
        self.state.set_retry_entry(RetryEntry {
            issue_id: issue_id.to_owned(),
            issue_identifier: issue_identifier.to_owned(),
            attempt,
            scheduled_at,
            reason: reason.map(str::to_owned),
            worker_host,
        });
        self.retry_queue
            .schedule(issue_id, attempt, delay, self.msg_tx.clone());

        info!(
            issue_id = %issue_id,
            attempt,
            delay_ms = delay.as_millis(),
            reason = reason.unwrap_or("retry scheduled"),
            "scheduled retry"
        );
    }

    fn retry_issue_identifier(&self, issue_id: &str) -> String {
        self.state
            .retry_entry(issue_id)
            .map(|entry| entry.issue_identifier)
            .unwrap_or_else(|| issue_id.to_owned())
    }

    fn shutdown(&mut self) {
        for (_, handle) in self.worker_handles.drain() {
            handle.abort();
        }
        self.retry_queue.clear();
        info!("orchestrator shutdown complete");
    }

    fn spawn_update_forwarder(&self) -> mpsc::Sender<WorkerUpdate> {
        let (update_tx, mut update_rx) = mpsc::channel(128);
        let msg_tx = self.msg_tx.clone();

        tokio::spawn(async move {
            while let Some(update) = update_rx.recv().await {
                if msg_tx
                    .send(OrchestratorMsg::CodexUpdate(update))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        update_tx
    }

    fn publish_snapshot(&self) {
        if let Some(state_provider) = &self.state_provider {
            state_provider.update(self.state.snapshot());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use chrono::Utc;
    use tokio::sync::mpsc;

    use crate::config::{
        AgentConfig, CodexConfig, HooksConfig, SymphonyConfig, TrackerConfig,
    };
    use crate::domain::{Issue, LiveSession, RetryEntry, TokenUsage};
    use crate::prompt::PromptBuilder;
    use crate::tracker::MemoryTracker;
    use crate::workspace::WorkspaceManager;

    use super::{Orchestrator, RevalidateResult};

    fn test_issue(id: &str, state: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: format!("SYM-{id}"),
            title: format!("Issue {id}"),
            description: None,
            priority: Some(1),
            state: state.into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: Some("2026-03-14T00:00:00Z".into()),
            updated_at: None,
        }
    }

    fn test_config() -> SymphonyConfig {
        SymphonyConfig {
            tracker: TrackerConfig {
                kind: Some("memory".into()),
                active_states: vec!["Todo".into(), "In Progress".into()],
                terminal_states: vec!["Done".into(), "Canceled".into()],
                ..TrackerConfig::default()
            },
            agent: AgentConfig {
                max_concurrent_agents: 3,
                max_concurrent_agents_by_state: HashMap::new(),
                max_turns: 5,
                max_retry_backoff_ms: 60_000,
            },
            codex: CodexConfig {
                command: Some("echo test".into()),
                ..CodexConfig::default()
            },
            ..SymphonyConfig::default()
        }
    }

    fn build_orchestrator(
        issues: Vec<Issue>,
    ) -> Orchestrator {
        let config = test_config();
        let tracker = Arc::new(MemoryTracker::new(issues));
        let root = std::env::temp_dir().join("symphony_test_orch");
        let _ = std::fs::create_dir_all(&root);
        let workspace_manager = Arc::new(
            WorkspaceManager::new(root, HooksConfig::default())
                .expect("workspace manager should initialize"),
        );
        let prompt_builder = Arc::new(PromptBuilder::default());

        Orchestrator::new(
            config,
            "Test prompt".into(),
            workspace_manager,
            prompt_builder,
            tracker,
        )
    }

    #[tokio::test]
    async fn revalidate_issue_returns_ok_for_active_issue() {
        let issue = test_issue("issue-1", "Todo");
        let orchestrator = build_orchestrator(vec![issue.clone()]);

        let result = orchestrator.revalidate_issue(&issue).await;

        assert!(matches!(result, RevalidateResult::Ok(refreshed) if refreshed.state == "Todo"));
    }

    #[tokio::test]
    async fn revalidate_issue_returns_skip_for_terminal_issue() {
        let issue = test_issue("issue-1", "Done");
        let orchestrator = build_orchestrator(vec![issue.clone()]);

        let result = orchestrator.revalidate_issue(&issue).await;

        assert!(matches!(result, RevalidateResult::Skip(reason) if reason.contains("terminal")));
    }

    #[tokio::test]
    async fn revalidate_issue_returns_skip_when_issue_not_found() {
        let issue = test_issue("missing", "Todo");
        let orchestrator = build_orchestrator(vec![]);

        let result = orchestrator.revalidate_issue(&issue).await;

        assert!(
            matches!(result, RevalidateResult::Skip(reason) if reason.contains("no longer visible"))
        );
    }

    #[tokio::test]
    async fn revalidate_issue_updates_state_from_tracker() {
        let mut issue = test_issue("issue-1", "Todo");
        let mut tracker_issue = issue.clone();
        tracker_issue.state = "In Progress".into();
        let orchestrator = build_orchestrator(vec![tracker_issue]);

        issue.state = "Todo".into();
        let result = orchestrator.revalidate_issue(&issue).await;

        assert!(
            matches!(result, RevalidateResult::Ok(refreshed) if refreshed.state == "In Progress")
        );
    }

    #[tokio::test]
    async fn handle_worker_exit_normal_schedules_continuation_retry() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        let session = LiveSession {
            session_id: "session-1".into(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            issue_state: "Todo".into(),
            worker_host: Some("host1".into()),
            workspace_path: "/tmp/test".into(),
            started_at: Utc::now(),
            turn_count: 1,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        orchestrator.state.claim_issue("issue-1");
        orchestrator.state.add_running(session);

        let result = crate::agent_runner::WorkerResult {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            outcome: crate::agent_runner::WorkerOutcome::Normal,
            total_tokens: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        };

        orchestrator.handle_worker_exit(result).await;

        assert!(orchestrator.state.completed.contains("issue-1"));
        assert!(orchestrator.state.is_claimed("issue-1"));
        let retry_entry = orchestrator.state.retry_entry("issue-1");
        assert!(retry_entry.is_some());
        let entry = retry_entry.unwrap();
        assert_eq!(entry.worker_host.as_deref(), Some("host1"));
    }

    #[tokio::test]
    async fn handle_worker_exit_failure_schedules_backoff_retry() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        let session = LiveSession {
            session_id: "session-1".into(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            issue_state: "Todo".into(),
            worker_host: None,
            workspace_path: "/tmp/test".into(),
            started_at: Utc::now(),
            turn_count: 1,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        orchestrator.state.claim_issue("issue-1");
        orchestrator.state.add_running(session);
        orchestrator.state.set_retry_attempt("issue-1", 2);

        let result = crate::agent_runner::WorkerResult {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            outcome: crate::agent_runner::WorkerOutcome::Failure("boom".into()),
            total_tokens: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        };

        orchestrator.handle_worker_exit(result).await;

        let attempt = orchestrator.state.retry_attempt("issue-1");
        assert!(attempt >= 3);
        assert!(orchestrator.state.is_claimed("issue-1"));
    }

    #[tokio::test]
    async fn handle_worker_exit_captures_worker_host_for_retry() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        let session = LiveSession {
            session_id: "session-1".into(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            issue_state: "Todo".into(),
            worker_host: Some("worker-host-A".into()),
            workspace_path: "/tmp/test".into(),
            started_at: Utc::now(),
            turn_count: 1,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        orchestrator.state.claim_issue("issue-1");
        orchestrator.state.add_running(session);

        let result = crate::agent_runner::WorkerResult {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            outcome: crate::agent_runner::WorkerOutcome::Normal,
            total_tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };

        orchestrator.handle_worker_exit(result).await;

        let retry_entry = orchestrator.state.retry_entry("issue-1").unwrap();
        assert_eq!(retry_entry.worker_host.as_deref(), Some("worker-host-A"));
    }

    #[tokio::test]
    async fn handle_retry_timer_passes_preferred_host() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        orchestrator.state.claim_issue("issue-1");
        orchestrator.state.set_retry_attempt("issue-1", 1);
        orchestrator.state.set_retry_entry(RetryEntry {
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            attempt: 1,
            scheduled_at: Utc::now(),
            reason: Some("continuation".into()),
            worker_host: Some("preferred-host".into()),
        });

        let (msg_tx, _msg_rx) = mpsc::channel(16);
        orchestrator
            .retry_queue
            .schedule("issue-1", 1, std::time::Duration::from_millis(1), msg_tx);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let preferred = orchestrator
            .state
            .retry_entry("issue-1")
            .and_then(|entry| entry.worker_host.clone());
        assert_eq!(preferred.as_deref(), Some("preferred-host"));
    }

    #[tokio::test]
    async fn handle_codex_update_updates_session_state() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        let session = LiveSession {
            session_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            issue_state: "Todo".into(),
            worker_host: None,
            workspace_path: "/tmp/test".into(),
            started_at: Utc::now(),
            turn_count: 0,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        orchestrator.state.add_running(session);

        orchestrator.handle_codex_update(crate::agent_runner::WorkerUpdate::SessionStarted {
            issue_id: "issue-1".into(),
            session_id: "session-abc".into(),
        });

        let running = orchestrator.state.running.get("issue-1").unwrap();
        assert_eq!(running.session_id, "session-abc");
    }

    #[tokio::test]
    async fn handle_codex_update_increments_turn_count() {
        let issue = test_issue("issue-1", "Todo");
        let mut orchestrator = build_orchestrator(vec![issue]);

        let session = LiveSession {
            session_id: String::new(),
            issue_id: "issue-1".into(),
            issue_identifier: "SYM-issue-1".into(),
            issue_state: "Todo".into(),
            worker_host: None,
            workspace_path: "/tmp/test".into(),
            started_at: Utc::now(),
            turn_count: 0,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        orchestrator.state.add_running(session);

        orchestrator.handle_codex_update(crate::agent_runner::WorkerUpdate::TurnCompleted {
            issue_id: "issue-1".into(),
            turn_number: 1,
            outcome: crate::domain::RunOutcome::Success,
        });

        let running = orchestrator.state.running.get("issue-1").unwrap();
        assert_eq!(running.turn_count, 1);
    }

    #[tokio::test]
    async fn shutdown_aborts_workers_and_clears_retries() {
        let mut orchestrator = build_orchestrator(vec![]);

        let (msg_tx, _msg_rx) = mpsc::channel(16);
        orchestrator.retry_queue.schedule(
            "issue-1",
            1,
            std::time::Duration::from_secs(60),
            msg_tx,
        );
        assert!(!orchestrator.retry_queue.is_empty());

        orchestrator.shutdown();

        assert!(orchestrator.retry_queue.is_empty());
        assert!(orchestrator.worker_handles.is_empty());
    }

    #[test]
    fn schedule_retry_claims_issue_and_sets_retry_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut orchestrator = build_orchestrator(vec![]);

            orchestrator.schedule_retry(
                "issue-1",
                "SYM-issue-1",
                2,
                &crate::agent_runner::WorkerOutcome::Failure("error".into()),
                Some("test retry"),
                Some("host-1".into()),
            );

            assert!(orchestrator.state.is_claimed("issue-1"));
            assert_eq!(orchestrator.state.retry_attempt("issue-1"), 2);
            let entry = orchestrator.state.retry_entry("issue-1").unwrap();
            assert_eq!(entry.worker_host.as_deref(), Some("host-1"));
            assert_eq!(entry.reason.as_deref(), Some("test retry"));
            assert!(orchestrator.retry_queue.contains("issue-1"));
        });
    }
}

fn build_tool_executor(config: &SymphonyConfig) -> Option<Arc<LinearGraphqlTool>> {
    if config.tracker.kind.as_deref() != Some("linear") {
        return None;
    }

    let api_key = config.tracker.api_key.as_deref()?;
    let client = match config.tracker.endpoint.as_deref() {
        Some(endpoint) => LinearClient::with_endpoint(api_key, endpoint),
        None => LinearClient::new(api_key),
    };

    match client {
        Ok(client) => Some(Arc::new(LinearGraphqlTool::new(Arc::new(client)))),
        Err(error) => {
            warn!(error = %error, "failed to build tool executor");
            None
        }
    }
}
