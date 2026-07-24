use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::sessions::SessionManager;
use xpressclaw_core::tasks::attachments::{decode_image_attachments, ImageAttachmentInput};
use xpressclaw_core::tasks::board::{CreateTask, Task, TaskBoard, TaskStatus, UpdateTask};
use xpressclaw_core::tasks::conversation::TaskConversation;
use xpressclaw_core::tasks::queue::TaskQueue;
use xpressclaw_core::workers::acp::{
    AcpElicitationResponseError, AcpInterruptMode, CreateElicitationResponse,
};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListParams {
    pub status: Option<String>,
    pub statuses: Option<String>,
    pub agent_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    #[serde(default)]
    pub sort: TaskListSort,
    pub exclude_statuses: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskListSort {
    #[default]
    Scheduler,
    Recent,
}

#[derive(Deserialize)]
pub struct RecentByAgentParams {
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
    #[serde(default)]
    pub attachments: Vec<ImageAttachmentInput>,
    #[serde(default)]
    pub config_options: std::collections::HashMap<String, Value>,
    #[serde(default)]
    pub delivery: MessageDelivery,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDelivery {
    #[default]
    AfterTool,
    Immediate,
}

#[derive(Deserialize)]
pub struct ActivityParams {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct ElicitationResponseInput {
    pub action: String,
    pub content: Option<Value>,
    pub message: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_tasks).post(create_task))
        .route("/batch", axum::routing::post(create_tasks_batch))
        .route("/counts", get(task_counts))
        .route("/recent-by-agent", get(list_recent_tasks_by_agent))
        .route(
            "/{id}",
            get(get_task).patch(update_task).delete(delete_task),
        )
        .route("/{id}/status", patch(update_task_status))
        .route("/{id}/messages", get(get_messages).post(add_message))
        .route(
            "/{id}/messages/{message_id}/attachments/{attachment_id}",
            get(get_message_attachment),
        )
        .route("/{id}/activity", get(get_activity))
        .route(
            "/{id}/elicitations/{elicitation_id}/response",
            post(respond_to_elicitation),
        )
        .route("/{id}/dependencies", axum::routing::post(add_dependency))
}

async fn list_tasks(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let limit = params.limit.unwrap_or(100).clamp(1, 100);
    let offset = params.offset.unwrap_or(0).max(0);

    let tasks = if let Some(ref parent_id) = params.parent_task_id {
        board.list_subtasks(parent_id).map_err(internal_error)?
    } else {
        let included_statuses = params
            .status
            .as_deref()
            .map(|status| vec![status])
            .unwrap_or_else(|| parse_comma_separated(params.statuses.as_deref()));
        match params.sort {
            TaskListSort::Scheduler => board
                .list_page(
                    &included_statuses,
                    params.agent_id.as_deref(),
                    limit,
                    offset,
                )
                .map_err(internal_error)?,
            TaskListSort::Recent => {
                let excluded_statuses = parse_comma_separated(params.exclude_statuses.as_deref());
                board
                    .list_recent_page(
                        &included_statuses,
                        params.agent_id.as_deref(),
                        &excluded_statuses,
                        limit,
                        offset,
                    )
                    .map_err(internal_error)?
            }
        }
    };

    let counts = board.counts().map_err(internal_error)?;

    Ok(Json(json!({
        "tasks": enrich_tasks(&board, &tasks),
        "counts": counts,
    })))
}

fn parse_comma_separated(value: Option<&str>) -> Vec<&str> {
    value
        .map(|items| {
            items
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

async fn list_recent_tasks_by_agent(
    State(state): State<AppState>,
    Query(params): Query<RecentByAgentParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let limit = params.limit.unwrap_or(5).clamp(1, 100);
    let tasks = board.list_recent_per_agent(limit).map_err(internal_error)?;

    Ok(Json(json!({ "tasks": enrich_tasks(&board, &tasks) })))
}

fn enrich_tasks(board: &TaskBoard, tasks: &[Task]) -> Vec<Value> {
    tasks
        .iter()
        .map(|t| {
            let mut v = json!(t);
            v["depends_on"] = json!(board.get_dependencies(&t.id).unwrap_or_default());
            v["blocked_by"] = json!(board.get_blockers(&t.id).unwrap_or_default());
            v["ready"] = json!(board.is_ready(&t.id).unwrap_or(true));
            v
        })
        .collect()
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
    // Enrich with dependency info (ADR-020)
    let depends_on = board.get_dependencies(&id).unwrap_or_default();
    let dependents = board.get_dependents(&id).unwrap_or_default();
    let blocked_by = board.get_blockers(&id).unwrap_or_default();
    let ready = board.is_ready(&id).unwrap_or(true);
    let mut result = json!(task);
    result["depends_on"] = json!(depends_on);
    result["dependents"] = json!(dependents);
    result["blocked_by"] = json!(blocked_by);
    result["ready"] = json!(ready);
    Ok(Json(result))
}

async fn update_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTask>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());

    // Check if agent is being assigned — we may need to enqueue
    let new_agent = req.agent_id.clone();

    let task = board.update(&id, &req).map_err(|e| match &e {
        xpressclaw_core::error::Error::TaskNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;

    // If agent was assigned and task is actionable, enqueue for dispatcher
    if let Some(ref agent_id) = new_agent {
        if !agent_id.is_empty()
            && (task.status == xpressclaw_core::tasks::board::TaskStatus::Pending
                || task.status == xpressclaw_core::tasks::board::TaskStatus::InProgress)
        {
            let queue = TaskQueue::new(state.db.clone());
            let _ = queue.enqueue_continuation(&task.id, agent_id);
            // Also set to in_progress if it was pending
            if task.status == xpressclaw_core::tasks::board::TaskStatus::Pending {
                let _ = board.update_status(&task.id, "in_progress", Some(agent_id));
            }
        }
    }

    // Re-fetch to get updated status
    let task = board.get(&id).map_err(internal_error)?;
    Ok(Json(json!(task)))
}

async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.elicitations.cancel_task(&id);
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
    if req.status == "completed" && !board.subtasks_complete(&id).map_err(internal_error)? {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "all subtasks must be completed first" })),
        ));
    }
    if req.status == "cancelled" {
        let active_attempt_id = state
            .db
            .with_conn(|conn| {
                let attempt_id = conn.query_row(
                    "SELECT active_attempt_id FROM tasks WHERE id = ?1",
                    [&id],
                    |row| row.get::<_, Option<String>>(0),
                )?;
                Ok::<_, xpressclaw_core::error::Error>(attempt_id)
            })
            .map_err(internal_error)?;
        if let Some(attempt_id) = active_attempt_id {
            let sessions = SessionManager::new(state.db.clone());
            if sessions.get_attempt(&attempt_id).is_ok() {
                let cancelled = sessions
                    .transition_attempt(
                        &attempt_id,
                        "cancelled",
                        "Work cancelled by user",
                        None,
                        None,
                    )
                    .map_err(internal_error)?;
                if cancelled.status != "cancelled" {
                    return Ok(Json(json!(board.get(&id).map_err(internal_error)?)));
                }
                if let Some(queue_id) = cancelled.queue_id {
                    state
                        .db
                        .with_conn(|conn| {
                            conn.execute(
                                "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                                    harness_response = 'cancelled by user' WHERE id = ?1",
                                [queue_id],
                            )?;
                            Ok::<_, xpressclaw_core::error::Error>(())
                        })
                        .map_err(internal_error)?;
                }
            }
            state.elicitations.cancel_attempt(&attempt_id);
            if let Some(docker) = state.docker().await {
                let _ = docker.stop(&format!("attempt-{attempt_id}")).await;
            }
        } else {
            state.elicitations.cancel_task(&id);
        }
    }
    let updated = if req.status == "completed" {
        board
            .complete_and_roll_up(&id, req.agent_id.as_deref())
            .and_then(|tasks| {
                tasks
                    .into_iter()
                    .next()
                    .ok_or_else(|| xpressclaw_core::error::Error::Task("task is not ready".into()))
            })
    } else {
        board.update_status(&id, &req.status, req.agent_id.as_deref())
    };
    let task = updated.map_err(|e| match &e {
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

async fn get_message_attachment(
    State(state): State<AppState>,
    Path((id, message_id, attachment_id)): Path<(String, i64, String)>,
) -> Result<Response<Body>, (StatusCode, Json<Value>)> {
    let attachment = TaskConversation::new(state.db.clone())
        .get_attachment(&id, message_id, &attachment_id)
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "image attachment not found" })),
            )
        })?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, attachment.mime_type)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .body(Body::from(attachment.data))
        .map_err(internal_error)
}

