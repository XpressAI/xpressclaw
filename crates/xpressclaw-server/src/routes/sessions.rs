use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::acp::{is_builtin_runner_image, is_host_runner_image};
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{default_native_runner_image, AgentConfig, ContainerEngineAccess};
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::sessions::{NewEvent, SessionManager};
use xpressclaw_core::tasks::attachments::{decode_image_attachments, ImageAttachmentInput};
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::tasks::conversation::TaskConversation;
use xpressclaw_core::tasks::queue::TaskQueue;
use xpressclaw_core::workers::native::{
    host_ssh_agent_socket, local_runner_image_alias, presentation_runtime_available,
    resolve_runner_kind, resolved_runner_image, runner_image_compatible,
    subscription_auth_available,
};

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
    #[serde(default)]
    attachments: Vec<ImageAttachmentInput>,
    priority: Option<i32>,
    #[serde(default)]
    new_session: bool,
    #[serde(default)]
    config_options: std::collections::HashMap<String, Value>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{id}", get(get_session))
        .route("/{id}/events", get(get_events))
        .route("/{id}/attempts", get(get_attempts))
        .route("/{id}/readiness", get(get_readiness).post(prepare_runner))
        .route("/{id}/messages", post(post_message))
        .route(
            "/{id}/attempts/{attempt_id}/interrupt",
            post(interrupt_attempt),
        )
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

async fn get_readiness(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let agent = session_config(&state, &id)?;
    Ok(Json(readiness(&state, &agent).await?))
}

async fn prepare_runner(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let agent = session_config(&state, &id)?;
    let kind = resolve_runner_kind(&agent).map_err(bad_request)?;
    if kind == "custom" && agent.runner.command.is_empty() {
        return Err(bad_request(
            "custom ACP agents require a server command in the Runner tab",
        ));
    }
    let image = resolved_runner_image(&agent.runner, &kind).map_err(bad_request)?;
    let docker = state.docker().await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "Docker or Podman is not available" })),
        )
    })?;
    if agent.runner.container_engine == ContainerEngineAccess::Host
        && docker.host_engine_socket().is_none()
    {
        return Err(bad_request(
            "host container-engine access requires a local Docker-compatible Unix socket",
        ));
    }
    if agent.runner.ssh_agent_forwarding && host_ssh_agent_socket().is_none() {
        return Err(bad_request(
            "host SSH-agent forwarding is enabled, but no live Unix SSH_AUTH_SOCK was detected",
        ));
    }
    if available_runner_image(&docker, &image, &kind)
        .await
        .is_none()
    {
        docker.pull_image(&image).await.map_err(internal_error)?;
    }
    let readiness = readiness(&state, &agent).await?;
    if readiness
        .get("ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        AgentRegistry::new(state.db.clone())
            .clear_error(&id)
            .map_err(internal_error)?;
    }
    Ok(Json(readiness))
}

