use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::TrackerConfig;
use crate::domain::{BlockerRef, Issue};
use crate::error::SymphonyError;
use crate::linear::client::LinearClient;

mod linear_adapter;
mod memory;

pub use linear_adapter::LinearAdapter;
pub use memory::{CommentRecord, MemoryTracker, StateUpdateRecord};

pub type TrackerFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Tracker: Send + Sync {
    fn fetch_candidates<'a>(
        &'a self,
        config: &'a TrackerConfig,
    ) -> TrackerFuture<'a, Result<Vec<Issue>, SymphonyError>>;

    fn refresh_issue_states<'a>(
        &'a self,
        issue_ids: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>>;

    fn fetch_issues_by_states<'a>(
        &'a self,
        project_slug: &'a str,
        states: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>>;

    fn create_comment<'a>(
        &'a self,
        issue_id: &'a str,
        body: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>>;

    fn update_issue_state<'a>(
        &'a self,
        issue_id: &'a str,
        state_name: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>>;
}

pub fn build_tracker(
    config: &TrackerConfig,
) -> Result<Arc<dyn Tracker + Send + Sync>, SymphonyError> {
    let kind = config
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SymphonyError::ConfigValidation("tracker.kind is required".into()))?;

    match kind {
        "linear" => {
            let client = Arc::new(LinearClient::from_config(config)?);
            Ok(Arc::new(LinearAdapter::new(client, config.clone())))
        }
        "memory" => Ok(Arc::new(MemoryTracker::default())),
        _ => Err(SymphonyError::ConfigValidation(
            "tracker.kind must equal \"linear\" or \"memory\"".into(),
        )),
    }
}
