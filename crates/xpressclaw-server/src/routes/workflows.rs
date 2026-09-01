use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::sessions::{SessionManager, WorkAttempt};
use xpressclaw_core::tasks::conversation::TaskConversation;
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
        .route("/{id}/default", post(set_default_workflow))
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

#[derive(Deserialize)]
struct DefaultWorkflowReq {
    default_for_tasks: bool,
}

async fn set_default_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DefaultWorkflowReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = WorkflowManager::new(state.db.clone());
    let workflow = mgr
        .set_default_for_tasks(&id, req.default_for_tasks)
        .map_err(|error| match &error {
            xpressclaw_core::error::Error::WorkflowNotFound { .. } => not_found(&error),
            xpressclaw_core::error::Error::Workflow(_) => bad_request(&error),
            _ => internal_error(error),
        })?;
    Ok(Json(json!(workflow)))
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
    let cancellation = WorkflowEngine::new(state.db.clone())
        .cancel_instance(&instance_id, "Workflow cancelled by user")
        .map_err(|error| match &error {
            xpressclaw_core::error::Error::WorkflowInstanceNotFound { .. } => not_found(&error),
            _ => internal_error(error),
        })?;
    if let Some(task_id) = cancellation.continuation_task_id.as_deref() {
        cleanup_cancelled_continuation(&state, task_id, &cancellation.cancelled_attempts).await?;
    }
    let instance = im.get_instance(&instance_id).map_err(internal_error)?;
    Ok(Json(json!(instance)))
}

