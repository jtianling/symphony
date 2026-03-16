use chrono::{DateTime, Utc};

use crate::orchestrator::{RunningSnapshot, StateSnapshot};

const PANEL_WIDTH: usize = 112;
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const BOLD: &str = "\x1b[1m";

pub fn render_panel(snapshot: &StateSnapshot, tps: f64) -> String {
    render_panel_with_context(
        snapshot,
        tps,
        "",
        u32::try_from(snapshot.counts.running).unwrap_or(u32::MAX),
    )
}

pub(crate) fn render_panel_with_context(
    snapshot: &StateSnapshot,
    tps: f64,
    sparkline: &str,
    max_agents: u32,
) -> String {
    let mut lines = vec![
        border('┌', '┐'),
        panel_line(&format!("{BOLD}{CYAN}Symphony Status Dashboard{RESET}")),
        panel_line(&render_header(snapshot, tps, max_agents)),
    ];

    if !sparkline.is_empty() {
        lines.push(panel_line(&format!(
            "{MAGENTA}Throughput 10m{RESET}: {sparkline}"
        )));
    }

    lines.push(border('├', '┤'));
    lines.push(panel_line(&format!("{CYAN}Running Issues{RESET}")));
    lines.push(panel_line(
        "Identifier         State              Turns      Tokens        Age",
    ));

    if snapshot.running.is_empty() {
        lines.push(panel_line("No active agents"));
    } else {
        lines.extend(
            snapshot
                .running
                .iter()
                .map(|entry| panel_line(&render_running_row(snapshot, entry))),
        );
    }

    lines.push(border('├', '┤'));
    lines.push(panel_line(&format!("{YELLOW}Backoff Queue{RESET}")));
    lines.push(panel_line(
        "Identifier         Attempt    Scheduled               Reason",
    ));

    if snapshot.retrying.is_empty() {
        lines.push(panel_line("No entries"));
    } else {
        lines.extend(snapshot.retrying.iter().map(|entry| {
            panel_line(&format!(
                "{:<18} {:>7}    {:<23} {}",
                truncate(&entry.issue_identifier, 18),
                entry.attempt,
                format_timestamp(entry.scheduled_at),
                truncate(entry.reason.as_deref().unwrap_or("n/a"), 40),
            ))
        }));
    }

    lines.push(border('└', '┘'));
    lines.join("\n")
}

fn render_header(snapshot: &StateSnapshot, tps: f64, max_agents: u32) -> String {
    let runtime = format_duration(snapshot.codex_totals.seconds_running);
    let limits = render_rate_limits(snapshot.rate_limits.as_ref());

    format!(
        "{GREEN}Agents{RESET}: {}/{}  {GREEN}Throughput{RESET}: {:.1} tps  \
{GREEN}Runtime{RESET}: {}  {GREEN}Tokens{RESET}: in {} | out {} | total {}  \
{GREEN}Rate Limits{RESET}: {}",
        snapshot.counts.running,
        max_agents,
        tps,
        runtime,
        snapshot.codex_totals.input_tokens,
        snapshot.codex_totals.output_tokens,
        snapshot.codex_totals.total_tokens,
        limits,
    )
}

fn render_running_row(snapshot: &StateSnapshot, entry: &RunningSnapshot) -> String {
    let age_seconds = snapshot
        .generated_at
        .signed_duration_since(entry.started_at)
        .num_milliseconds() as f64
        / 1000.0;

    format!(
        "{:<18} {:<18} {:>5} {:>11} {:>10}",
        truncate(&entry.issue_identifier, 18),
        truncate(&entry.state, 18),
        entry.turn_count,
        entry.tokens.total_tokens,
        format_duration(age_seconds),
    )
}

