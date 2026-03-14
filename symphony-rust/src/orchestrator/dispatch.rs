use crate::config::AgentConfig;
use crate::domain::Issue;

use super::state::OrchestratorState;

pub fn select_eligible<'a>(
    candidates: &'a [Issue],
    state: &OrchestratorState,
    config: &AgentConfig,
    active_states: &[String],
    terminal_states: &[String],
) -> Vec<&'a Issue> {
    candidates
        .iter()
        .filter(|issue| has_required_fields(issue))
        .filter(|issue| state_matches(&issue.state, active_states))
        .filter(|issue| !state_matches(&issue.state, terminal_states))
        .filter(|issue| !state.is_claimed(&issue.id))
        .filter(|_| available_global_slots(state, config.max_concurrent_agents) > 0)
        .filter(|issue| {
            available_state_slots(state, &issue.state, config)
                .map(|slots| slots > 0)
                .unwrap_or(true)
        })
        .filter(|issue| todo_blockers_satisfied(issue, terminal_states))
        .collect()
}

pub fn sort_candidates(candidates: &mut [&Issue]) {
    candidates.sort_by(|left, right| {
        priority_key(left)
            .cmp(&priority_key(right))
            .then_with(|| created_at_key(left).cmp(created_at_key(right)))
            .then_with(|| left.identifier.cmp(&right.identifier))
    });
}

pub fn available_global_slots(state: &OrchestratorState, max: u32) -> u32 {
    max.saturating_sub(u32::try_from(state.running_count()).unwrap_or(u32::MAX))
}

pub fn available_state_slots(
    state: &OrchestratorState,
    issue_state: &str,
    config: &AgentConfig,
) -> Option<u32> {
    let normalized = normalize_state(issue_state);
    config
        .max_concurrent_agents_by_state
        .get(&normalized)
        .map(|max| {
            max.saturating_sub(
                u32::try_from(state.running_count_by_state(issue_state)).unwrap_or(u32::MAX),
            )
        })
}

fn has_required_fields(issue: &Issue) -> bool {
    !issue.id.trim().is_empty()
        && !issue.identifier.trim().is_empty()
        && !issue.title.trim().is_empty()
        && !issue.state.trim().is_empty()
}

fn todo_blockers_satisfied(issue: &Issue, terminal_states: &[String]) -> bool {
    if !normalize_state(&issue.state).eq("todo") {
        return true;
    }

    issue
        .blocked_by
        .iter()
        .all(|blocker| state_matches(&blocker.state, terminal_states))
}

fn state_matches(issue_state: &str, states: &[String]) -> bool {
    let normalized = normalize_state(issue_state);
    states
        .iter()
        .any(|state| normalize_state(state) == normalized)
}

