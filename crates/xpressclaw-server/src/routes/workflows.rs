use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::workflows::engine::WorkflowEngine;
use xpressclaw_core::workflows::instance::InstanceManager;
use xpressclaw_core::workflows::manager::{CreateWorkflow, WorkflowManager};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_workflows).post(create_workflow))
        .route(
            "/{id}",
            get(get_workflow)
                .put(update_workflow)
                .delete(delete_workflow),
        )
        .route("/{id}/enable", post(enable_workflow))
        .route("/{id}/disable", post(disable_workflow))
        .route("/{id}/run", post(run_workflow))
        .route("/{id}/instances", get(list_instances))
        .route("/instances/{instance_id}", get(get_instance))
        .route("/instances/{instance_id}/cancel", post(cancel_instance))
}

// -- Handlers --

async fn list_workflows(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let list = mgr.list().map_err(internal_error)?;
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct CreateWorkflowReq {
    name: String,
    #[serde(default)]
    description: Option<String>,
    yaml_content: String,
}

async fn create_workflow(
    State(state): State<AppState>,
    Json(req): Json<CreateWorkflowReq>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let wf = mgr
        .create(&CreateWorkflow {
            name: req.name,
            description: req.description,
            yaml_content: req.yaml_content,
        })
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::Workflow(_) => bad_request(&e),
            _ => internal_error(e),
        })?;
    Ok((StatusCode::CREATED, Json(json!(wf))))
}

async fn get_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let wf = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(wf)))
}

async fn update_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateWorkflowReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let wf = mgr.update(&id, &req.yaml_content).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        xpressclaw_core::error::Error::Workflow(_) => bad_request(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(wf)))
}

async fn delete_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    mgr.delete(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let wf = mgr.set_enabled(&id, true).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(wf)))
}

async fn disable_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let wf = mgr.set_enabled(&id, false).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(wf)))
}

async fn run_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(trigger_data): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let engine = WorkflowEngine::new(state.db.clone());
    let instance_id = engine
        .start_instance(&id, trigger_data)
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
            _ => internal_error(e),
        })?;

    let im = InstanceManager::new(state.db.clone());
    let instance = im.get_instance(&instance_id).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(instance))))
}

async fn list_instances(
    State(state): State<AppState>,
    Path(workflow_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let im = InstanceManager::new(state.db.clone());
    let list = im
        .list_instances(&workflow_id, 50)
        .map_err(internal_error)?;
    Ok(Json(json!(list)))
}

async fn get_instance(
    State(state): State<AppState>,
    Path(instance_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let im = InstanceManager::new(state.db.clone());
    let instance = im.get_instance(&instance_id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowInstanceNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    // Include step executions
    let executions = im
        .list_step_executions(&instance_id)
        .map_err(internal_error)?;
    Ok(Json(json!({
        "instance": instance,
        "step_executions": executions,
    })))
}

async fn cancel_instance(
    State(state): State<AppState>,
    Path(instance_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let im = InstanceManager::new(state.db.clone());
    im.update_status(&instance_id, "cancelled", None)
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::WorkflowInstanceNotFound { .. } => not_found(&e),
            _ => internal_error(e),
        })?;
    let instance = im.get_instance(&instance_id).map_err(internal_error)?;
    Ok(Json(json!(instance)))
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
