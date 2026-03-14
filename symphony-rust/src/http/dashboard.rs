use chrono::{DateTime, Utc};

use crate::domain::RetryEntry;
use crate::orchestrator::{RunningSnapshot, StateSnapshot};

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
            "<p class=\"muted\">Generated at {generated_at}</p>",
            "<section class=\"grid\">",
            "<article class=\"card\"><h2>Running</h2><p>{running_count}</p></article>",
            "<article class=\"card\"><h2>Retrying</h2><p>{retrying_count}</p></article>",
            "<article class=\"card\"><h2>Input Tokens</h2><p>{input_tokens}</p></article>",
            "<article class=\"card\"><h2>Output Tokens</h2><p>{output_tokens}</p></article>",
            "<article class=\"card\"><h2>Total Tokens</h2><p>{total_tokens}</p></article>",
            "<article class=\"card\"><h2>Seconds Running</h2><p>{seconds_running:.1}</p></article>",
            "</section>",
            "<section class=\"section\">",
            "<h2>Running Sessions</h2>",
            "<table><thead><tr>",
            "<th>Issue</th><th>State</th><th>Turns</th><th>Elapsed</th><th>Workspace</th>",
            "</tr></thead><tbody>{running_rows}</tbody></table>",
            "</section>",
            "<section class=\"section\">",
            "<h2>Retry Queue</h2>",
            "<table><thead><tr>",
            "<th>Issue</th><th>Attempt</th><th>Scheduled At</th><th>Reason</th>",
            "</tr></thead><tbody>{retry_rows}</tbody></table>",
            "</section>",
            "</main>",
            "</body>",
            "</html>"
        ),
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
