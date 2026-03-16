use chrono::{DateTime, Utc};

use crate::domain::RetryEntry;
use crate::orchestrator::{RunningSnapshot, StateSnapshot};

const DASHBOARD_REFRESH_SECONDS: u64 = 5;

pub fn render_dashboard(snapshot: &StateSnapshot) -> String {
    let now = Utc::now();
    let running_rows = render_running_rows(&snapshot.running, now);
    let retry_rows = render_retry_rows(&snapshot.retrying);

    format!(
        concat!(
            "<!DOCTYPE html>",
            "<html lang=\"en\">",
            "<head>",
            "<meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
            "<meta id=\"dashboard-refresh\" http-equiv=\"refresh\" content=\"{refresh_seconds}\">",
            "<title>Symphony Dashboard</title>",
            "<style>",
            "body{{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;",
            "margin:0;background:#f5f7fb;color:#172033;}}",
            ".page{{max-width:1200px;margin:0 auto;padding:32px 20px 48px;}}",
            "h1{{margin:0 0 8px;font-size:32px;}}",
            ".muted{{color:#5b6475;font-size:14px;}}",
            ".grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));",
            "gap:16px;margin:24px 0;}}",
            ".card{{background:#fff;border:1px solid #d9deea;border-radius:12px;padding:18px;}}",
            ".card h2{{margin:0 0 8px;font-size:14px;color:#5b6475;text-transform:uppercase;",
            "letter-spacing:.04em;}}",
            ".card p{{margin:0;font-size:28px;font-weight:700;}}",
            ".section{{margin-top:28px;}}",
            ".section h2{{margin:0 0 12px;font-size:20px;}}",
            "table{{width:100%;border-collapse:collapse;background:#fff;border:1px solid #d9deea;",
            "border-radius:12px;overflow:hidden;}}",
            "th,td{{padding:12px 14px;border-bottom:1px solid #e8ecf5;text-align:left;",
            "font-size:14px;vertical-align:top;}}",
            "th{{background:#eef2fb;color:#46506a;}}",
            "tr:last-child td{{border-bottom:none;}}",
            ".empty{{color:#7a8397;}}",
            "</style>",
            "</head>",
            "<body>",
            "<main class=\"page\">",
            "<h1>Symphony</h1>",
            "<p class=\"muted\">Generated at <span id=\"generated-at\">{generated_at}</span></p>",
            "<section class=\"grid\">",
            "<article class=\"card\"><h2>Running</h2><p id=\"running-count\">{running_count}</p></article>",
            "<article class=\"card\"><h2>Retrying</h2><p id=\"retrying-count\">{retrying_count}</p></article>",
            "<article class=\"card\"><h2>Input Tokens</h2><p id=\"input-tokens\">{input_tokens}</p></article>",
            "<article class=\"card\"><h2>Output Tokens</h2><p id=\"output-tokens\">{output_tokens}</p></article>",
            "<article class=\"card\"><h2>Total Tokens</h2><p id=\"total-tokens\">{total_tokens}</p></article>",
            "<article class=\"card\"><h2>Seconds Running</h2><p id=\"seconds-running\">{seconds_running:.1}</p></article>",
            "</section>",
            "<section class=\"section\">",
            "<h2>Running Sessions</h2>",
            "<table><thead><tr>",
            "<th>Issue</th><th>State</th><th>Turns</th><th>Elapsed</th><th>Workspace</th>",
            "</tr></thead><tbody id=\"running-rows\">{running_rows}</tbody></table>",
            "</section>",
            "<section class=\"section\">",
            "<h2>Retry Queue</h2>",
            "<table><thead><tr>",
            "<th>Issue</th><th>Attempt</th><th>Scheduled At</th><th>Reason</th>",
            "</tr></thead><tbody id=\"retry-rows\">{retry_rows}</tbody></table>",
            "</section>",
            "</main>",
            "<script>",
            "const refreshMetaId='dashboard-refresh';",
            "const refreshSeconds='{refresh_seconds}';",
            "function ensureRefreshFallback(){{",
            "let meta=document.getElementById(refreshMetaId);",
            "if(meta){{meta.content=refreshSeconds;return;}}",
            "meta=document.createElement('meta');",
            "meta.id=refreshMetaId;",
            "meta.httpEquiv='refresh';",
            "meta.content=refreshSeconds;",
            "document.head.appendChild(meta);",
            "}}",
            "function disableRefreshFallback(){{",
            "const meta=document.getElementById(refreshMetaId);",
            "if(meta){{meta.remove();}}",
            "}}",
            "function escapeHtml(value){{",
            "return String(value??'')",
            ".replaceAll('&','&amp;')",
            ".replaceAll('<','&lt;')",
            ".replaceAll('>','&gt;')",
            ".replaceAll('\"','&quot;')",
            ".replaceAll(\"'\",'&#39;');",
            "}}",
            "function formatElapsed(startedAt, generatedAt){{",
            "const start=Date.parse(startedAt);",
            "const now=Date.parse(generatedAt);",
            "if(Number.isNaN(start)||Number.isNaN(now)){{return '-';}}",
            "const totalSeconds=Math.max(0,Math.floor((now-start)/1000));",
            "const minutes=Math.floor(totalSeconds/60);",
            "const seconds=totalSeconds%60;",
            "if(minutes>0){{return `${{minutes}}m ${{seconds}}s`;}}",
            "return `${{seconds}}s`;",
            "}}",
            "function renderRunningRows(snapshot){{",
            "const entries=Array.isArray(snapshot.running)?snapshot.running:[];",
            "if(entries.length===0){{",
            "return '<tr><td colspan=\"5\" class=\"empty\">No running sessions</td></tr>';",
            "}}",
            "return entries.map((entry)=>`<tr><td>${{escapeHtml(entry.issue_identifier)}}</td>",
            "<td>${{escapeHtml(entry.state)}}</td><td>${{escapeHtml(entry.turn_count)}}</td>",
            "<td>${{escapeHtml(formatElapsed(entry.started_at,snapshot.generated_at))}}</td>",
            "<td>${{escapeHtml(entry.workspace_path)}}</td></tr>`).join('');",
            "}}",
            "function renderRetryRows(snapshot){{",
            "const entries=Array.isArray(snapshot.retrying)?snapshot.retrying:[];",
            "if(entries.length===0){{",
            "return '<tr><td colspan=\"4\" class=\"empty\">No retry entries</td></tr>';",
            "}}",
            "return entries.map((entry)=>`<tr><td>${{escapeHtml(entry.issue_identifier)}}</td>",
            "<td>${{escapeHtml(entry.attempt)}}</td><td>${{escapeHtml(entry.scheduled_at)}}</td>",
            "<td>${{escapeHtml(entry.reason??'-')}}</td></tr>`).join('');",
            "}}",
            "function updateDashboard(snapshot){{",
            "document.getElementById('generated-at').textContent=snapshot.generated_at??'-';",
            "document.getElementById('running-count').textContent=snapshot.counts?.running??0;",
            "document.getElementById('retrying-count').textContent=snapshot.counts?.retrying??0;",
            "document.getElementById('input-tokens').textContent=",
            "snapshot.codex_totals?.input_tokens??0;",
            "document.getElementById('output-tokens').textContent=",
            "snapshot.codex_totals?.output_tokens??0;",
            "document.getElementById('total-tokens').textContent=",
            "snapshot.codex_totals?.total_tokens??0;",
            "document.getElementById('seconds-running').textContent=",
            "Number(snapshot.codex_totals?.seconds_running??0).toFixed(1);",
            "document.getElementById('running-rows').innerHTML=renderRunningRows(snapshot);",
            "document.getElementById('retry-rows').innerHTML=renderRetryRows(snapshot);",
            "}}",
            "if(window.EventSource){{",
            "const source=new EventSource('/api/v1/events');",
            "source.addEventListener('state',(event)=>{{",
            "disableRefreshFallback();",
            "updateDashboard(JSON.parse(event.data));",
            "}});",
            "source.onerror=()=>{{",
            "ensureRefreshFallback();",
            "source.close();",
            "}};",
            "}}else{{",
            "ensureRefreshFallback();",
            "}}",
            "</script>",
            "</body>",
            "</html>"
        ),
        refresh_seconds = DASHBOARD_REFRESH_SECONDS,
        generated_at = escape_html(&snapshot.generated_at.to_rfc3339()),
        running_count = snapshot.running.len(),
        retrying_count = snapshot.retrying.len(),
        input_tokens = snapshot.codex_totals.input_tokens,
        output_tokens = snapshot.codex_totals.output_tokens,
        total_tokens = snapshot.codex_totals.total_tokens,
        seconds_running = snapshot.codex_totals.seconds_running,
        running_rows = running_rows,
        retry_rows = retry_rows,
    )
}

