use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use xpressclaw_core::dashboard::{
    DashboardFilter, DashboardManager, DashboardRange, DashboardReplay,
};

use crate::state::AppState;

const STREAM_BATCH: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/snapshot", get(snapshot))
        .route("/feed", get(feed))
        .route("/stream", get(stream))
        .route("/resources", get(resources))
}

async fn resources(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let config = state.config();
    let monitor = state.resource_monitor.clone();
    let sample = tokio::task::spawn_blocking(move || {
        monitor
            .try_lock()
            .map(|mut monitor| {
                monitor.sample(&config.system.data_dir, &config.system.workspace_dir)
            })
            .map_err(|_| ())
    })
    .await
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Resource monitoring unavailable"})),
        )
    })?
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Resource monitoring unavailable"})),
        )
    })?;
    Ok(Json(json!(sample)))
}

#[derive(Debug, Deserialize)]
struct DashboardParams {
    project_id: Option<String>,
    #[serde(default = "default_range")]
    range: String,
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<i64>,
}

fn default_range() -> String {
    "24h".to_string()
}

fn filter(params: &DashboardParams) -> Result<DashboardFilter, ApiError> {
    let range = match params.range.as_str() {
        "1h" => DashboardRange::Hour,
        "24h" => DashboardRange::Day,
        "7d" => DashboardRange::Week,
        _ => return Err(bad_request("range must be one of 1h, 24h, or 7d")),
    };
    Ok(DashboardFilter {
        project_id: params.project_id.clone(),
        range,
    })
}

async fn snapshot(
    State(state): State<AppState>,
    Query(params): Query<DashboardParams>,
) -> Result<Json<Value>, ApiError> {
    let filter = filter(&params)?;
    let snapshot = DashboardManager::new(state.db)
        .snapshot(&filter, params.limit.unwrap_or(40).clamp(1, 100))
        .map_err(api_error)?;
    Ok(Json(json!(snapshot)))
}

async fn feed(
    State(state): State<AppState>,
    Query(params): Query<DashboardParams>,
) -> Result<Json<Value>, ApiError> {
    let filter = filter(&params)?;
    let page = DashboardManager::new(state.db)
        .feed(
            &filter,
            params.before,
            params.limit.unwrap_or(40).clamp(1, 100),
        )
        .map_err(api_error)?;
    Ok(Json(json!(page)))
}

async fn stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<DashboardParams>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // Validate both the range and Project before committing an SSE response.
    let requested_filter = filter(&params)?;
    DashboardManager::new(state.db.clone())
        .replay_after(&requested_filter, 0, 1)
        .map_err(api_error)?;
    let header_cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok());
    let mut cursor = header_cursor.or(params.after).unwrap_or_default().max(0);
    let replay_filter = requested_filter;

    let event_stream = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_millis(800));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match DashboardManager::new(state.db.clone())
                .replay_after(&replay_filter, cursor, STREAM_BATCH)
            {
                Ok(DashboardReplay { reset_required: true, latest_cursor, .. }) => {
                    cursor = latest_cursor;
                    let event = Event::default()
                        .id(cursor.to_string())
                        .event("reset")
                        .json_data(json!({
                            "cursor": cursor,
                            "reason": "replay_window_expired"
                        }))
                        .expect("dashboard reset event is serializable");
                    yield Ok(event);
                }
                Ok(replay) if !replay.events.is_empty() => {
                    for dashboard_event in replay.events {
                        cursor = dashboard_event.cursor;
                        let event = Event::default()
                            .id(cursor.to_string())
                            .event("dashboard")
                            .json_data(dashboard_event)
                            .expect("dashboard event is serializable");
                        yield Ok(event);
                    }
                }
                Ok(replay) if replay.latest_cursor > cursor => {
                    // Other-Project events still advance this filtered stream's
                    // durable cursor, preventing repeated scans on a quiet scope.
                    cursor = replay.latest_cursor;
                    yield Ok(Event::default()
                        .id(cursor.to_string())
                        .event("cursor")
                        .data("{}"));
                }
                Ok(_) => {}
                Err(_) => {
                    yield Ok(Event::default()
                        .event("stream_error")
                        .data(r#"{"message":"Dashboard stream is temporarily unavailable"}"#));
                }
            }
        }
    };
    Ok(Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("dashboard-heartbeat"),
    ))
}