async fn readiness(
    state: &AppState,
    agent: &AgentConfig,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let kind = resolve_runner_kind(agent).map_err(bad_request)?;
    let image = resolved_runner_image(&agent.runner, &kind).map_err(bad_request)?;
    let workspace = session_workspace(state, agent);
    let workspace_present = workspace.is_dir();
    let command_present = kind != "custom" || !agent.runner.command.is_empty();
    let auth_required =
        agent.runner.subscription_auth && xpressclaw_core::acp::agent_definition(&kind).is_some();
    let auth_present = !auth_required || subscription_auth_available(&kind);
    let docker = state.docker().await;
    let docker_available = docker.is_some();
    let container_engine_available = agent.runner.container_engine != ContainerEngineAccess::Host
        || docker
            .as_ref()
            .is_some_and(|docker| docker.host_engine_socket().is_some());
    let ssh_agent_socket = host_ssh_agent_socket();
    let ssh_agent_available = ssh_agent_socket.is_some();
    let ssh_agent_ready = !agent.runner.ssh_agent_forwarding || ssh_agent_available;
    let runtime_image = match docker.as_ref() {
        Some(docker) => available_runner_image(docker, &image, &kind).await,
        None => None,
    };
    let image_present = runtime_image.is_some();
    let presentation_supported = kind == "codex";
    let presentation_available = match (docker.as_ref(), runtime_image.as_deref()) {
        (Some(docker), Some(image)) if presentation_supported => {
            presentation_runtime_available(docker, image).await
        }
        _ => false,
    };
    let mut issues = Vec::new();
    if !docker_available {
        issues.push("Docker or Podman is not available".to_string());
    } else if !image_present {
        let detail = if default_native_runner_image(&kind, agent.runner.container_engine)
            == Some(image.as_str())
        {
            "has not been pulled or is missing the required ACP/container-engine compatibility labels"
        } else {
            "has not been pulled"
        };
        issues.push(format!("Runner image {image} {detail}"));
    }
    if !workspace_present {
        issues.push(format!(
            "Workspace {} does not exist or is not a directory",
            workspace.display()
        ));
    }
    if !container_engine_available {
        issues.push(
            "Host container-engine access needs a local Docker or Podman Unix socket".to_string(),
        );
    }
    if !ssh_agent_ready {
        issues.push(
            "Host SSH-agent forwarding is enabled, but no live Unix SSH_AUTH_SOCK was detected; start an SSH agent and restart XpressClaw from that desktop session"
                .to_string(),
        );
    }
    if !command_present {
        issues.push(
            "Custom ACP server command is not configured; add it in the Runner tab".to_string(),
        );
    }
    if !auth_present {
        issues.push(format!(
            "No host {kind} login was found; authenticate with {kind} on the host first"
        ));
    }
    Ok(json!({
        "protocol": "acp",
        "ready": docker_available && image_present && workspace_present && auth_present && command_present && container_engine_available && ssh_agent_ready,
        "docker_available": docker_available,
        "container_runtime": docker.as_ref().map(|docker| docker.runtime()),
        "container_runtime_version": docker.as_ref().and_then(|docker| docker.runtime_version()),
        "kind": kind,
        "image": image,
        "runtime_image": runtime_image,
        "image_present": image_present,
        "workspace": workspace.display().to_string(),
        "workspace_present": workspace_present,
        "model": agent.runner.model,
        "container_engine": agent.runner.container_engine,
        "container_engine_available": container_engine_available,
        "container_engine_socket": docker.as_ref().and_then(|docker| docker.host_engine_socket()).map(|path| path.display().to_string()),
        "ssh_agent_forwarding": agent.runner.ssh_agent_forwarding,
        "ssh_agent_available": ssh_agent_available,
        "ssh_agent_socket": ssh_agent_socket.map(|path| path.display().to_string()),
        "command_present": command_present,
        "subscription_auth": agent.runner.subscription_auth,
        "auth_present": auth_present,
        "presentation_artifacts": {
            "supported": presentation_supported,
            "available": presentation_available,
            "capability": presentation_available.then_some(xpressclaw_core::workers::presentations::PRESENTATION_CAPABILITY),
            "runtime": presentation_available.then_some(format!("PptxGenJS {}", xpressclaw_core::workers::presentations::PRESENTATION_RUNTIME_VERSION)),
            "reason": if presentation_supported && !presentation_available {
                Some("This runner image does not include the pinned XpressClaw presentation runtime; incompatible OpenAI desktop artifact skills are disabled")
            } else {
                None
            },
        },
        "issues": issues,
    }))
}

async fn available_runner_image(docker: &DockerManager, image: &str, kind: &str) -> Option<String> {
    let host_engine_image = is_host_runner_image(image);
    let built_in = is_builtin_runner_image(image);
    let pi_mcp_image = built_in && kind == "pi";
    if runner_image_compatible(docker, image, built_in, host_engine_image, pi_mcp_image).await {
        return Some(image.to_string());
    }
    if let Some(local_image) = local_runner_image_alias(image) {
        if runner_image_compatible(
            docker,
            local_image,
            built_in,
            host_engine_image,
            pi_mcp_image,
        )
        .await
        {
            return Some(local_image.to_string());
        }
    }
    None
}

