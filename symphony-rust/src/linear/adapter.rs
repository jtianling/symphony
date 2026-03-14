use crate::domain::{BlockerRef, Issue};

use super::types::LinearIssue;

pub fn normalize_issue(linear: &LinearIssue) -> Issue {
    Issue {
        id: linear.id.clone(),
        identifier: linear.identifier.clone(),
        title: linear.title.clone().unwrap_or_default(),
        description: linear.description.clone(),
        priority: normalize_priority(linear.priority.as_ref()),
        state: extract_state_name(linear.state.as_ref()),
        branch_name: linear.branch_name.clone(),
        url: linear.url.clone(),
        labels: linear
            .labels
            .as_ref()
            .map(|labels| {
                labels
                    .nodes
                    .iter()
                    .filter_map(|label| label.name.as_deref())
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default(),
        blocked_by: linear
            .inverse_relations
            .as_ref()
            .map(|relations| {
                relations
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.relation_type
                            .as_deref()
                            .map(|t| t.trim().eq_ignore_ascii_case("blocks"))
                            .unwrap_or(false)
                    })
                    .filter_map(|node| node.issue.as_ref())
                    .map(|issue| BlockerRef {
                        id: issue.id.clone(),
                        identifier: issue.identifier.clone(),
                        state: extract_state_name(issue.state.as_ref()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        created_at: linear.created_at.clone(),
        updated_at: linear.updated_at.clone(),
    }
}

pub(crate) fn normalize_issue_ref(linear: &LinearIssue) -> BlockerRef {
    BlockerRef {
        id: linear.id.clone(),
        identifier: linear.identifier.clone(),
        state: extract_state_name(linear.state.as_ref()),
    }
}

fn extract_state_name(state: Option<&super::types::StateNode>) -> String {
    state
        .and_then(|value| value.name.clone())
        .unwrap_or_default()
}

fn normalize_priority(priority: Option<&serde_json::Value>) -> Option<i32> {
    priority
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::normalize_issue;
    use crate::linear::types::LinearIssue;

    #[test]
    // SPEC 17.3: blocker relations and labels are normalized from Linear issue payloads.
    fn normalize_issue_lowercases_labels_extracts_blockers_and_priority(
    ) -> Result<(), serde_json::Error> {
        let linear: LinearIssue = serde_json::from_value(json!({
            "id": "issue-1",
            "identifier": "SYM-1",
            "title": "Build client",
            "description": "Ship it",
            "priority": 2,
            "branchName": "feature/sym-1",
            "url": "https://linear.app/symphony/issue/SYM-1",
            "createdAt": "2026-03-14T00:00:00Z",
            "updatedAt": "2026-03-14T01:00:00Z",
            "state": { "name": "Todo" },
            "labels": {
                "nodes": [
                    { "name": "Backend" },
                    { "name": " Rust " },
                    { "name": "" }
                ]
            },
            "inverseRelations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "issue": {
                            "id": "issue-0",
                            "identifier": "SYM-0",
                            "state": { "name": "In Progress" }
                        }
                    },
                    {
                        "type": "related",
                        "issue": {
                            "id": "issue-2",
                            "identifier": "SYM-2",
                            "state": { "name": "Done" }
                        }
                    }
                ]
            }
        }))?;

        let normalized = normalize_issue(&linear);

        assert_eq!(normalized.labels, vec!["backend", "rust"]);
        assert_eq!(normalized.blocked_by.len(), 1);
        assert_eq!(normalized.blocked_by[0].identifier, "SYM-0");
        assert_eq!(normalized.priority, Some(2));
        assert_eq!(
            normalized.created_at.as_deref(),
            Some("2026-03-14T00:00:00Z")
        );

        Ok(())
    }

    #[test]
    // SPEC 17.3: non-numeric priority values are ignored during normalization.
    fn normalize_issue_ignores_non_numeric_priority() -> Result<(), serde_json::Error> {
        let linear: LinearIssue = serde_json::from_value(json!({
            "id": "issue-1",
            "identifier": "SYM-1",
            "priority": "high",
            "state": { "name": "Todo" }
        }))?;

        let normalized = normalize_issue(&linear);

        assert_eq!(normalized.priority, None);

        Ok(())
    }
}
