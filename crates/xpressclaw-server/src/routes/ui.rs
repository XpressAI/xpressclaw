use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use xpressclaw_core::db::{Database, UiRevisions};

use crate::state::AppState;

pub const UI_REVISIONS_HEADER: &str = "x-ui-revisions";

const COALESCE_WINDOW: Duration = Duration::from_millis(100);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(25);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/revisions", get(revisions))
        .route("/stream", get(stream))
}

/// Stamp 2xx GET/HEAD API responses with the UI revision snapshot captured
/// *before* the handler reads SQLite or runtime state. This lets the browser
/// establish an exact revision baseline on initial loads with zero extra
/// requests when idle while still catching mutations that occur before the SSE
/// stream's first event arrives.
pub async fn attach_ui_revisions_header(
    State(db): State<Arc<Database>>,
    request: Request,
    next: Next,
) -> Response {
    let is_read = matches!(*request.method(), Method::GET | Method::HEAD);
    let pre_read = is_read.then(|| db.ui_revisions());
    let mut response = next.run(request).await;
    if let Some(snapshot) = pre_read {
        if response.status().is_success() && !response.headers().contains_key(UI_REVISIONS_HEADER) {
            if let Ok(value) = HeaderValue::from_str(&snapshot.to_header_value()) {
                response.headers_mut().insert(UI_REVISIONS_HEADER, value);
            }
        }
    }
    response
}

async fn revisions(State(state): State<AppState>) -> Json<UiRevisions> {
    Json(state.db.ui_revisions())
}

async fn stream(State(state): State<AppState>) -> Response {
    let mut rx = state.db.subscribe_ui_revisions();
    let initial = *rx.borrow_and_update();

    let event_stream = async_stream::stream! {
        yield Result::<Event, Infallible>::Ok(revision_event(initial));
        while rx.changed().await.is_ok() {
            tokio::time::sleep(COALESCE_WINDOW).await;
            let current = *rx.borrow_and_update();
            yield Result::<Event, Infallible>::Ok(revision_event(current));
        }
    };

    let mut response = Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL).text("ping"))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Ok(value) = HeaderValue::from_str(&initial.to_header_value()) {
        headers.insert(UI_REVISIONS_HEADER, value);
    }
    response
}

fn revision_event(revisions: UiRevisions) -> Event {
    Event::default()
        .id(revisions.seq.to_string())
        .event("revisions")
        .json_data(revisions)
        .expect("UiRevisions is serializable")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::{ui_domains, Database};

    use super::*;

    fn test_state() -> AppState {
        let db = Arc::new(Database::open_memory().unwrap());
        AppState::new(
            Arc::new(Config::default()),
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        )
    }

    #[tokio::test]
    async fn revisions_endpoint_reflects_database_and_runtime_updates() {
        let state = test_state();
        let app = Router::new()
            .nest("/ui", routes())
            .with_state(state.clone());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ui/revisions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let initial: UiRevisions =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(initial.seq, 0);

        state
            .db
            .bump_ui_domains(ui_domains::TASKS | ui_domains::SCHEDULES);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/ui/revisions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let updated: UiRevisions =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(updated.seq, 1);
        assert_eq!(updated.tasks, 1);
        assert_eq!(updated.schedules, 1);
        assert_eq!(updated.projects, 0);
    }

    #[tokio::test]
    async fn attach_ui_revisions_header_stamps_pre_read_snapshot_on_gets_only() {
        let state = test_state();
        state.db.bump_ui_domains(ui_domains::TASKS);

        let app = Router::new()
            .route(
                "/probe",
                get(|State(state): State<AppState>| async move {
                    // Simulate a concurrent write completing after the GET started.
                    state.db.bump_ui_domains(ui_domains::TASKS);
                    "ok"
                })
                .post(|| async { "mutated" }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state.db.clone(),
                attach_ui_revisions_header,
            ))
            .with_state(state.clone());

        let get_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let raw_header = get_resp.headers()[UI_REVISIONS_HEADER].to_str().unwrap();
        let stamped = UiRevisions::from_header_value(raw_header).unwrap();
        // Header reflects the conservative pre-read snapshot (tasks == 1), not the
        // post-write revision (tasks == 2), so SSE delivering tasks == 2 reconciles.
        assert_eq!(stamped.tasks, 1);
        assert_eq!(state.db.ui_revisions().tasks, 2);

        let post_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_resp.status(), StatusCode::OK);
        assert!(!post_resp.headers().contains_key(UI_REVISIONS_HEADER));
    }
}
