use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::warn;

use crate::orchestrator::OrchestratorMsg;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request, StatusCode};
    use axum::Router;
    use chrono::{Duration, Utc};
    use serde_json::json;
    use serde_json::Value;
    use tokio::sync::mpsc;
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

    fn test_router() -> (Router, mpsc::Receiver<OrchestratorMsg>) {
        let provider = Arc::new(StateProvider::new());
        provider.update(sample_snapshot());
        let (msg_tx, msg_rx) = mpsc::channel(4);

        (create_router(provider, msg_tx), msg_rx)
    }

    #[tokio::test]
    // SPEC 17.4 / 17.6: snapshot API returns running rows, retry rows, token totals, and limits.
    async fn state_response_contains_expected_structure() -> Result<(), Box<dyn std::error::Error>>
    {
        let (app, _) = test_router();
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
        let (app, mut msg_rx) = test_router();
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
        let (app, _) = test_router();
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
        let (app, _) = test_router();
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
}