fn session_config(state: &AppState, id: &str) -> Result<AgentConfig, (StatusCode, Json<Value>)> {
    state
        .config()
        .agents
        .iter()
        .find(|agent| agent.name == id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("session configuration not found: {id}") })),
            )
        })
}

fn session_workspace(state: &AppState, agent: &AgentConfig) -> std::path::PathBuf {
    let configured = agent
        .runner
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let Some(configured) = configured else {
        return state.config().system.workspace_dir.clone();
    };
    if let Some(rest) = configured.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(configured)
}

async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<MessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let content = input.content.trim().to_string();
    if content.is_empty() && input.attachments.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "message must include text or an image" })),
        ));
    }
    let attachments = decode_image_attachments(&input.attachments).map_err(bad_request)?;
    let summary = if content.is_empty() {
        if attachments.len() == 1 {
            "Sent an image".to_string()
        } else {
            format!("Sent {} images", attachments.len())
        }
    } else {
        content.clone()
    };
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
                summary: &summary,
                payload: json!({
                    "content": content,
                    "attachments": attachments.iter().map(|attachment| json!({
                        "name": attachment.name,
                        "mime_type": attachment.mime_type,
                        "size": attachment.data.len(),
                    })).collect::<Vec<_>>(),
                    "config_options": input.config_options.clone(),
                }),
            },
        )
        .map_err(internal_error)?;

    let title = if content.is_empty() {
        if attachments.len() == 1 {
            "Image attachment".to_string()
        } else {
            "Image attachments".to_string()
        }
    } else {
        concise_title(&content)
    };
    let board = TaskBoard::new(state.db.clone());
    let task = board
        .create(&CreateTask {
            title,
            description: (!content.is_empty()).then(|| content.clone()),
            agent_id: Some(id.clone()),
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            priority: input.priority,
            context: Some(json!({
                "origin": "session_message",
                "kind": "interactive",
                "source_event_id": event.id,
                "session_mode": if input.new_session { "new" } else { "continue" },
                "session_config": input.config_options,
            })),
        })
        .map_err(internal_error)?;
    TaskConversation::new(state.db.clone())
        .add_message_with_attachments(&task.id, "user", &content, &attachments)
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
        return Ok(Json(json!(cancelled)));
    }

    state.elicitations.cancel_attempt(&attempt_id);
    let mut container_stopped = cancelled.container_id.is_none();
    if let Some(docker) = state.docker().await {
        container_stopped = docker.stop_preserving(&cancelled.session_id).await.is_ok();
    }
    if container_stopped {
        let _ = sessions.clear_container(&attempt_id);
    }
    // The running queue row is the lease on the shared Agent container.
    // Release it only after the retained environment has stopped.
    state
        .db
        .with_conn(|conn| {
            if let Some(queue_id) = cancelled.queue_id {
                conn.execute(
                    "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                        harness_response = 'cancelled by user' WHERE id = ?1",
                    [queue_id],
                )?;
            }
            if let Some(task_id) = cancelled.task_id.as_deref() {
                conn.execute(
                    "UPDATE tasks SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [task_id],
                )?;
            }
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .map_err(internal_error)?;
    Ok(Json(json!(cancelled)))
}

async fn interrupt_attempt(
    State(state): State<AppState>,
    Path((id, attempt_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ensure_session(&state, &id)?;
    let attempt = SessionManager::new(state.db.clone())
        .get_attempt(&attempt_id)
        .map_err(not_found)?;
    if attempt.session_id != id {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "attempt does not belong to this session" })),
        ));
    }
    let interrupted = state
        .interrupt_attempt(&attempt_id)
        .await
        .map_err(internal_error)?;
    Ok(Json(json!(interrupted)))
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
        .map(|agent| agent.context_label())
        .unwrap_or(record.name);
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