fn render_running_rows(entries: &[RunningSnapshot], now: DateTime<Utc>) -> String {
    if entries.is_empty() {
        return String::from("<tr><td colspan=\"5\" class=\"empty\">No running sessions</td></tr>");
    }

    entries
        .iter()
        .map(|entry| {
            format!(
                concat!(
                    "<tr>",
                    "<td>{issue}</td>",
                    "<td>{state}</td>",
                    "<td>{turn_count}</td>",
                    "<td>{elapsed}</td>",
                    "<td>{workspace}</td>",
                    "</tr>"
                ),
                issue = escape_html(&entry.issue_identifier),
                state = escape_html(&entry.state),
                turn_count = entry.turn_count,
                elapsed = escape_html(&format_elapsed(entry.started_at, now)),
                workspace = escape_html(&entry.workspace_path),
            )
        })
        .collect()
}

fn render_retry_rows(entries: &[RetryEntry]) -> String {
    if entries.is_empty() {
        return String::from("<tr><td colspan=\"4\" class=\"empty\">No retry entries</td></tr>");
    }

    entries
        .iter()
        .map(|entry| {
            format!(
                concat!(
                    "<tr>",
                    "<td>{issue}</td>",
                    "<td>{attempt}</td>",
                    "<td>{scheduled_at}</td>",
                    "<td>{reason}</td>",
                    "</tr>"
                ),
                issue = escape_html(&entry.issue_identifier),
                attempt = entry.attempt,
                scheduled_at = escape_html(&entry.scheduled_at.to_rfc3339()),
                reason = escape_html(entry.reason.as_deref().unwrap_or("-")),
            )
        })
        .collect()
}

