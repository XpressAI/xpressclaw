use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use xpressclaw_core::activity::ActivityManager;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ActivityParams {
    pub limit: Option<i64>,
    pub agent_id: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_activity))
        .route("/stream", get(activity_stream))
}

async fn list_activity(
    State(state): State<AppState>,
    Query(params): Query<ActivityParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ActivityManager::new(state.db.clone());
    let limit = params.limit.unwrap_or(50);

    let events = if let Some(ref agent_id) = params.agent_id {
        mgr.get_by_agent(agent_id, limit)
    } else {
        mgr.get_recent(limit)
    }
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!(events)))
}

/// SSE endpoint that streams new activity events.
///
/// Polls the database every 2 seconds for events newer than the last seen.
/// Clients connect via `GET /api/activity/stream`.
async fn activity_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let db = state.db.clone();

    let stream = async_stream::stream! {
        let mgr = ActivityManager::new(db);
        let mut last_id: i64 = mgr
            .get_recent(1)
            .ok()
            .and_then(|v| v.first().map(|e| e.id))
            .unwrap_or(0);

        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let events = match mgr.get_recent(50) {
                Ok(events) => events,
                Err(_) => continue,
            };

            for event in events.into_iter().rev() {
                if event.id > last_id {
                    last_id = event.id;
                    let data = json!({
                        "id": event.id,
                        "event_type": event.event_type,
                        "agent_id": event.agent_id,
                        "event_data": event.data,
                        "timestamp": event.timestamp,
                    });
                    if let Ok(sse) = Event::default()
                        .event("activity")
                        .json_data(&data)
                    {
                        yield Ok(sse);
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::activity::ActivityManager;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_app() -> (Arc<Database>, Router) {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState {
            config,
            db: db.clone(),
            llm_router: None,
        };
        let router = Router::new().nest("/activity", routes()).with_state(state);
        (db, router)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_list_empty_activity() {
        let (_db, app) = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/activity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_list_activity_with_events() {
        let (db, app) = test_app();

        // Insert events via core
        let mgr = ActivityManager::new(db);
        let data = serde_json::json!({"task": "t1"});
        mgr.log("task.created", Some("atlas"), Some(&data), None)
            .unwrap();
        mgr.log("task.completed", Some("atlas"), None, None)
            .unwrap();
        mgr.log("task.created", Some("hermes"), None, None).unwrap();

        // List all
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/activity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 3);

        // Filter by agent
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/activity?agent_id=hermes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 1);

        // With limit
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/activity?limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
    }
}
