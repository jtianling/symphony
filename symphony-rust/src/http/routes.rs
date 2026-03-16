use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tracing::warn;

use crate::orchestrator::{OrchestratorMsg, StateSnapshot};

use super::{render_dashboard, StateProvider};

#[derive(Clone)]
pub struct AppState {
    pub state_provider: Arc<StateProvider>,
    pub msg_tx: mpsc::Sender<OrchestratorMsg>,
}

pub fn create_router(
    state_provider: Arc<StateProvider>,
    msg_tx: mpsc::Sender<OrchestratorMsg>,
) -> Router {
    let state = AppState {
        state_provider,
        msg_tx,
    };

    Router::new()
        .route("/", get(dashboard_handler))
        .route("/api/v1/events", get(get_events))
        .route("/api/v1/state", get(get_state))
        .route("/api/v1/refresh", post(post_refresh))
        .route("/api/v1/{identifier}", get(get_issue_detail))
        .fallback(not_found_handler)
        .with_state(state)
}

async fn dashboard_handler(State(state): State<AppState>) -> Html<String> {
    let snapshot = state.state_provider.snapshot();
    Html(render_dashboard(&snapshot))
}

async fn get_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.state_provider.subscribe_events();
    let initial_snapshot = state.state_provider.snapshot();
    let initial = tokio_stream::iter(snapshot_event(initial_snapshot));
    let updates = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(snapshot) => snapshot_event(snapshot),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            warn!(skipped, "sse client lagged behind state broadcast");
            None
        }
    });

    Sse::new(initial.chain(updates))
}

async fn get_state(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.state_provider.snapshot();

    Json(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "counts": {
            "running": snapshot.running.len(),
            "retrying": snapshot.retrying.len(),
        },
        "running": snapshot.running,
        "retrying": snapshot.retrying,
        "codex_totals": {
            "input_tokens": snapshot.codex_totals.input_tokens,
            "output_tokens": snapshot.codex_totals.output_tokens,
            "total_tokens": snapshot.codex_totals.total_tokens,
            "seconds_running": snapshot.codex_totals.seconds_running,
        },
        "rate_limits": snapshot.rate_limits,
    }))
}

async fn get_issue_detail(
    State(state): State<AppState>,
    Path(identifier): Path<String>,
) -> Response {
    let snapshot = state.state_provider.snapshot();

    if let Some(running) = snapshot
        .running
        .iter()
        .find(|entry| entry.issue_identifier == identifier || entry.issue_id == identifier)
    {
        return (
            StatusCode::OK,
            Json(json!({
                "status": "running",
                "issue_id": &running.issue_id,
                "issue_identifier": &running.issue_identifier,
                "details": running,
            })),
        )
            .into_response();
    }

    if let Some(retrying) = snapshot
        .retrying
        .iter()
        .find(|entry| entry.issue_identifier == identifier || entry.issue_id == identifier)
    {
        return (
            StatusCode::OK,
            Json(json!({
                "status": "retrying",
                "issue_id": &retrying.issue_id,
                "issue_identifier": &retrying.issue_identifier,
                "details": retrying,
            })),
        )
            .into_response();
    }

    error_response(StatusCode::NOT_FOUND, "issue_not_found", "Issue not found")
}

async fn post_refresh(State(state): State<AppState>) -> impl IntoResponse {
    if state
        .msg_tx
        .send(OrchestratorMsg::RefreshRequest)
        .await
        .is_err()
    {
        warn!("failed to queue refresh request");
    }

    (StatusCode::ACCEPTED, Json(json!({ "queued": true })))
}

async fn not_found_handler() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "Not found")
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response()
}

