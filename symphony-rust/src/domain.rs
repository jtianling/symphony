use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<BlockerRef>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerRef {
    pub id: String,
    pub identifier: String,
    pub state: String,
}

pub fn sanitize_workspace_key(identifier: &str) -> String {
    identifier
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub config: serde_yaml::Value,
    pub prompt_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAttempt {
    pub attempt_number: u32,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub outcome: Option<RunOutcome>,
    pub session_id: Option<String>,
    pub tokens: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunOutcome {
    Success,
    Failure(String),
    Timeout,
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub session_id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub issue_state: String,
    pub workspace_path: String,
    pub started_at: DateTime<Utc>,
    pub turn_count: u32,
    pub last_codex_timestamp: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryEntry {
    pub issue_id: String,
    pub issue_identifier: String,
    pub attempt: u32,
    pub scheduled_at: DateTime<Utc>,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{sanitize_workspace_key, BlockerRef, Issue};

    #[test]
    fn issue_deserializes_normalized_fields() -> Result<(), serde_json::Error> {
        let issue: Issue = serde_json::from_value(json!({
            "id": "issue-id",
            "identifier": "ABC-123",
            "title": "Implement domain model",
            "description": "Normalize issue payload",
            "priority": 1,
            "state": "Todo",
            "branch_name": "feature/abc-123",
            "url": "https://example.com/issues/ABC-123",
            "labels": ["backend", "rust"],
            "blocked_by": [
                {
                    "id": "blocker-id",
                    "identifier": "ABC-100",
                    "state": "In Progress"
                }
            ],
            "created_at": "2026-03-14T00:00:00Z",
            "updated_at": "2026-03-14T01:00:00Z"
        }))?;

        assert_eq!(issue.id, "issue-id");
        assert_eq!(issue.identifier, "ABC-123");
        assert_eq!(issue.labels, vec!["backend", "rust"]);
        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].identifier, "ABC-100");
        assert_eq!(issue.updated_at.as_deref(), Some("2026-03-14T01:00:00Z"));

        Ok(())
    }

    #[test]
    fn blocker_ref_deserializes_normalized_fields() -> Result<(), serde_json::Error> {
        let blocker: BlockerRef = serde_json::from_value(json!({
            "id": "blocker-id",
            "identifier": "ABC-100",
            "state": "Done"
        }))?;

        assert_eq!(blocker.id, "blocker-id");
        assert_eq!(blocker.identifier, "ABC-100");
        assert_eq!(blocker.state, "Done");

        Ok(())
    }

    #[test]
    fn sanitize_workspace_key_keeps_standard_identifier() {
        assert_eq!(sanitize_workspace_key("ABC-123"), "ABC-123");
    }

    #[test]
    fn sanitize_workspace_key_replaces_special_characters() {
        assert_eq!(sanitize_workspace_key("ABC/123"), "ABC_123");
    }

    #[test]
    fn sanitize_workspace_key_preserves_dots_and_dashes() {
        assert_eq!(sanitize_workspace_key("my.project-1"), "my.project-1");
    }

    #[test]
    fn sanitize_workspace_key_replaces_spaces() {
        assert_eq!(sanitize_workspace_key("A B C"), "A_B_C");
    }

    #[test]
    fn sanitize_workspace_key_replaces_unicode_characters() {
        assert_eq!(sanitize_workspace_key("项目-1"), "__-1");
    }

    #[test]
    fn sanitize_workspace_key_keeps_empty_string() {
        assert_eq!(sanitize_workspace_key(""), "");
    }
}
