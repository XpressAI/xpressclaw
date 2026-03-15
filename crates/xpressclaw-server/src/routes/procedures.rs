use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::tasks::sop::{CreateSop, SopManager, UpdateSop};

use crate::state::AppState;

/// Request body for running an SOP (creating a task from it).
#[derive(Debug, Deserialize)]
pub struct RunSopRequest {
    pub agent_id: String,
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
    pub priority: Option<i32>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_procedures).post(create_procedure))
        .route(
            "/{name}",
            get(get_procedure)
                .put(update_procedure)
                .delete(delete_procedure),
        )
        .route("/{name}/run", axum::routing::post(run_procedure))
}

async fn list_procedures(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    let sops = mgr.list().map_err(internal_error)?;
    Ok(Json(json!(sops)))
}

async fn create_procedure(
    State(state): State<AppState>,
    Json(req): Json<CreateSop>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    let sop = mgr.create(&req).map_err(|e| match &e {
        xpressclaw_core::error::Error::Sop(_) => bad_request(&e),
        _ => internal_error(e),
    })?;
    Ok((StatusCode::CREATED, Json(json!(sop))))
}

async fn get_procedure(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    let sop = mgr.get_by_name(&name).map_err(|e| match &e {
        xpressclaw_core::error::Error::SopNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(sop)))
}

async fn update_procedure(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<UpdateSop>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    let sop = mgr.update(&name, &req).map_err(|e| match &e {
        xpressclaw_core::error::Error::SopNotFound { .. } => not_found(&e),
        xpressclaw_core::error::Error::Sop(_) => bad_request(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(sop)))
}

async fn delete_procedure(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    mgr.delete(&name).map_err(|e| match &e {
        xpressclaw_core::error::Error::SopNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn run_procedure(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<RunSopRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = SopManager::new(state.db.clone());
    let board = TaskBoard::new(state.db.clone());

    let sop = mgr.get_by_name(&name).map_err(|e| match &e {
        xpressclaw_core::error::Error::SopNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    let description = mgr.format_task_description(&sop, &req.inputs);

    let task = board
        .create(&CreateTask {
            title: format!("SOP: {}", sop.name),
            description: Some(description),
            agent_id: Some(req.agent_id),
            parent_task_id: None,
            sop_id: Some(sop.id),
            priority: req.priority,
            context: if req.inputs.is_empty() {
                None
            } else {
                Some(json!(req.inputs))
            },
        })
        .map_err(internal_error)?;

    Ok((StatusCode::CREATED, Json(json!(task))))
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

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState {
            config,
            db,
            llm_router: None,
            config_path: std::path::PathBuf::from("test.yaml"),
            setup_complete: true,
        };

        Router::new()
            .nest("/procedures", routes())
            .with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    const TEST_SOP_CONTENT: &str = "summary: Deploy service\nsteps:\n  - name: Build\n    description: Build the image\n  - name: Deploy\n    description: Push to prod\ninputs:\n  - name: version\n    description: Version to deploy\n    required: true\noutputs:\n  - name: url\n    description: Deployed URL\n";

    async fn create_test_sop(app: &Router) -> Value {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/procedures")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "deploy-service",
                            "description": "Deploy a service",
                            "content": TEST_SOP_CONTENT
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        body_json(resp.into_body()).await
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let app = test_app();

        create_test_sop(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/procedures")
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
    async fn test_get_procedure() {
        let app = test_app();

        create_test_sop(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/procedures/deploy-service")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["name"], "deploy-service");
        assert!(body["parsed"]["steps"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/procedures/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_procedure() {
        let app = test_app();

        create_test_sop(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/procedures/deploy-service")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "description": "Updated description"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["description"], "Updated description");
    }

    #[tokio::test]
    async fn test_delete_procedure() {
        let app = test_app();

        create_test_sop(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/procedures/deploy-service")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify 404
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/procedures/deploy-service")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_run_procedure() {
        let app = test_app();

        create_test_sop(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/procedures/deploy-service/run")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agent_id": "atlas",
                            "inputs": {
                                "version": "v2.0.0"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let task = body_json(resp.into_body()).await;
        assert_eq!(task["title"], "SOP: deploy-service");
        assert_eq!(task["agent_id"], "atlas");
        assert!(task["description"]
            .as_str()
            .unwrap()
            .contains("**version**: v2.0.0"));
        assert!(task["sop_id"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_invalid_yaml_rejected() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/procedures")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "bad-sop",
                            "content": "not: [valid: yaml: here"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
