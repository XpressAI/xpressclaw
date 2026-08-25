use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::workflows::definition::WorkflowDefinition;
use xpressclaw_core::workflows::engine::{WorkflowContext, WorkflowEngine};
use xpressclaw_core::workflows::instance::InstanceManager;
use xpressclaw_core::workflows::manager::{CreateWorkflow, WorkflowManager};
use xpressclaw_core::workflows::waits::WaitState;

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
    reject_new_connector_automation(&req.yaml_content)?;
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
    let previous = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    let previous_uses_connectors = WorkflowDefinition::parse(&previous.yaml_content)
        .map_err(|e| bad_request(&e))?
        .uses_connector_automation();
    if !previous_uses_connectors {
        reject_new_connector_automation(&req.yaml_content)?;
    }
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
    let record = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    reject_connector_execution(&record.yaml_content)?;
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
    Query(context): Query<RunWorkflowContext>,
    Json(trigger_data): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let record = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    reject_connector_execution(&record.yaml_content)?;
    let engine = WorkflowEngine::new(state.db.clone());
    let instance_id = engine
        .start_instance_in_context(
            &id,
            trigger_data,
            WorkflowContext {
                project_id: context.project_id,
                conversation_id: None,
            },
        )
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&e),
            xpressclaw_core::error::Error::Workflow(_) => bad_request(&e),
            _ => internal_error(e),
        })?;

    let im = InstanceManager::new(state.db.clone());
    let instance = im.get_instance(&instance_id).map_err(internal_error)?;
    let current_task_id = im
        .list_step_executions(&instance_id)
        .map_err(internal_error)?
        .into_iter()
        .rev()
        .find_map(|execution| execution.task_id);
    let mut response = serde_json::to_value(instance).map_err(internal_error)?;
    if let Some(object) = response.as_object_mut() {
        object.insert("current_task_id".into(), json!(current_task_id));
    }
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Default, Deserialize)]
struct RunWorkflowContext {
    project_id: Option<String>,
}

async fn list_instances(
    State(state): State<AppState>,
    Path(workflow_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let im = InstanceManager::new(state.db.clone());
    let list = im
        .list_instances(&workflow_id, 50)
        .map_err(internal_error)?;
    let values = list
        .into_iter()
        .map(|instance| instance_with_wait_details(&im, instance))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!(values)))
}

fn instance_with_wait_details(
    instances: &InstanceManager,
    instance: xpressclaw_core::workflows::instance::WorkflowInstance,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let mut value = serde_json::to_value(&instance).map_err(internal_error)?;
    if instance.status != "waiting" {
        return Ok(value);
    }
    let wait = instances
        .list_step_executions(&instance.id)
        .map_err(internal_error)?
        .into_iter()
        .rev()
        .find(|execution| execution.status == "waiting")
        .and_then(|execution| execution.input_context)
        .and_then(|state| serde_json::from_str::<WaitState>(&state).ok());
    if let (Some(object), Some(wait)) = (value.as_object_mut(), wait) {
        object.insert("wait_event".into(), json!(wait.event));
        object.insert("wait_resource".into(), json!(wait.resource));
        object.insert("wait_next_poll_at".into(), json!(wait.next_poll_at));
        object.insert("wait_error".into(), json!(wait.last_error));
    }
    Ok(value)
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

fn reject_new_connector_automation(yaml_content: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let definition = WorkflowDefinition::parse(yaml_content).map_err(|e| bad_request(&e))?;
    if definition.uses_connector_automation() {
        return Err(bad_request(
            "connector triggers and notification sinks are disabled in this beta",
        ));
    }
    Ok(())
}

fn reject_connector_execution(yaml_content: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let definition = WorkflowDefinition::parse(yaml_content).map_err(|e| bad_request(&e))?;
    if definition.uses_connector_automation() {
        return Err(bad_request(
            "remove disabled connector triggers and notification sinks before enabling or running this workflow",
        ));
    }
    Ok(())
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
    use xpressclaw_core::db::Database;

    use super::*;

    const TYPED_WORKFLOW: &str = r#"
name: release-report
inputs:
  goal:
    type: string
    required: true
  retries:
    type: number
    default: 2
schedule:
  cron: "0 9 * * 1"
  inputs:
    goal: Weekly release report
flows:
  main:
    steps:
      - id: report
        agent: atlas
        prompt: "Build @goal"
"#;

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "codex")
            .unwrap();
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        Router::new().nest("/workflows", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn create_workflow(app: &Router) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/workflows")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "release-report",
                            "description": "A typed, scheduled workflow",
                            "yaml_content": TYPED_WORKFLOW,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response.into_body()).await
    }

    #[tokio::test]
    async fn creates_a_scheduled_workflow_with_trigger_state() {
        let app = test_app();
        let workflow = create_workflow(&app).await;

        assert_eq!(workflow["enabled"], true);
        assert_eq!(workflow["trigger_count"], 0);
        assert!(workflow["last_triggered_at"].is_null());
        assert!(workflow["trigger_error"].is_null());
    }

    #[tokio::test]
    async fn manual_run_validates_and_persists_typed_inputs() {
        let app = test_app();
        let workflow = create_workflow(&app).await;
        let id = workflow["id"].as_str().unwrap();

        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workflows/{id}/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(missing.into_body()).await["error"]
            .as_str()
            .unwrap()
            .contains("goal"));

        let started = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workflows/{id}/run"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"goal":"Prepare 0.3"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(started.status(), StatusCode::CREATED);
        let instance = body_json(started.into_body()).await;
        assert!(instance["current_task_id"].as_str().is_some());
        let inputs: Value =
            serde_json::from_str(instance["trigger_data"].as_str().unwrap()).unwrap();
        assert_eq!(inputs, json!({"goal": "Prepare 0.3", "retries": 2}));
    }
}