fn bad_request(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
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
    use xpressclaw_core::tasks::board::TaskStatus;

    use super::*;

    fn test_app_with_db() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("builder", "codex")
            .unwrap();
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "builder".to_string(),
            backend: "codex".to_string(),
            ..Default::default()
        });
        let state = AppState::new(
            Arc::new(config),
            db.clone(),
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        (
            Router::new().nest("/sessions", routes()).with_state(state),
            db,
        )
    }

    fn test_app() -> Router {
        test_app_with_db().0
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
        assert_eq!(value["task"]["context"]["session_mode"], "continue");
    }

    #[tokio::test]
    async fn message_can_request_a_fresh_native_conversation() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/builder/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "content": "Try another approach", "new_session": true })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["task"]["context"]["session_mode"], "new");
    }

    #[tokio::test]
    async fn image_message_is_persisted_before_it_is_queued() {
        let (app, db) = test_app_with_db();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/builder/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
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
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["task"]["title"], "Image attachment");
        let task_id = value["task"]["id"].as_str().unwrap();
        let messages = TaskConversation::new(db).get_messages(task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.is_empty());
        assert_eq!(messages[0].attachments[0].name, "screen.png");
    }

    #[tokio::test]
    async fn interrupt_stops_only_the_waiting_attempt() {
        let (app, db) = test_app_with_db();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/builder/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "content": "Long task" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let attempt_id = value["attempt_id"].as_str().unwrap();
        let sessions = SessionManager::new(db.clone());
        sessions
            .transition_attempt(attempt_id, "running", "Working", None, None)
            .unwrap();
        sessions
            .transition_attempt(
                attempt_id,
                "waiting_for_input",
                "Waiting for your answer",
                None,
                None,
            )
            .unwrap();
        let task_id = value["task"]["id"].as_str().unwrap();
        TaskBoard::new(db.clone())
            .update_status(task_id, "waiting_for_input", Some("builder"))
            .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/builder/attempts/{attempt_id}/interrupt"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["status"], "interrupted");
        let task_id = value["task_id"].as_str().unwrap();
        assert_eq!(
            TaskBoard::new(db.clone()).get(task_id).unwrap().status,
            TaskStatus::Pending
        );
        assert_eq!(TaskQueue::new(db).pending_count("builder").unwrap(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_completed_attempt_preserves_completion_state() {
        let (app, db) = test_app_with_db();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/builder/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "content": "Quick task" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let attempt_id = value["attempt_id"].as_str().unwrap().to_string();
        let task_id = value["task"]["id"].as_str().unwrap().to_string();

        let sessions = SessionManager::new(db.clone());
        let queue = TaskQueue::new(db.clone());
        let queue_id = sessions.get_attempt(&attempt_id).unwrap().queue_id.unwrap();
        sessions
            .transition_attempt(&attempt_id, "completed", "Done", Some("Done"), None)
            .unwrap();
        queue.complete(queue_id, "Done").unwrap();
        TaskBoard::new(db.clone())
            .update_status(&task_id, "completed", Some("builder"))
            .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/builder/attempts/{attempt_id}/cancel"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["status"], "completed");
        assert_eq!(
            sessions.get_attempt(&attempt_id).unwrap().status,
            "completed"
        );
        let queue_item = queue.get(queue_id).unwrap();
        assert_eq!(queue_item.status, "completed");
        assert_eq!(queue_item.harness_response.as_deref(), Some("Done"));
        assert_eq!(
            TaskBoard::new(db).get(&task_id).unwrap().status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn readiness_describes_the_resolved_acp_agent() {
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/sessions/builder/readiness")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["protocol"], "acp");
        assert_eq!(value["kind"], "codex");
        assert_eq!(
            value["image"],
            "ghcr.io/xpressai/xpressclaw-runner-codex:latest"
        );
        assert!(value["workspace_present"].as_bool().is_some());
        assert_eq!(value["presentation_artifacts"]["supported"], true);
        assert_eq!(value["presentation_artifacts"]["available"], false);
        assert!(value["presentation_artifacts"]["reason"]
            .as_str()
            .unwrap()
            .contains("does not include the pinned XpressClaw presentation runtime"));
        assert!(value["issues"].is_array());
    }
}