async fn get_activity(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<ActivityParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if params.after.is_some() && params.before.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "use either 'after' or 'before', not both" })),
        ));
    }
    TaskBoard::new(state.db.clone())
        .get(&id)
        .map_err(|error| match &error {
            xpressclaw_core::error::Error::TaskNotFound { .. } => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": error.to_string() })),
            ),
            _ => internal_error(error),
        })?;
    let activity = SessionManager::new(state.db.clone())
        .task_activity(
            &id,
            params.after,
            params.before,
            params.limit.unwrap_or(250).clamp(1, 500),
            20,
        )
        .map_err(internal_error)?;
    Ok(Json(json!(activity)))
}

async fn respond_to_elicitation(
    State(state): State<AppState>,
    Path((id, elicitation_id)): Path<(String, String)>,
    Json(req): Json<ElicitationResponseInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    TaskBoard::new(state.db.clone())
        .get(&id)
        .map_err(|error| match &error {
            xpressclaw_core::error::Error::TaskNotFound { .. } => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": error.to_string() })),
            ),
            _ => internal_error(error),
        })?;

    if !matches!(req.action.as_str(), "accept" | "decline" | "cancel") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "action must be accept, decline, or cancel" })),
        ));
    }
    let mut wire = json!({ "action": req.action });
    if req.action == "accept" {
        let content = req.content.clone().unwrap_or_else(|| json!({}));
        if !content.is_object() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "accepted elicitation content must be an object" })),
            ));
        }
        wire["content"] = content;
    }
    let response: CreateElicitationResponse = serde_json::from_value(wire).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid elicitation response: {error}") })),
        )
    })?;

    state
        .elicitations
        .respond(&id, &elicitation_id, response)
        .map_err(|error| match error {
            AcpElicitationResponseError::WrongTask => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "elicitation does not belong to this task" })),
            ),
            AcpElicitationResponseError::NotFound | AcpElicitationResponseError::Closed => (
                StatusCode::CONFLICT,
                Json(json!({ "error": "this question is no longer awaiting a response" })),
            ),
        })?;

    if req.action == "accept" {
        let message = req
            .message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "Answered the agent's question".to_string());
        if let Err(error) =
            TaskConversation::new(state.db.clone()).add_message(&id, "user", &message)
        {
            tracing::warn!(%error, task_id = id, "failed to persist elicitation answer in task chat");
        }
    }

    Ok(Json(json!({ "resolved": true, "action": req.action })))
}

