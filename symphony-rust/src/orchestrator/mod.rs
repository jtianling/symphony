use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::agent_runner::{run_worker, WorkerOutcome, WorkerResult, WorkerUpdate};
use crate::config::SymphonyConfig;
use crate::domain::{Issue, LiveSession, RetryEntry, WorkflowDefinition};
use crate::http::StateProvider;
use crate::linear::client::LinearClient;
use crate::prompt::PromptBuilder;
use crate::workspace::{default_workspace_root, WorkspaceManager};

mod dispatch;
mod reconciliation;
mod retry;
mod state;

pub use dispatch::{
    available_global_slots, available_state_slots, select_eligible, sort_candidates,
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

pub struct Orchestrator {
    state: OrchestratorState,
    config: SymphonyConfig,
    prompt_template: String,
    workspace_manager: Arc<WorkspaceManager>,
    prompt_builder: Arc<PromptBuilder>,
    tracker_client: Arc<LinearClient>,
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
        tracker_client: Arc<LinearClient>,
    ) -> Self {
        let (msg_tx, msg_rx) = mpsc::channel(512);

        Self {
            state: OrchestratorState::default(),
            config,
            prompt_template,
            workspace_manager,
            prompt_builder,
            tracker_client,
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
            self.tracker_client.as_ref(),
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

        let candidates = match self
            .tracker_client
            .fetch_candidates(&self.config.tracker)
            .await
        {
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

            self.dispatch_issue(issue.clone(), None).await;
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

    async fn dispatch_issue(&mut self, issue: Issue, attempt: Option<u32>) {
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let workspace_path = self.workspace_manager.workspace_path(&issue.identifier);
        let prompt_template = self.prompt_template.clone();
        let update_tx = self.spawn_update_forwarder();
        let worker_msg_tx = self.msg_tx.clone();
        let workspace_manager = Arc::clone(&self.workspace_manager);
        let prompt_builder = Arc::clone(&self.prompt_builder);
        let tracker_client = Arc::clone(&self.tracker_client);
        let agent_config = self.config.agent.clone();
        let codex_config = self.config.codex.clone();
        let active_states = self.config.tracker.active_states.clone();
        let issue_for_worker = issue.clone();

        let handle = tokio::spawn(async move {
            let result = run_worker(
                issue_for_worker,
                workspace_manager,
                prompt_builder,
                agent_config,
                codex_config,
                prompt_template,
                update_tx,
                tracker_client,
                active_states,
                attempt,
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
            "dispatched issue"
        );
    }

    async fn handle_worker_exit(&mut self, result: WorkerResult) {
        let issue_id = result.issue_id.clone();
        self.worker_handles.remove(&issue_id);

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

        let tracker_client = match LinearClient::from_config(&config.tracker) {
            Ok(client) => Arc::new(client),
            Err(error) => {
                warn!(error = %error, "failed to rebuild tracker client after reload");
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

        self.config = config;
        self.prompt_template = definition.prompt_template;
        self.tracker_client = tracker_client;
        self.workspace_manager = workspace_manager;

        info!("workflow reloaded");
    }

    async fn handle_retry_timer(&mut self, issue_id: &str) {
        let scheduled_attempt = self
            .retry_queue
            .attempt(issue_id)
            .unwrap_or_else(|| self.state.retry_attempt(issue_id).max(1));
        let _ = self.retry_queue.remove(issue_id);
        self.state.release_claim(issue_id);

        let candidates = match self
            .tracker_client
            .fetch_candidates(&self.config.tracker)
            .await
        {
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
            self.dispatch_issue(issue.clone(), Some(scheduled_attempt.max(1)))
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
            .tracker_client
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
                .cleanup_workspace(&issue.identifier)
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