fn render_rate_limits(value: Option<&serde_json::Value>) -> String {
    match value {
        None => String::from("n/a"),
        Some(serde_json::Value::Object(object)) => {
            let keys = ["remaining", "limit", "reset_seconds"];
            let summary = keys
                .iter()
                .filter_map(|key| object.get(*key).map(|value| format!("{key}={value}")))
                .collect::<Vec<_>>();

            if summary.is_empty() {
                truncate(&serde_json::to_string(object).unwrap_or_default(), 36)
            } else {
                truncate(&summary.join(", "), 36)
            }
        }
        Some(other) => truncate(&other.to_string(), 36),
    }
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn format_duration(seconds: f64) -> String {
    let total_seconds = seconds.max(0.0).round() as i64;

    if total_seconds < 60 {
        return format!("{total_seconds}s");
    }

    let minutes = total_seconds / 60;
    let remaining_seconds = total_seconds % 60;

    if minutes < 60 {
        return format!("{minutes}m {remaining_seconds}s");
    }

    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;

    format!("{hours}h {remaining_minutes}m")
}

fn border(left: char, right: char) -> String {
    format!("{left}{}{right}", "─".repeat(PANEL_WIDTH + 2))
}

fn panel_line(content: &str) -> String {
    let visible_width = visible_width(content);
    let padding = PANEL_WIDTH.saturating_sub(visible_width);

    format!("│ {content}{} │", " ".repeat(padding))
}

fn visible_width(value: &str) -> usize {
    let mut width = 0;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }

        width += 1;
    }

    width
}

fn truncate(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_owned();
    }

    if width <= 3 {
        return value.chars().take(width).collect();
    }

    let prefix = value.chars().take(width - 3).collect::<String>();
    format!("{prefix}...")
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;

    use super::render_panel;
    use crate::domain::{RetryEntry, TokenUsage};
    use crate::orchestrator::{AggregateTokens, RunningSnapshot, SnapshotCounts, StateSnapshot};

    fn sample_snapshot() -> StateSnapshot {
        let generated_at = Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap();

        StateSnapshot {
            generated_at,
            counts: SnapshotCounts {
                running: 1,
                claimed: 1,
                completed: 0,
                retrying: 1,
            },
            running: vec![RunningSnapshot {
                issue_id: String::from("issue-1"),
                issue_identifier: String::from("SYM-101"),
                state: String::from("in_progress"),
                worker_host: None,
                session_id: String::from("session-1"),
                turn_count: 3,
                workspace_path: String::from("/tmp/issue-1"),
                started_at: generated_at - Duration::seconds(42),
                last_event_at: None,
                tokens: TokenUsage {
                    input_tokens: 40,
                    output_tokens: 60,
                    total_tokens: 100,
                },
            }],
            retrying: vec![RetryEntry {
                issue_id: String::from("issue-2"),
                issue_identifier: String::from("SYM-202"),
                attempt: 2,
                scheduled_at: generated_at + Duration::seconds(30),
                reason: Some(String::from("backoff")),
                worker_host: None,
            }],
            codex_totals: AggregateTokens {
                input_tokens: 400,
                output_tokens: 500,
                total_tokens: 900,
                seconds_running: 123.0,
            },
            rate_limits: Some(json!({ "remaining": 42, "limit": 100 })),
        }
    }

    #[test]
    fn render_panel_shows_empty_state() {
        let mut snapshot = sample_snapshot();
        snapshot.counts.running = 0;
        snapshot.counts.retrying = 0;
        snapshot.running.clear();
        snapshot.retrying.clear();

        let panel = render_panel(&snapshot, 0.0);

        assert!(panel.contains("No active agents"));
        assert!(panel.contains("No entries"));
    }

    #[test]
    fn render_panel_shows_running_sessions_and_retry_entries() {
        let panel = render_panel(&sample_snapshot(), 4.2);

        assert!(panel.contains("SYM-101"));
        assert!(panel.contains("in_progress"));
        assert!(panel.contains("SYM-202"));
        assert!(panel.contains("backoff"));
    }

    #[test]
    fn render_panel_shows_token_statistics() {
        let panel = render_panel(&sample_snapshot(), 7.5);

        assert!(panel.contains("Throughput"));
        assert!(panel.contains("7.5 tps"));
        assert!(panel.contains("in 400 | out 500 | total 900"));
        assert!(panel.contains("remaining=42, limit=100"));
    }
}
