use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::{AgentRegistry, RegisterAgent};
use xpressclaw_core::agents::state::AgentStatus;
use xpressclaw_core::docker::images::image_for_backend;
use xpressclaw_core::docker::manager::{ContainerSpec, DockerManager};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    /// Override the harness image (optional).
    pub image: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_agents).post(register_agent))
        .route(
            "/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/{id}/start", axum::routing::post(start_agent))
        .route("/{id}/stop", axum::routing::post(stop_agent))
}

async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let agents = registry.list().map_err(internal_error)?;
    Ok(Json(json!(agents)))
}

async fn register_agent(
    State(state): State<AppState>,
    Json(req): Json<RegisterAgent>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.register(&req).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(record))))
}

async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(record)))
}

async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RegisterAgent>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Ensure agent exists first
    registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    let record = registry.register(&req).map_err(internal_error)?;
    Ok(Json(json!(record)))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Stop container if running
    if let Ok(record) = registry.get(&id) {
        if record.status == "running" {
            if let Ok(docker) = DockerManager::connect().await {
                let _ = docker.stop(&id).await;
            }
        }
    }
    registry.delete(&id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<StartRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    if record.status == "running" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("agent '{}' is already running", id) })),
        ));
    }

    // Mark as starting
    registry
        .update_status(&id, &AgentStatus::Starting, None)
        .map_err(internal_error)?;

    // Connect to Docker
    let docker = DockerManager::connect().await.map_err(|e| {
        // Revert status on failure
        let _ = registry.update_status(&id, &AgentStatus::Error(e.to_string()), None);
        internal_error(e)
    })?;

    // Determine image
    let image = body
        .and_then(|b| b.image.clone())
        .unwrap_or_else(|| image_for_backend(&record.backend).to_string());

    // Build container spec
    let mut spec = ContainerSpec {
        image,
        ..Default::default()
    };

    // Pass agent config as environment
    spec.environment.push(format!("AGENT_ID={}", id));
    spec.environment.push(format!("AGENT_NAME={}", record.name));
    spec.environment
        .push(format!("AGENT_BACKEND={}", record.backend));
    spec.environment
        .push(format!("AGENT_CONFIG={}", record.config));

    // Launch container
    match docker.launch(&id, &spec).await {
        Ok(info) => {
            let record = registry
                .update_status(&id, &AgentStatus::Running, Some(&info.container_id))
                .map_err(internal_error)?;
            Ok(Json(json!(record)))
        }
        Err(e) => {
            let record = registry
                .update_status(&id, &AgentStatus::Error(e.to_string()), None)
                .map_err(internal_error)?;
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": e.to_string(),
                    "agent": record,
                })),
            ))
        }
    }
}

async fn stop_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    if record.status == "stopped" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("agent '{}' is already stopped", id) })),
        ));
    }

    // Stop the container
    if let Ok(docker) = DockerManager::connect().await {
        let _ = docker.stop(&id).await;
    }

    let record = registry
        .update_status(&id, &AgentStatus::Stopped, None)
        .map_err(internal_error)?;
    Ok(Json(json!(record)))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

fn not_found(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
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
        let state = AppState {
            config,
            db,
            llm_router: None,
        };

        Router::new()
            .nest("/agents", routes())
            .with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_register_and_list() {
        let app = test_app();

        // Register
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {"role": "You are a helpful assistant"}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["name"], "atlas");
        assert_eq!(body["backend"], "generic");
        assert_eq!(body["status"], "stopped");

        // List
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agents")
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
    async fn test_get_agent() {
        let app = test_app();

        // Register
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Get
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agents/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["id"], "atlas");
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/agents/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_agent() {
        let app = test_app();

        // Register
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/atlas")
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
                    .uri("/agents/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_stop_already_stopped() {
        let app = test_app();

        // Register (starts as stopped)
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Stop already-stopped agent
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents/atlas/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