async fn add_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if req.role != "user" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "task chat only accepts user messages" })),
        ));
    }
    let content = req.content.trim().to_string();
    if content.is_empty() && req.attachments.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "message must include text or an image" })),
        ));
    }
    let attachments = decode_image_attachments(&req.attachments).map_err(bad_request)?;

    let board = TaskBoard::new(state.db.clone());
    let task = board.get(&id).map_err(|error| match &error {
        xpressclaw_core::error::Error::TaskNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        ),
        _ => internal_error(error),
    })?;
    let conv = TaskConversation::new(state.db.clone());
    let msg = conv
        .add_message_with_attachments(&id, &req.role, &content, &attachments)
        .map_err(internal_error)?;

    let summary = if content.is_empty() {
        if attachments.len() == 1 {
            "Sent an image".to_string()
        } else {
            format!("Sent {} images", attachments.len())
        }
    } else {
        content.clone()
    };

    let mut continuation = None;
    let mut delivery = "stored";
    if let Some(ref agent_id) = task.agent_id {
        let sessions = SessionManager::new(state.db.clone());
        sessions
            .ensure(agent_id, Some(agent_id))
            .map_err(internal_error)?;
        let active_attempt = sessions
            .task_activity(&id, None, None, 1, 50)
            .map_err(internal_error)?
            .attempts
            .into_iter()
            .find(|attempt| {
                matches!(
                    attempt.status.as_str(),
                    "preparing" | "running" | "waiting_for_input" | "review"
                )
            });
        sessions
            .append_event(
                agent_id,
                xpressclaw_core::sessions::NewEvent {
                    attempt_id: None,
                    task_id: Some(&id),
                    source_type: "user",
                    source_id: Some("local-user"),
                    event_type: "task_message_received",
                    summary: &summary,
                    payload: json!({
                        "role": req.role,
                        "content": content,
                        "attachments": msg.attachments,
                        "config_options": req.config_options,
                    }),
                },
            )
            .map_err(internal_error)?;

        let queue = TaskQueue::new(state.db.clone());
        continuation = queue
            .enqueue_continuation(&id, agent_id)
            .map_err(internal_error)?;
        delivery = if let Some(active_attempt) = active_attempt {
            if req.delivery == MessageDelivery::Immediate {
                state
                    .interrupt_attempt(&active_attempt.id)
                    .await
                    .map_err(internal_error)?;
                "immediate"
            } else {
                state
                    .turn_controls
                    .request_interrupt(&active_attempt.id, AcpInterruptMode::AfterTool);
                "after_tool"
            }
        } else {
            "queued"
        };
        if continuation.is_some()
            && matches!(
                task.status,
                TaskStatus::WaitingForInput
                    | TaskStatus::Blocked
                    | TaskStatus::Completed
                    | TaskStatus::Cancelled
            )
        {
            board
                .update_status(&id, "pending", Some(agent_id))
                .map_err(internal_error)?;
        }
        tracing::info!(
            task_id = id,
            agent_id,
            continuation_queued = continuation.is_some(),
            "received task chat message"
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "message": msg,
            "continuation_queued": continuation.is_some(),
            "attempt_id": continuation.and_then(|item| item.attempt_id),
            "delivery": delivery,
        })),
    ))
}