async fn cleanup_cancelled_continuation(
    state: &AppState,
    task_id: &str,
    cancelled: &[WorkAttempt],
) -> Result<(), (StatusCode, Json<Value>)> {
    let sessions = SessionManager::new(state.db.clone());
    let mut sessions_to_stop = std::collections::BTreeMap::<String, bool>::new();
    for attempt in cancelled {
        state.elicitations.cancel_attempt(&attempt.id);
        sessions_to_stop
            .entry(attempt.session_id.clone())
            .and_modify(|has_container| *has_container |= attempt.container_id.is_some())
            .or_insert(attempt.container_id.is_some());
    }

    let docker = if sessions_to_stop
        .values()
        .any(|has_container| *has_container)
    {
        state.docker().await
    } else {
        None
    };
    let mut stopped_sessions = std::collections::BTreeSet::new();
    for (session_id, has_container) in sessions_to_stop {
        let stopped = if !has_container {
            true
        } else if let Some(docker) = docker.as_ref() {
            docker.stop_preserving(&session_id).await.is_ok()
        } else {
            false
        };
        if stopped {
            stopped_sessions.insert(session_id);
        }
    }
    for attempt in cancelled {
        if stopped_sessions.contains(&attempt.session_id) {
            let _ = sessions.clear_container(&attempt.id);
        }
    }
    state
        .db
        .with_conn(|conn| {
            for queue_id in cancelled.iter().filter_map(|attempt| attempt.queue_id) {
                conn.execute(
                    "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                        harness_response = 'cancelled by user'
                     WHERE id = ?1 AND status = 'running'",
                    [queue_id],
                )?;
            }
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .map_err(internal_error)?;

    let output = TaskConversation::new(state.db.clone())
        .get_messages(task_id)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.content)
        .unwrap_or_default();
    let source_status = state
        .db
        .with_conn(|conn| {
            conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(xpressclaw_core::error::Error::from)
        })
        .map_err(internal_error)?;
    let completion_status = if source_status == "cancelled" {
        "cancelled"
    } else {
        "completed"
    };
    if let Err(error) =
        WorkflowEngine::new(state.db.clone()).on_task_completed(task_id, completion_status, &output)
    {
        tracing::warn!(
            task_id,
            error = %error,
            "failed to release workflows waiting behind cancelled continuation"
        );
    }
    Ok(())
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
    use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
    use xpressclaw_core::tasks::queue::TaskQueue;

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

    fn test_app_with_db() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "codex")
            .unwrap();
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db.clone(),
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        (
            Router::new().nest("/workflows", routes()).with_state(state),
            db,
        )
    }

    fn test_app() -> Router {
        test_app_with_db().0
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
    async fn marks_eligible_workflows_as_default_task_policy() {
        let app = test_app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/workflows")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "final-ui-check",
                            "yaml_content": r#"
name: final-ui-check
flows:
  main:
    steps:
      - id: review_ui
        type: continue
        prompt: Ensure the UI contains no unnecessary messages.
"#,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let workflow = body_json(response.into_body()).await;
        assert_eq!(workflow["default_for_tasks"], false);
        let id = workflow["id"].as_str().unwrap();

        let marked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workflows/{id}/default"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"default_for_tasks":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(marked.status(), StatusCode::OK);
        assert_eq!(
            body_json(marked.into_body()).await["default_for_tasks"],
            true
        );

        let typed = create_workflow(&app).await;
        let typed_id = typed["id"].as_str().unwrap();
        let rejected = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workflows/{typed_id}/default"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"default_for_tasks":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(rejected.into_body()).await["error"]
            .as_str()
            .unwrap()
            .contains("cannot collect run-time inputs"));
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

    #[tokio::test]
    async fn cancelling_run_preserves_a_newer_user_response() {
        let (app, db) = test_app_with_db();
        let workflow = WorkflowManager::new(db.clone())
            .create(&CreateWorkflow {
                name: "final-ui-check".into(),
                description: None,
                yaml_content: r#"
name: final-ui-check
flows:
  main:
    steps:
      - id: review_ui
        type: continue
        prompt: Ensure the UI contains no unnecessary messages.
"#
                .into(),
            })
            .unwrap();
        WorkflowManager::new(db.clone())
            .set_default_for_tasks(&workflow.id, true)
            .unwrap();
        let other_workflow = WorkflowManager::new(db.clone())
            .create(&CreateWorkflow {
                name: "second-ui-check".into(),
                description: None,
                yaml_content: r#"
name: second-ui-check
flows:
  main:
    steps:
      - id: second_review
        type: continue
        prompt: Run the second UI check.
"#
                .into(),
            })
            .unwrap();
        WorkflowManager::new(db.clone())
            .set_default_for_tasks(&other_workflow.id, true)
            .unwrap();

        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Fix the interface".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        TaskQueue::new(db.clone())
            .enqueue(&task.id, "atlas")
            .unwrap();
        let engine = WorkflowEngine::new(db.clone());
        let instance_ids = engine.attach_default_workflows_to_task(&task.id).unwrap();
        let instances = InstanceManager::new(db.clone());

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_queue SET status = 'completed', completed_at = CURRENT_TIMESTAMP
                 WHERE task_id = ?1",
                [&task.id],
            )?;
            conn.execute(
                "UPDATE work_attempts SET status = 'completed', completed_at = CURRENT_TIMESTAMP
                 WHERE task_id = ?1",
                [&task.id],
            )?;
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .unwrap();
        TaskBoard::new(db.clone())
            .update_status(&task.id, "completed", Some("atlas"))
            .unwrap();
        engine
            .on_task_completed(&task.id, "completed", "Initial response")
            .unwrap();

        let instance_id = instance_ids
            .iter()
            .find(|instance_id| {
                instances
                    .list_step_executions(instance_id)
                    .is_ok_and(|executions| {
                        executions
                            .iter()
                            .any(|execution| execution.continuation_attempt_id.is_some())
                    })
            })
            .unwrap()
            .clone();
        let other_instance_id = instance_ids
            .iter()
            .find(|candidate_id| candidate_id.as_str() != instance_id.as_str())
            .unwrap()
            .clone();

        let continuation = instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.continuation_attempt_id.is_some())
            .unwrap();
        let workflow_attempt_id = continuation.continuation_attempt_id.unwrap();
        let conversation = TaskConversation::new(db.clone());
        let user_message = conversation
            .add_message(
                &task.id,
                "user",
                "Please handle this after the workflow prompt.",
            )
            .unwrap();
        let user_queue = TaskQueue::new(db.clone())
            .enqueue_continuation_for_message(
                &task.id,
                "atlas",
                user_message.id,
                &user_message.timestamp,
            )
            .unwrap()
            .expect("a user response must not coalesce into workflow-owned work");
        let user_attempt_id = user_queue.attempt_id.clone().unwrap();
        assert_ne!(workflow_attempt_id, user_attempt_id);

        let active_before: (String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT queue.status, attempt.status
                     FROM task_queue queue
                     JOIN work_attempts attempt ON attempt.queue_id = queue.id
                     WHERE attempt.id = ?1",
                    [&workflow_attempt_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(xpressclaw_core::error::Error::from)
            })
            .unwrap();
        assert_eq!(active_before, ("queued".into(), "queued".into()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workflows/instances/{instance_id}/cancel"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response.into_body()).await["status"], "cancelled");

        let terminal: (String, Option<String>, String, String, String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT task.status, task.active_attempt_id,
                            workflow_queue.status, workflow_attempt.status,
                            user_queue.status, user_attempt.status
                     FROM tasks task
                     JOIN work_attempts workflow_attempt ON workflow_attempt.id = ?2
                     JOIN task_queue workflow_queue ON workflow_queue.id = workflow_attempt.queue_id
                     JOIN work_attempts user_attempt ON user_attempt.id = ?3
                     JOIN task_queue user_queue ON user_queue.id = user_attempt.queue_id
                     WHERE task.id = ?1",
                    (&task.id, &workflow_attempt_id, &user_attempt_id),
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .map_err(xpressclaw_core::error::Error::from)
            })
            .unwrap();
        assert_eq!(
            terminal,
            (
                "pending".into(),
                Some(user_attempt_id),
                "failed".into(),
                "cancelled".into(),
                "queued".into(),
                "queued".into(),
            )
        );
        assert_eq!(
            instances.get_instance(&other_instance_id).unwrap().status,
            "running"
        );
        let other_executions = instances.list_step_executions(&other_instance_id).unwrap();
        assert_eq!(other_executions.len(), 1);
        assert_eq!(other_executions[0].step_id, "__source_task__");
        assert_eq!(other_executions[0].status, "running");
        assert_eq!(
            TaskQueue::new(db).claim_next().unwrap().unwrap().id,
            user_queue.id
        );
    }
}