type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn api_error(error: xpressclaw_core::error::Error) -> ApiError {
    let status = match error {
        xpressclaw_core::error::Error::ProjectNotFound { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::agents::registry::AgentRegistry;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::dashboard::DashboardManager;
    use xpressclaw_core::db::Database;
    use xpressclaw_core::projects::{CreateProject, ProjectManager};
    use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
    use xpressclaw_core::tasks::conversation::TaskConversation;

    use super::*;

    fn app_with_db() -> (Router, Arc<Database>, String) {
        let db = Arc::new(Database::open_memory().unwrap());
        let project = ProjectManager::new(db.clone())
            .create(&CreateProject {
                name: "Platform".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        let state = AppState::new(
            Arc::new(Config::load_default().unwrap()),
            db.clone(),
            None,
            "test.yaml".into(),
            true,
        );
        (routes().with_state(state), db, project.id)
    }

    fn app() -> Router {
        app_with_db().0
    }

    async fn json_body(response: axum::response::Response) -> Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn snapshot_is_bounded_and_reports_context_semantics() {
        let response = app()
            .oneshot(
                Request::get("/snapshot?range=1h&limit=7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["projects"][0]["name"], "Platform");
        assert!(body["series"].as_array().unwrap().len() <= 14);
        assert!(body["counters"]["working_agents"].is_number());
        assert!(body["cursor"].is_number());
        assert!(body["token_usage"]["total_tokens"].is_null());
        assert_eq!(body["token_usage"]["reported_responses"], 0);
        assert!(body["token_usage"]["recording_started_at"].is_string());
    }

    #[tokio::test]
    async fn resources_are_independent_of_filters_and_share_one_sample() {
        let app = app();
        let mut samples = Vec::new();
        for path in ["/resources", "/resources?project_id=missing&range=7d"] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            samples.push(json_body(response).await);
        }
        assert_eq!(samples[0]["sampled_at"], samples[1]["sampled_at"]);
        assert!(samples[0]["cpu_percent"].is_null());
        assert!(samples[0]["cpu_count"].is_number());
        assert!(!samples[0]["disks"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn feed_pagination_counts_agent_text_instead_of_noisy_control_events() {
        let (app, db, project_id) = app_with_db();
        AgentRegistry::new(db.clone())
            .create_in_project("platform-agent", "native", &project_id)
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Readable feed".into(),
                agent_id: Some("platform-agent".into()),
                ..Default::default()
            })
            .unwrap();
        let messages = TaskConversation::new(db.clone());
        messages
            .add_message(&task.id, "assistant", "Earlier response")
            .unwrap();
        for _ in 0..50 {
            messages
                .add_message(&task.id, "user", "User context")
                .unwrap();
            DashboardManager::new(db.clone())
                .record_task_tool_call("a", &task.id, "Read workspace")
                .unwrap();
        }
        messages
            .add_message(&task.id, "assistant", "Latest response")
            .unwrap();
        let response = app
            .clone()
            .oneshot(Request::get("/feed?limit=1").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let first = json_body(response).await;
        assert_eq!(first["events"][0]["preview"], "Latest response");
        assert_eq!(first["has_more"], true);
        let response = app
            .oneshot(
                Request::get(format!("/feed?limit=1&before={}", first["next_before"]))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let second = json_body(response).await;
        assert_eq!(second["events"][0]["preview"], "Earlier response");
        assert_eq!(second["has_more"], false);
    }

    #[tokio::test]
    async fn snapshots_keep_deletion_markers_to_evict_loaded_older_messages() {
        let (app, db, project_id) = app_with_db();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO conversations (id, project_id) VALUES ('c', ?1)", [&project_id]).unwrap();
            conn.execute("INSERT INTO conversation_messages (conversation_id, sender_type, sender_id, content) VALUES ('c', 'agent', 'agent', 'Old response')", []).unwrap();
            conn.execute("UPDATE conversation_messages SET deleted_at = CURRENT_TIMESTAMP WHERE conversation_id = 'c'", []).unwrap();
        });
        let response = app
            .oneshot(Request::get("/snapshot").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = json_body(response).await;
        let events = body["feed"]["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_kind"], "conversation_message_deleted");
        assert_eq!(events[0]["preview"], "");
    }

    #[tokio::test]
    async fn filters_are_validated_on_the_server() {
        let invalid_range = app()
            .clone()
            .oneshot(
                Request::get("/snapshot?range=forever")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_range.status(), StatusCode::BAD_REQUEST);

        let missing_project = app()
            .oneshot(
                Request::get("/feed?project_id=missing&range=24h")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_project.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stream_replays_after_last_event_id_without_overlap() {
        let (app, db, project_id) = app_with_db();
        AgentRegistry::new(db.clone())
            .create_in_project("platform-agent", "native", &project_id)
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Stream the dashboard".into(),
                agent_id: Some("platform-agent".into()),
                ..Default::default()
            })
            .unwrap();
        let conversation = TaskConversation::new(db.clone());
        conversation
            .add_message(&task.id, "user", "Already delivered")
            .unwrap();
        let cursor = DashboardManager::new(db.clone()).latest_cursor().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dashboard_events (
                    event_id, event_kind, occurred_at, project_id, project_name,
                    source_label, target_type, target_id, target_title, href, preview
                 ) VALUES ('old-after-cursor', 'progress',
                           '2000-01-01T00:00:00.000Z', ?1, 'Platform', 'Agent',
                           'task', ?2, 'Stream the dashboard', '/tasks/' || ?2,
                           'Out-of-window replay')",
                [&project_id, &task.id],
            )
            .unwrap();
        });
        conversation
            .add_message(&task.id, "assistant", "New response")
            .unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/stream?project_id={project_id}&range=1h&after=0"))
                    .header("last-event-id", cursor.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/event-stream"
        );
        let mut body = response.into_body();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
            .await
            .expect("dashboard stream should produce a replay promptly")
            .expect("dashboard stream should remain open")
            .expect("dashboard stream frame should be readable")
            .into_data()
            .expect("dashboard stream should emit data");
        let event = String::from_utf8(frame.to_vec()).unwrap();
        assert!(event.contains("event: dashboard"));
        assert!(event.contains("New response"));
        assert!(!event.contains("Already delivered"));
        assert!(!event.contains("Out-of-window replay"));
    }
}
