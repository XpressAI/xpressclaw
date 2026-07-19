use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{default_native_runner_image, AgentConfig, ContainerEngineAccess};
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::sessions::{NewEvent, SessionManager};
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::tasks::queue::TaskQueue;
use xpressclaw_core::workers::native::{
    local_runner_image_alias, resolve_runner_kind, resolved_runner_image, runner_image_compatible,
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
    if available_runner_image(&docker, &image).await.is_none() {
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
        agent.runner.subscription_auth && matches!(kind.as_str(), "codex" | "claude" | "opencode");
    let auth_present = !auth_required || subscription_auth_available(&kind);
    let docker = state.docker().await;
    let docker_available = docker.is_some();
    let container_engine_available = agent.runner.container_engine != ContainerEngineAccess::Host
        || docker
            .as_ref()
            .is_some_and(|docker| docker.host_engine_socket().is_some());
    let runtime_image = match docker.as_ref() {
        Some(docker) => available_runner_image(docker, &image).await,
        None => None,
    };
    let image_present = runtime_image.is_some();
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
        "ready": docker_available && image_present && workspace_present && auth_present && command_present && container_engine_available,
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
        "command_present": command_present,
        "subscription_auth": agent.runner.subscription_auth,
        "auth_present": auth_present,
        "issues": issues,
    }))
}

async fn available_runner_image(docker: &DockerManager, image: &str) -> Option<String> {
    let host_engine_image = matches!(
        image,
        "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-claude-docker:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-opencode-docker:latest"
    );
    let built_in = matches!(
        image,
        "ghcr.io/xpressai/xpressclaw-runner-codex:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-claude:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-opencode:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-claude-docker:latest"
            | "ghcr.io/xpressai/xpressclaw-runner-opencode-docker:latest"
    );
    if runner_image_compatible(docker, image, built_in, host_engine_image).await {
        return Some(image.to_string());
    }
    if let Some(local_image) = local_runner_image_alias(image) {
        if runner_image_compatible(docker, local_image, built_in, host_engine_image).await {
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
                payload: json!({
                    "content": input.content.trim(),
                    "config_options": input.config_options.clone(),
                }),
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
                "session_mode": if input.new_session { "new" } else { "continue" },
                "session_config": input.config_options,
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

    state.elicitations.cancel_attempt(&attempt_id);
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
        assert!(value["issues"].is_array());
    }
}
