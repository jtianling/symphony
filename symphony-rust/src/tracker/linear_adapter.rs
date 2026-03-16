use std::sync::Arc;

use crate::config::TrackerConfig;
use crate::domain::{BlockerRef, Issue};
use crate::error::SymphonyError;
use crate::linear::client::LinearClient;

use super::{Tracker, TrackerFuture};

#[derive(Debug, Clone)]
pub struct LinearAdapter {
    client: Arc<LinearClient>,
    config: TrackerConfig,
}

impl LinearAdapter {
    pub fn new(client: Arc<LinearClient>, config: TrackerConfig) -> Self {
        Self { client, config }
    }

    pub fn client(&self) -> Arc<LinearClient> {
        Arc::clone(&self.client)
    }

    pub fn config(&self) -> &TrackerConfig {
        &self.config
    }
}

impl Tracker for LinearAdapter {
    fn fetch_candidates<'a>(
        &'a self,
        config: &'a TrackerConfig,
    ) -> TrackerFuture<'a, Result<Vec<Issue>, SymphonyError>> {
        Box::pin(async move { self.client.fetch_candidates(config).await })
    }

    fn refresh_issue_states<'a>(
        &'a self,
        issue_ids: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>> {
        Box::pin(async move { self.client.refresh_issue_states(issue_ids).await })
    }

    fn fetch_issues_by_states<'a>(
        &'a self,
        project_slug: &'a str,
        states: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>> {
        Box::pin(async move {
            self.client
                .fetch_issues_by_states(project_slug, states)
                .await
        })
    }

    fn create_comment<'a>(
        &'a self,
        issue_id: &'a str,
        body: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>> {
        Box::pin(async move { self.client.create_comment(issue_id, body).await })
    }

    fn update_issue_state<'a>(
        &'a self,
        issue_id: &'a str,
        state_name: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>> {
        Box::pin(async move { self.client.update_issue_state(issue_id, state_name).await })
    }
}