/// Batch create tasks with ref-based dependencies (ADR-020).
async fn create_tasks_batch(
    State(state): State<AppState>,
    Json(req): Json<BatchCreateRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    let tasks = board
        .create_batch(&req.tasks, req.parent_task_id.as_deref())
        .map_err(internal_error)?;

    // Enqueue tasks that have agents assigned
    let queue = xpressclaw_core::tasks::queue::TaskQueue::new(state.db.clone());
    for task in &tasks {
        if let Some(ref agent_id) = task.agent_id {
            let _ = queue.enqueue(&task.id, agent_id);
        }
    }

    Ok((StatusCode::CREATED, Json(json!(tasks))))
}

#[derive(Deserialize)]
struct BatchCreateRequest {
    tasks: Vec<xpressclaw_core::tasks::board::BatchTaskInput>,
    parent_task_id: Option<String>,
}

/// Add a dependency to an existing task.
#[derive(Deserialize)]
struct AddDependencyRequest {
    depends_on: String,
}

async fn add_dependency(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddDependencyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let board = TaskBoard::new(state.db.clone());
    board
        .add_dependency(&id, &req.depends_on)
        .map_err(internal_error)?;
    Ok(Json(json!({ "task_id": id, "depends_on": req.depends_on })))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

fn bad_request(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
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

    fn test_app_with_db() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db.clone(),
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        (Router::new().nest("/tasks", routes()).with_state(state), db)
    }

    fn test_app() -> Router {
        test_app_with_db().0
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
    async fn test_recent_tasks_are_limited_per_agent_after_ordering() {
        let (app, db) = test_app_with_db();
        let board = TaskBoard::new(db.clone());
        let create_task = |title: &str, agent_id: Option<&str>, priority: i32| {
            board
                .create(&CreateTask {
                    title: title.to_string(),
                    agent_id: agent_id.map(str::to_string),
                    priority: Some(priority),
                    ..Default::default()
                })
                .unwrap()
        };
        let alpha_old = create_task("Alpha old priority", Some("alpha"), 100);
        let alpha_second = create_task("Alpha second", Some("alpha"), 0);
        let alpha_newest = create_task("Alpha newest", Some("alpha"), 0);
        let beta_old = create_task("Beta old", Some("beta"), 0);
        let beta_second = create_task("Beta second", Some("beta"), 0);
        let beta_newest = create_task("Beta newest", Some("beta"), 0);
        let unassigned_old = create_task("Unassigned old", None, 0);
        let unassigned_second = create_task("Unassigned second", None, 0);
        let unassigned_newest = create_task("Unassigned newest", None, 0);

        {
            let conn = db.conn();
            for (task, updated_at) in [
                (&alpha_old, "2026-01-01 00:00:00"),
                (&unassigned_old, "2026-01-02 00:00:00"),
                (&beta_old, "2026-01-03 00:00:00"),
                (&unassigned_second, "2026-01-04 00:00:00"),
                (&alpha_second, "2026-01-05 00:00:00"),
                (&beta_second, "2026-01-06 00:00:00"),
                (&unassigned_newest, "2026-01-07 00:00:00"),
                (&alpha_newest, "2026-01-08 00:00:00"),
                (&beta_newest, "2026-01-09 00:00:00"),
            ] {
                conn.execute(
                    "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
                    [updated_at, task.id.as_str()],
                )
                .unwrap();
            }
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tasks/recent-by-agent?limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let task_ids = body["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            task_ids,
            vec![
                beta_newest.id,
                alpha_newest.id,
                unassigned_newest.id,
                beta_second.id,
                alpha_second.id,
                unassigned_second.id,
            ]
        );
    }

    #[tokio::test]
    async fn test_list_tasks_orders_recent_before_limiting() {
        let (app, db) = test_app_with_db();
        let board = TaskBoard::new(db.clone());
        let create_task = |title: &str, agent_id: &str, priority: i32| {
            board
                .create(&CreateTask {
                    title: title.to_string(),
                    agent_id: Some(agent_id.to_string()),
                    priority: Some(priority),
                    ..Default::default()
                })
                .unwrap()
        };
        let old_high_priority = create_task("Old high priority", "atlas", 100);
        let second_newest = create_task("Second newest", "atlas", 0);
        let newest = create_task("Newest", "atlas", 0);
        let waiting = create_task("Waiting", "atlas", 0);
        let other_agent = create_task("Other agent", "zephyr", 0);

        {
            let conn = db.conn();
            for (task, status, updated_at) in [
                (&old_high_priority, "pending", "2026-01-01 00:00:00"),
                (&second_newest, "completed", "2026-01-03 00:00:00"),
                (&newest, "pending", "2026-01-04 00:00:00"),
                (&waiting, "waiting_for_input", "2026-01-05 00:00:00"),
                (&other_agent, "pending", "2026-01-06 00:00:00"),
            ] {
                conn.execute(
                    "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    [status, updated_at, task.id.as_str()],
                )
                .unwrap();
            }
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri(
                        "/tasks?agent_id=atlas&sort=recent&exclude_statuses=waiting_for_input%2Cblocked&limit=2",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let task_ids = body["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(task_ids, vec![newest.id, second_newest.id]);
    }

    #[tokio::test]
    async fn test_list_tasks_filters_statuses_and_applies_offset() {
        let (app, db) = test_app_with_db();
        let board = TaskBoard::new(db.clone());
        let create_task = |title: &str| {
            board
                .create(&CreateTask {
                    title: title.to_string(),
                    ..Default::default()
                })
                .unwrap()
        };
        let pending = create_task("Pending");
        let completed_first = create_task("Completed first");
        let cancelled = create_task("Cancelled");
        let completed_last = create_task("Completed last");

        {
            let conn = db.conn();
            for (task, status, created_at) in [
                (&pending, "pending", "2026-01-01 00:00:00"),
                (&completed_first, "completed", "2026-01-02 00:00:00"),
                (&cancelled, "cancelled", "2026-01-03 00:00:00"),
                (&completed_last, "completed", "2026-01-04 00:00:00"),
            ] {
                conn.execute(
                    "UPDATE tasks SET status = ?1, created_at = ?2, updated_at = ?2 WHERE id = ?3",
                    [status, created_at, task.id.as_str()],
                )
                .unwrap();
            }
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tasks?statuses=completed%2Ccancelled&limit=2&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        let task_ids = body["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(task_ids, vec![cancelled.id, completed_last.id]);
        assert_eq!(body["counts"]["completed"], 2);
        assert_eq!(body["counts"]["cancelled"], 1);
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
    async fn cancelling_after_attempt_completion_preserves_terminal_state() {
        let (app, db) = test_app_with_db();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": "Race task", "agent_id": "developer"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        let queue = TaskQueue::new(db.clone());
        let queue_item = queue.list(Some("developer"), Some("queued"), 1).unwrap()[0].clone();
        let attempt_id = queue_item.attempt_id.as_deref().unwrap().to_string();
        let sessions = SessionManager::new(db.clone());
        sessions
            .transition_attempt(&attempt_id, "completed", "Done", Some("Done"), None)
            .unwrap();
        queue.complete(queue_item.id, "Done").unwrap();
        TaskBoard::new(db.clone())
            .update_status(&task_id, "completed", Some("developer"))
            .unwrap();

        // Model a cancel handler that read the active attempt immediately
        // before the worker committed its terminal transition.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET active_attempt_id = ?1 WHERE id = ?2",
                [attempt_id.as_str(), task_id.as_str()],
            )?;
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/tasks/{task_id}/status"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"status": "cancelled"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;

        assert_eq!(body["status"], "completed");
        assert_eq!(
            sessions.get_attempt(&attempt_id).unwrap().status,
            "completed"
        );
        let queue_item = queue.get(queue_item.id).unwrap();
        assert_eq!(queue_item.status, "completed");
        assert_eq!(queue_item.harness_response.as_deref(), Some("Done"));
        assert_eq!(
            TaskBoard::new(db).get(&task_id).unwrap().status,
            TaskStatus::Completed
        );
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
        assert_eq!(body["message"]["role"], "user");
        assert_eq!(body["message"]["content"], "Hello agent");
        assert_eq!(body["continuation_queued"], false);
        assert_eq!(body["delivery"], "stored");

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

    #[tokio::test]
    async fn image_only_task_message_can_be_previewed() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Image message"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tasks/{task_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "role": "user",
                            "content": "",
                            "attachments": [{
                                "name": "screen.png",
                                "mime_type": "image/png",
                                "data": "iVBORw0KGgpieXRlcw=="
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        let message_id = body["message"]["id"].as_i64().unwrap();
        let attachment = &body["message"]["attachments"][0];
        assert_eq!(attachment["name"], "screen.png");
        assert_eq!(attachment["size"], 13);
        assert!(attachment.get("data").is_none());
        let attachment_id = attachment["id"].as_str().unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/tasks/{task_id}/messages/{message_id}/attachments/{attachment_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "image/png");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), b"\x89PNG\r\n\x1a\nbytes");
    }

    #[tokio::test]
    async fn rejects_an_image_with_a_mismatched_type() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Bad image"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tasks/{task_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "role": "user",
                            "content": "Inspect this",
                            "attachments": [{
                                "name": "fake.jpg",
                                "mime_type": "image/jpeg",
                                "data": "iVBORw0KGgpieXRlcw=="
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn completed_task_message_queues_one_continuation() {
        let (app, db) = test_app_with_db();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": "Correct this", "agent_id": "developer"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();

        let queue = TaskQueue::new(db.clone());
        let first = queue.list(Some("developer"), Some("queued"), 10).unwrap()[0].clone();
        let attempt_id = first.attempt_id.as_deref().unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(attempt_id, "completed", "Done", Some("Done"), None)
            .unwrap();
        queue.complete(first.id, "Done").unwrap();
        TaskBoard::new(db.clone())
            .update_status(&task_id, "completed", Some("developer"))
            .unwrap();

        let send = |content: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/tasks/{task_id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"role": "user", "content": content}).to_string(),
                ))
                .unwrap()
        };
        let resp = app
            .clone()
            .oneshot(send("Please fix the mistake"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["continuation_queued"], true);
        assert!(body["attempt_id"].is_string());
        assert_eq!(body["delivery"], "queued");
        let reopened = TaskBoard::new(db.clone()).get(&task_id).unwrap();
        assert_eq!(reopened.status, TaskStatus::Pending);
        assert!(reopened.completed_at.is_none());

        let resp = app.clone().oneshot(send("One more detail")).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["continuation_queued"], false);
        assert_eq!(queue.pending_count("developer").unwrap(), 1);
    }

    #[tokio::test]
    async fn running_task_message_yields_after_the_current_tool() {
        let (app, db) = test_app_with_db();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": "Steer this", "agent_id": "developer"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();
        let queue = TaskQueue::new(db.clone());
        let first = queue.claim("developer").unwrap().unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                first.attempt_id.as_deref().unwrap(),
                "running",
                "Working",
                None,
                None,
            )
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tasks/{task_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"role": "user", "content": "Use the smaller API"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["delivery"], "after_tool");
        assert_eq!(body["continuation_queued"], true);
        assert_eq!(
            SessionManager::new(db)
                .get_attempt(first.attempt_id.as_deref().unwrap())
                .unwrap()
                .status,
            "running"
        );
    }

    #[tokio::test]
    async fn immediate_message_interrupts_the_running_attempt_and_keeps_the_continuation() {
        let (app, db) = test_app_with_db();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": "Interrupt this", "agent_id": "developer"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap().to_string();
        let queue = TaskQueue::new(db.clone());
        let first = queue.claim("developer").unwrap().unwrap();
        let attempt_id = first.attempt_id.as_deref().unwrap().to_string();
        SessionManager::new(db.clone())
            .transition_attempt(&attempt_id, "running", "Working", None, None)
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tasks/{task_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "role": "user",
                            "content": "Stop and do this instead",
                            "delivery": "immediate"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["delivery"], "immediate");
        assert_eq!(body["continuation_queued"], true);
        assert_eq!(
            SessionManager::new(db.clone())
                .get_attempt(&attempt_id)
                .unwrap()
                .status,
            "interrupted"
        );
        assert_eq!(
            TaskBoard::new(db).get(&task_id).unwrap().status,
            TaskStatus::InProgress
        );
    }

    #[tokio::test]
    async fn task_activity_includes_native_attempt_events() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "title": "Visible native work",
                            "agent_id": "website-codex"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let task_id = body["id"].as_str().unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/tasks/{task_id}/activity"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["attempts"].as_array().unwrap().len(), 1);
        assert!(body["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event_type"] == "attempt_queued"));
    }
}
