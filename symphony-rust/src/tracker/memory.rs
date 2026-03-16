use std::sync::{Arc, Mutex};

use crate::config::TrackerConfig;
use crate::domain::{BlockerRef, Issue};
use crate::error::SymphonyError;

use super::{Tracker, TrackerFuture};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRecord {
    pub issue_id: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateUpdateRecord {
    pub issue_id: String,
    pub state_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryTracker {
    issues: Arc<Mutex<Vec<Issue>>>,
    comments: Arc<Mutex<Vec<CommentRecord>>>,
    state_updates: Arc<Mutex<Vec<StateUpdateRecord>>>,
}

impl MemoryTracker {
    pub fn new(issues: Vec<Issue>) -> Self {
        Self {
            issues: Arc::new(Mutex::new(issues)),
            comments: Arc::new(Mutex::new(Vec::new())),
            state_updates: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn issues(&self) -> Result<Vec<Issue>, SymphonyError> {
        self.issues
            .lock()
            .map(|issues| issues.clone())
            .map_err(|_| SymphonyError::Tracker("memory_tracker_lock_failed".into()))
    }

    pub fn comments(&self) -> Result<Vec<CommentRecord>, SymphonyError> {
        self.comments
            .lock()
            .map(|comments| comments.clone())
            .map_err(|_| SymphonyError::Tracker("memory_tracker_lock_failed".into()))
    }

    pub fn state_updates(&self) -> Result<Vec<StateUpdateRecord>, SymphonyError> {
        self.state_updates
            .lock()
            .map(|updates| updates.clone())
            .map_err(|_| SymphonyError::Tracker("memory_tracker_lock_failed".into()))
    }
}

impl Tracker for MemoryTracker {
    fn fetch_candidates<'a>(
        &'a self,
        _config: &'a TrackerConfig,
    ) -> TrackerFuture<'a, Result<Vec<Issue>, SymphonyError>> {
        Box::pin(async move { self.issues() })
    }

    fn refresh_issue_states<'a>(
        &'a self,
        issue_ids: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>> {
        Box::pin(async move {
            let issues = self.issues()?;

            Ok(issues
                .into_iter()
                .filter(|issue| issue_ids.iter().any(|issue_id| issue_id == &issue.id))
                .map(|issue| BlockerRef {
                    id: issue.id,
                    identifier: issue.identifier,
                    state: issue.state,
                })
                .collect())
        })
    }

    fn fetch_issues_by_states<'a>(
        &'a self,
        _project_slug: &'a str,
        states: &'a [String],
    ) -> TrackerFuture<'a, Result<Vec<BlockerRef>, SymphonyError>> {
        Box::pin(async move {
            if states.is_empty() {
                return Ok(Vec::new());
            }

            let issues = self.issues()?;

            Ok(issues
                .into_iter()
                .filter(|issue| state_matches(&issue.state, states))
                .map(|issue| BlockerRef {
                    id: issue.id,
                    identifier: issue.identifier,
                    state: issue.state,
                })
                .collect())
        })
    }

    fn create_comment<'a>(
        &'a self,
        issue_id: &'a str,
        body: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>> {
        Box::pin(async move {
            self.comments
                .lock()
                .map_err(|_| SymphonyError::Tracker("memory_tracker_lock_failed".into()))?
                .push(CommentRecord {
                    issue_id: issue_id.to_owned(),
                    body: body.to_owned(),
                });

            Ok(())
        })
    }

    fn update_issue_state<'a>(
        &'a self,
        issue_id: &'a str,
        state_name: &'a str,
    ) -> TrackerFuture<'a, Result<(), SymphonyError>> {
        Box::pin(async move {
            self.state_updates
                .lock()
                .map_err(|_| SymphonyError::Tracker("memory_tracker_lock_failed".into()))?
                .push(StateUpdateRecord {
                    issue_id: issue_id.to_owned(),
                    state_name: state_name.to_owned(),
                });

            Ok(())
        })
    }
}

fn state_matches(current_state: &str, states: &[String]) -> bool {
    let normalized_current = current_state.trim().to_ascii_lowercase();
    states
        .iter()
        .any(|state| state.trim().to_ascii_lowercase() == normalized_current)
}

#[cfg(test)]
mod tests {
    use crate::config::TrackerConfig;
    use crate::domain::Issue;

    use super::{CommentRecord, MemoryTracker, StateUpdateRecord};
    use crate::tracker::Tracker;

    fn issue(id: &str, identifier: &str, state: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: identifier.into(),
            title: format!("Issue {identifier}"),
            description: None,
            priority: None,
            state: state.into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn fetch_candidates_returns_configured_issues() {
        let tracker = MemoryTracker::new(vec![
            issue("issue-1", "SYM-1", "Todo"),
            issue("issue-2", "SYM-2", "In Progress"),
        ]);

        let issues = tracker
            .fetch_candidates(&TrackerConfig::default())
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "SYM-1");
        assert_eq!(issues[1].identifier, "SYM-2");
    }

    #[tokio::test]
    async fn fetch_issues_by_states_filters_case_insensitively() {
        let tracker = MemoryTracker::new(vec![
            issue("issue-1", "SYM-1", "Todo"),
            issue("issue-2", "SYM-2", "In Progress"),
            issue("issue-3", "SYM-3", "Done"),
        ]);

        let issues = tracker
            .fetch_issues_by_states("project", &[String::from("todo"), String::from("done")])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "SYM-1");
        assert_eq!(issues[1].identifier, "SYM-3");
    }

    #[tokio::test]
    async fn refresh_issue_states_returns_only_matching_ids() {
        let tracker = MemoryTracker::new(vec![
            issue("issue-1", "SYM-1", "Todo"),
            issue("issue-2", "SYM-2", "In Progress"),
            issue("issue-3", "SYM-3", "Done"),
        ]);

        let issues = tracker
            .refresh_issue_states(&[String::from("issue-2"), String::from("issue-3")])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "SYM-2");
        assert_eq!(issues[1].identifier, "SYM-3");
    }

    #[tokio::test]
    async fn write_operations_are_recorded() {
        let tracker = MemoryTracker::new(vec![issue("issue-1", "SYM-1", "Todo")]);

        tracker.create_comment("issue-1", "hello").await.unwrap();
        tracker.update_issue_state("issue-1", "Done").await.unwrap();

        assert_eq!(
            tracker.comments().unwrap(),
            vec![CommentRecord {
                issue_id: String::from("issue-1"),
                body: String::from("hello"),
            }]
        );
        assert_eq!(
            tracker.state_updates().unwrap(),
            vec![StateUpdateRecord {
                issue_id: String::from("issue-1"),
                state_name: String::from("Done"),
            }]
        );
    }
}