fn format_elapsed(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(started_at).num_seconds().max(0);
    let minutes = seconds / 60;
    let remaining_seconds = seconds % 60;

    if minutes > 0 {
        return format!("{minutes}m {remaining_seconds}s");
    }

    format!("{remaining_seconds}s")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use super::{escape_html, format_elapsed, render_dashboard};
    use crate::domain::{RetryEntry, TokenUsage};
    use crate::orchestrator::{AggregateTokens, RunningSnapshot, SnapshotCounts, StateSnapshot};

    fn empty_snapshot() -> StateSnapshot {
        StateSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: 0,
                claimed: 0,
                completed: 0,
                retrying: 0,
            },
            running: Vec::new(),
            retrying: Vec::new(),
            codex_totals: AggregateTokens::default(),
            rate_limits: None,
        }
    }

    fn populated_snapshot() -> StateSnapshot {
        let now = Utc::now();
        StateSnapshot {
            generated_at: now,
            counts: SnapshotCounts {
                running: 1,
                claimed: 1,
                completed: 0,
                retrying: 1,
            },
            running: vec![RunningSnapshot {
                issue_id: "issue-1".into(),
                issue_identifier: "SYM-1".into(),
                state: "In Progress".into(),
                worker_host: Some("host1".into()),
                session_id: "session-1".into(),
                turn_count: 3,
                workspace_path: "/tmp/ws".into(),
                started_at: now - Duration::seconds(120),
                last_event_at: Some(now),
                tokens: TokenUsage {
                    input_tokens: 50,
                    output_tokens: 25,
                    total_tokens: 75,
                },
            }],
            retrying: vec![RetryEntry {
                issue_id: "issue-2".into(),
                issue_identifier: "SYM-2".into(),
                attempt: 2,
                scheduled_at: now + Duration::seconds(30),
                reason: Some("backoff".into()),
                worker_host: None,
            }],
            codex_totals: AggregateTokens {
                input_tokens: 500,
                output_tokens: 300,
                total_tokens: 800,
                seconds_running: 45.7,
            },
            rate_limits: Some(json!({"remaining": 42})),
        }
    }

    #[test]
    fn render_dashboard_produces_valid_html() {
        let html = render_dashboard(&empty_snapshot());

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html"));
        assert!(html.contains("</html>"));
        assert!(html.contains("Symphony"));
    }

    #[test]
    fn render_dashboard_shows_empty_state_messages() {
        let html = render_dashboard(&empty_snapshot());

        assert!(html.contains("No running sessions"));
        assert!(html.contains("No retry entries"));
    }

    #[test]
    fn render_dashboard_includes_running_sessions() {
        let html = render_dashboard(&populated_snapshot());

        assert!(html.contains("SYM-1"));
        assert!(html.contains("In Progress"));
        assert!(html.contains("/tmp/ws"));
    }

    #[test]
    fn render_dashboard_includes_retry_entries() {
        let html = render_dashboard(&populated_snapshot());

        assert!(html.contains("SYM-2"));
        assert!(html.contains("backoff"));
    }

    #[test]
    fn render_dashboard_includes_token_totals() {
        let html = render_dashboard(&populated_snapshot());

        assert!(html.contains("500"));
        assert!(html.contains("300"));
        assert!(html.contains("800"));
    }

    #[test]
    fn render_dashboard_includes_meta_refresh() {
        let html = render_dashboard(&empty_snapshot());

        assert!(html.contains("http-equiv=\"refresh\""));
    }

    #[test]
    fn render_dashboard_includes_sse_script() {
        let html = render_dashboard(&empty_snapshot());

        assert!(html.contains("EventSource"));
        assert!(html.contains("/api/v1/events"));
    }

    #[test]
    fn escape_html_handles_special_characters() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("\"quotes\""), "&quot;quotes&quot;");
        assert_eq!(escape_html("'single'"), "&#39;single&#39;");
        assert_eq!(escape_html("a&b"), "a&amp;b");
    }

    #[test]
    fn escape_html_preserves_plain_text() {
        assert_eq!(escape_html("hello world"), "hello world");
        assert_eq!(escape_html("SYM-123"), "SYM-123");
    }

    #[test]
    fn format_elapsed_shows_seconds_only_under_a_minute() {
        let now = Utc::now();
        let started = now - Duration::seconds(42);

        assert_eq!(format_elapsed(started, now), "42s");
    }

    #[test]
    fn format_elapsed_shows_minutes_and_seconds() {
        let now = Utc::now();
        let started = now - Duration::seconds(125);

        assert_eq!(format_elapsed(started, now), "2m 5s");
    }

    #[test]
    fn format_elapsed_handles_zero_duration() {
        let now = Utc::now();

        assert_eq!(format_elapsed(now, now), "0s");
    }

    #[test]
    fn format_elapsed_clamps_negative_to_zero() {
        let now = Utc::now();
        let future = now + Duration::seconds(10);

        assert_eq!(format_elapsed(future, now), "0s");
    }
}