fn normalize_state(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn priority_key(issue: &Issue) -> (bool, i32) {
    match issue.priority {
        Some(priority) => (false, priority),
        None => (true, i32::MAX),
    }
}

fn created_at_key(issue: &Issue) -> &str {
    issue
        .created_at
        .as_deref()
        .unwrap_or("9999-99-99T99:99:99Z")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::config::AgentConfig;
    use crate::domain::{BlockerRef, Issue, LiveSession, TokenUsage};

    use super::{available_global_slots, available_state_slots, select_eligible, sort_candidates};
    use crate::orchestrator::state::OrchestratorState;

    fn issue(id: &str, state: &str, priority: Option<i32>, created_at: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: format!("SYM-{id}"),
            title: format!("Issue {id}"),
            description: None,
            priority,
            state: state.into(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: Some(created_at.into()),
            updated_at: None,
        }
    }

    fn session(issue_id: &str, state: &str) -> LiveSession {
        LiveSession {
            session_id: "session".into(),
            issue_id: issue_id.into(),
            issue_identifier: format!("SYM-{issue_id}"),
            issue_state: state.into(),
            workspace_path: "/tmp/workspace".into(),
            started_at: chrono::Utc::now(),
            turn_count: 0,
            last_codex_timestamp: None,
            tokens: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        }
    }

    fn config() -> AgentConfig {
        AgentConfig {
            max_concurrent_agents: 3,
            max_concurrent_agents_by_state: HashMap::from([(String::from("todo"), 1)]),
            max_turns: 5,
            max_retry_backoff_ms: 300_000,
        }
    }

    #[test]
    // SPEC 17.4: claimed issues and exhausted slots are not eligible for dispatch.
    fn select_eligible_filters_claimed_and_full_slots() {
        let mut state = OrchestratorState::default();
        state.claim_issue("claimed");
        state.add_running(session("running-1", "Todo"));
        state.add_running(session("running-2", "In Progress"));
        state.add_running(session("running-3", "In Progress"));

        let candidates = vec![
            issue("claimed", "Todo", Some(1), "2026-03-14T00:00:00Z"),
            issue("free", "Todo", Some(1), "2026-03-14T00:00:00Z"),
        ];

        let eligible = select_eligible(
            &candidates,
            &state,
            &config(),
            &[String::from("Todo"), String::from("In Progress")],
            &[String::from("Done")],
        );

        assert!(eligible.is_empty());
    }

    #[test]
    // SPEC 17.4: `Todo` issues blocked by non-terminal blockers are not eligible.
    fn select_eligible_requires_todo_blockers_to_be_terminal() {
        let state = OrchestratorState::default();
        let mut candidate = issue("todo", "Todo", Some(1), "2026-03-14T00:00:00Z");
        candidate.blocked_by = vec![BlockerRef {
            id: "blocker".into(),
            identifier: "SYM-0".into(),
            state: "In Progress".into(),
        }];
        let candidates = [candidate];

        let eligible = select_eligible(
            &candidates,
            &state,
            &config(),
            &[String::from("Todo"), String::from("In Progress")],
            &[String::from("Done"), String::from("Canceled")],
        );

        assert!(eligible.is_empty());
    }

    #[test]
    // SPEC 17.4: eligible active-state issues are admitted for dispatch.
    fn select_eligible_accepts_valid_issue() {
        let state = OrchestratorState::default();
        let candidate = issue("todo", "Todo", Some(1), "2026-03-14T00:00:00Z");
        let candidates = [candidate];

        let eligible = select_eligible(
            &candidates,
            &state,
            &config(),
            &[String::from("Todo"), String::from("In Progress")],
            &[String::from("Done"), String::from("Canceled")],
        );

        assert_eq!(eligible.len(), 1);
    }

    #[test]
    // SPEC 17.4: dispatch order is priority first, then oldest creation time, then identifier.
    fn sort_candidates_orders_by_priority_then_created_at_then_identifier() {
        let first = issue("2", "Todo", Some(2), "2026-03-15T00:00:00Z");
        let second = issue("1", "Todo", Some(1), "2026-03-16T00:00:00Z");
        let third = issue("3", "Todo", Some(1), "2026-03-14T00:00:00Z");
        let fourth = issue("4", "Todo", None, "2026-03-13T00:00:00Z");

        let mut candidates = vec![&first, &second, &third, &fourth];
        sort_candidates(&mut candidates);

        assert_eq!(candidates[0].id, "3");
        assert_eq!(candidates[1].id, "1");
        assert_eq!(candidates[2].id, "2");
        assert_eq!(candidates[3].id, "4");
    }

    #[test]
    // SPEC 17.4: global and per-state slot calculations enforce configured concurrency limits.
    fn available_slot_helpers_respect_limits() {
        let mut state = OrchestratorState::default();
        state.add_running(session("1", "Todo"));
        state.add_running(session("2", "In Progress"));

        assert_eq!(available_global_slots(&state, 3), 1);
        assert_eq!(available_state_slots(&state, "Todo", &config()), Some(0));
        assert_eq!(
            available_state_slots(&state, "In Progress", &config()),
            None
        );
    }
}
