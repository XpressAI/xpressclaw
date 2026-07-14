use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::sessions::{NewEvent, SessionManager};
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::tasks::queue::TaskQueue;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct EventsQuery {
    after: Option<i64>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AttemptsQuery {
    status: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MessageInput {
    content: String,
    priority: Option<i32>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{id}", get(get_session))
        .route("/{id}/events", get(get_events))
        .route("/{id}/attempts", get(get_attempts))
        .route("/{id}/messages", post(post_message))
        .route("/{id}/attempts/{attempt_id}/cancel", post(cancel_attempt))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let overview = SessionManager::new(state.db.clone())
        .overview(&id)
        .map_err(internal_error)?;
    Ok(Json(json!(overview)))
}

async fn get_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let events = SessionManager::new(state.db.clone())
        .list_events(&id, query.after, query.limit.unwrap_or(100).clamp(1, 500))
        .map_err(internal_error)?;
    Ok(Json(json!(events)))
}

async fn get_attempts(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<AttemptsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let attempts = SessionManager::new(state.db.clone())
        .list_attempts(
            &id,
            query.status.as_deref(),
            query.limit.unwrap_or(50).clamp(1, 200),
        )
        .map_err(internal_error)?;
    Ok(Json(json!(attempts)))
}

async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<MessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if input.content.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "message cannot be empty" })),
        ));
    }
    ensure_session(&state, &id)?;
    let sessions = SessionManager::new(state.db.clone());
    let event = sessions
        .append_event(
            &id,
            NewEvent {
                attempt_id: None,
                task_id: None,
                source_type: "user",
                source_id: Some("local-user"),
                event_type: "message_received",
                summary: input.content.trim(),
                payload: json!({ "content": input.content.trim() }),
            },
        )
        .map_err(internal_error)?;

    let title = concise_title(input.content.trim());
    let board = TaskBoard::new(state.db.clone());
    let task = board
        .create(&CreateTask {
            title,
            description: Some(input.content.trim().to_string()),
            agent_id: Some(id.clone()),
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            priority: input.priority,
            context: Some(json!({
                "origin": "session_message",
                "kind": "interactive",
                "source_event_id": event.id,
            })),
        })
        .map_err(internal_error)?;
    let queue_item = TaskQueue::new(state.db.clone())
        .enqueue(&task.id, &id)
        .map_err(internal_error)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "event": event,
            "task": task,
            "attempt_id": queue_item.attempt_id,
            "queued": true,
        })),
    ))
}

async fn cancel_attempt(
    State(state): State<AppState>,
    Path((id, attempt_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let sessions = SessionManager::new(state.db.clone());
    let attempt = sessions.get_attempt(&attempt_id).map_err(not_found)?;
    if attempt.session_id != id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "attempt does not belong to this session" })),
        ));
    }
    if matches!(
        attempt.status.as_str(),
        "completed" | "failed" | "cancelled"
    ) {
        return Ok(Json(json!(attempt)));
    }

    let cancelled = sessions
        .transition_attempt(
            &attempt_id,
            "cancelled",
            "Work cancelled by user",
            None,
            None,
        )
        .map_err(internal_error)?;
    state
        .db
        .with_conn(|conn| {
            if let Some(queue_id) = attempt.queue_id {
                conn.execute(
                    "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                        harness_response = 'cancelled by user' WHERE id = ?1",
                    [queue_id],
                )?;
            }
            if let Some(task_id) = attempt.task_id.as_deref() {
                conn.execute(
                    "UPDATE tasks SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [task_id],
                )?;
            }
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .map_err(internal_error)?;

    if let Some(docker) = state.docker().await {
        let _ = docker.stop(&format!("attempt-{attempt_id}")).await;
    }
    Ok(Json(json!(cancelled)))
}

fn ensure_session(state: &AppState, id: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let record = AgentRegistry::new(state.db.clone())
        .get(id)
        .map_err(not_found)?;
    let title = state
        .config()
        .agents
        .iter()
        .find(|agent| agent.name == id)
        .and_then(|agent| agent.display_name.as_deref())
        .unwrap_or(&record.name)
        .to_string();
    SessionManager::new(state.db.clone())
        .ensure(id, Some(&title))
        .map_err(internal_error)?;
    Ok(())
}

fn concise_title(content: &str) -> String {
    let mut title: String = content.chars().take(72).collect();
    if content.chars().count() > 72 {
        title.push('…');
    }
    title
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

fn not_found(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": error.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::config::{AgentConfig, Config};
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("builder", "codex")
            .unwrap();
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "builder".to_string(),
            backend: "codex".to_string(),
            display_name: Some("Builder".to_string()),
            ..Default::default()
        });
        let state = AppState::new(
            Arc::new(config),
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        Router::new().nest("/sessions", routes()).with_state(state)
    }

    #[tokio::test]
    async fn message_is_accepted_while_work_runs_in_background() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/builder/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "content": "Implement the feature" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["queued"], true);
        assert!(value["attempt_id"].is_string());
    }
}
