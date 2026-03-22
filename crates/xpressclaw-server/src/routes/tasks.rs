use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::tasks::board::{CreateTask, TaskBoard, UpdateTask};
use xpressclaw_core::tasks::conversation::TaskConversation;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListParams {
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct StatusUpdate {
    pub status: String,
    pub agent_id: Option<String>,
}

#[derive(Deserialize)]
pub struct MessageInput {
    pub role: String,
    pub content: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_tasks).post(create_task))
        .route("/counts", get(task_counts))
        .route(
            "/{id}",
            get(get_task).patch(update_task).delete(delete_task),
        )
        .route("/{id}/status", patch(update_task_status))
        .route("/{id}/messages", get(get_messages).post(add_message))
}

async fn list_tasks(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let limit = params.limit.unwrap_or(100);

    let tasks = board
        .list(params.status.as_deref(), params.agent_id.as_deref(), limit)
        .map_err(internal_error)?;

    let counts = board.counts().map_err(internal_error)?;

    Ok(Json(json!({ "tasks": tasks, "counts": counts })))
}

async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<CreateTask>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let task = board.create(&req).map_err(internal_error)?;

    // Auto-enqueue for the dispatcher if the task has an assigned agent
    if let Some(ref agent_id) = task.agent_id {
        let queue = xpressclaw_core::tasks::queue::TaskQueue::new(state.db.clone());
        if let Err(e) = queue.enqueue(&task.id, agent_id) {
            tracing::warn!(
                task_id = task.id,
                agent_id,
                error = %e,
                "failed to enqueue task for dispatch"
            );
        }
    }

    Ok((StatusCode::CREATED, Json(json!(task))))
}

async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let task = board.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::TaskNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(task)))
}

async fn update_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTask>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let task = board.update(&id, &req).map_err(|e| match &e {
        xpressclaw_core::error::Error::TaskNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(task)))
}

async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    board.delete(&id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_task_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<StatusUpdate>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let task = board
        .update_status(&id, &req.status, req.agent_id.as_deref())
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::TaskNotFound { .. } => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": e.to_string() })),
            ),
            xpressclaw_core::error::Error::Task(_) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            ),
            _ => internal_error(e),
        })?;
    Ok(Json(json!(task)))
}

async fn task_counts(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let counts = board.counts().map_err(internal_error)?;
    Ok(Json(json!(counts)))
}

async fn get_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let conv = TaskConversation::new(state.db.clone());
    let messages = conv.get_messages(&id).map_err(internal_error)?;
    Ok(Json(json!(messages)))
}

async fn add_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let conv = TaskConversation::new(state.db.clone());
    let msg = conv
        .add_message(&id, &req.role, &req.content)
        .map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(msg))))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        Router::new().nest("/tasks", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_create_and_list_tasks() {
        let app = test_app();

        // Create a task
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "title": "Test task",
                            "description": "A test",
                            "agent_id": "atlas"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["title"], "Test task");
        assert_eq!(body["status"], "pending");
        let task_id = body["id"].as_str().unwrap().to_string();

        // List tasks
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(body["counts"]["pending"], 1);

        // Get single task
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/tasks/{task_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["id"], task_id);
    }

    #[tokio::test]
    async fn test_update_task_status() {
        let app = test_app();

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Status test"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        // Update status
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/tasks/{task_id}/status"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"status": "in_progress", "agent_id": "atlas"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"], "in_progress");
        assert_eq!(body["agent_id"], "atlas");
    }

    #[tokio::test]
    async fn test_invalid_status_returns_400() {
        let app = test_app();

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Bad status"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        // Invalid status
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/tasks/{task_id}/status"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"status": "invalid_status"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_not_found_returns_404() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/tasks/nonexistent-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_task() {
        let app = test_app();

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "To delete"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/tasks/{task_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify gone
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/tasks/{task_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_task_messages() {
        let app = test_app();

        // Create task first
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Message test"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        // Add message
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tasks/{task_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"role": "user", "content": "Hello agent"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["role"], "user");
        assert_eq!(body["content"], "Hello agent");

        // Get messages
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/tasks/{task_id}/messages"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
    }
}
