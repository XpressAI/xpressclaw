use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use xpressclaw_core::db::UiRevisions;

use crate::state::AppState;

const COALESCE_WINDOW: Duration = Duration::from_millis(100);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(25);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/revisions", get(revisions))
        .route("/stream", get(stream))
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
}