fn snapshot_event(snapshot: StateSnapshot) -> Option<Result<Event, std::convert::Infallible>> {
    match Event::default().event("state").json_data(snapshot) {
        Ok(event) => Some(Ok(event)),
        Err(error) => {
            warn!(%error, "failed to serialize state snapshot for sse");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use axum::body::{to_bytes, Body, Bytes};
    use axum::http::{Method, Request, StatusCode};
    use axum::Router;
    use chrono::{Duration, Utc};
    use serde_json::json;
    use serde_json::Value;
    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use tokio_stream::StreamExt;
    use tower::util::ServiceExt;

    use super::create_router;
    use crate::domain::{RetryEntry, TokenUsage};
    use crate::http::StateProvider;
    use crate::orchestrator::{
        AggregateTokens, OrchestratorMsg, RunningSnapshot, SnapshotCounts, StateSnapshot,
    };

    fn sample_snapshot() -> StateSnapshot {
        StateSnapshot {
            generated_at: Utc::now(),
            counts: SnapshotCounts {
                running: 1,
                claimed: 1,
                completed: 0,
                retrying: 1,
            },
            running: vec![RunningSnapshot {
                issue_id: String::from("issue-1"),
                issue_identifier: String::from("SYM-1"),
                state: String::from("Todo"),
                worker_host: Some(String::from("host1")),
                session_id: String::from("session-1"),
                turn_count: 2,
                workspace_path: String::from("/tmp/issue-1"),
                started_at: Utc::now() - Duration::seconds(5),
                last_event_at: Some(Utc::now()),
                tokens: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                },
            }],
            retrying: vec![RetryEntry {
                issue_id: String::from("issue-2"),
                issue_identifier: String::from("SYM-2"),
                attempt: 3,
                scheduled_at: Utc::now() + Duration::seconds(30),
                reason: Some(String::from("retry")),
                worker_host: None,
            }],
            codex_totals: AggregateTokens {
                input_tokens: 100,
                output_tokens: 200,
                total_tokens: 300,
                seconds_running: 12.5,
            },
            rate_limits: Some(json!({ "remaining": 42 })),
        }
    }

    fn test_router() -> (Router, Arc<StateProvider>, mpsc::Receiver<OrchestratorMsg>) {
        let provider = Arc::new(StateProvider::new());
        provider.update(sample_snapshot());
        let (msg_tx, msg_rx) = mpsc::channel(4);

        (create_router(provider.clone(), msg_tx), provider, msg_rx)
    }

    #[tokio::test]
    // SPEC 17.4 / 17.6: snapshot API returns running rows, retry rows, token totals, and limits.
    async fn state_response_contains_expected_structure() -> Result<(), Box<dyn std::error::Error>>
    {
        let (app, _, _) = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/state")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let payload: Value = serde_json::from_slice(&body)?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["counts"]["running"], 1);
        assert_eq!(payload["counts"]["retrying"], 1);
        assert_eq!(payload["running"][0]["issue_identifier"], "SYM-1");
        assert_eq!(payload["retrying"][0]["issue_identifier"], "SYM-2");

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.6: refresh requests are operator-visible and queued without crashing the host.
    async fn refresh_response_returns_accepted() -> Result<(), Box<dyn std::error::Error>> {
        let (app, _, mut msg_rx) = test_router();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/refresh")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let payload: Value = serde_json::from_slice(&body)?;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(payload["queued"], true);
        assert!(matches!(
            msg_rx.recv().await,
            Some(OrchestratorMsg::RefreshRequest)
        ));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.4: issue-scoped snapshot lookups surface unavailable issues cleanly.
    async fn issue_not_found_returns_404() -> Result<(), Box<dyn std::error::Error>> {
        let (app, _, _) = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/SYM-404")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let payload: Value = serde_json::from_slice(&body)?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(payload["error"]["code"], "issue_not_found");

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.6: the human-readable dashboard is derived from orchestrator state.
    async fn dashboard_returns_html() -> Result<(), Box<dyn std::error::Error>> {
        let (app, _, _) = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let html = String::from_utf8(body.to_vec())?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html; charset=utf-8"));
        assert!(html.contains("Symphony"));
        assert!(html.contains("Running Sessions"));

        Ok(())
    }

    #[tokio::test]
    // SPEC sse-push: SSE endpoint exposes text/event-stream and emits an initial snapshot.
    async fn sse_endpoint_returns_initial_event() -> Result<(), Box<dyn std::error::Error>> {
        let (app, _, _) = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/events")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut stream = response.into_body().into_data_stream();
        let event = read_sse_event(&mut stream).await?;
        let payload = event_payload(&event)?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/event-stream"));
        assert!(event.contains("event: state"));
        assert_eq!(payload["counts"]["running"], 1);
        assert_eq!(payload["running"][0]["issue_identifier"], "SYM-1");

        Ok(())
    }

    #[tokio::test]
    // SPEC sse-push: connected SSE clients receive state snapshots after provider updates.
    async fn sse_endpoint_receives_published_updates() -> Result<(), Box<dyn std::error::Error>> {
        let (app, provider, _) = test_router();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/events")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;
        let mut stream = response.into_body().into_data_stream();
        let _ = read_sse_event(&mut stream).await?;

        let mut updated = sample_snapshot();
        updated.generated_at = Utc::now();
        updated.counts.running = 2;
        updated.counts.retrying = 0;
        updated.retrying.clear();
        updated.codex_totals.total_tokens = 999;
        updated.running.push(RunningSnapshot {
            issue_id: String::from("issue-3"),
            issue_identifier: String::from("SYM-3"),
            state: String::from("In Progress"),
            worker_host: None,
            session_id: String::from("session-3"),
            turn_count: 4,
            workspace_path: String::from("/tmp/issue-3"),
            started_at: Utc::now() - Duration::seconds(9),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage {
                input_tokens: 11,
                output_tokens: 22,
                total_tokens: 33,
            },
        });
        provider.update(updated);

        let event = read_sse_event(&mut stream).await?;
        let payload = event_payload(&event)?;

        assert!(event.contains("event: state"));
        assert_eq!(payload["counts"]["running"], 2);
        assert_eq!(payload["running"][1]["issue_identifier"], "SYM-3");
        assert_eq!(payload["codex_totals"]["total_tokens"], 999);

        Ok(())
    }

    async fn read_sse_event<S>(stream: &mut S) -> Result<String, Box<dyn std::error::Error>>
    where
        S: tokio_stream::Stream<Item = Result<Bytes, axum::Error>> + Unpin,
    {
        let mut buffer = String::new();

        loop {
            let next = timeout(StdDuration::from_secs(1), stream.next())
                .await
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::TimedOut, error))?;
            let chunk = next.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "sse stream ended")
            })??;
            buffer.push_str(std::str::from_utf8(&chunk)?);

            if let Some(index) = buffer.find("\n\n") {
                return Ok(buffer[..index].to_owned());
            }
        }
    }

    fn event_payload(event: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let data = event
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing data field")
            })?;
        Ok(serde_json::from_str(data)?)
    }
}
