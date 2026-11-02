//! Project-scoped planning capabilities for native manager Agents.
use super::{authorize, bad, require_agent, ApiResult};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use xpressclaw_core::{
    agents::registry::AgentRegistry,
    tasks::{
        board::{BatchTaskInput, TaskBoard},
        planning::{PlanningChange, PlanningFilter, TaskPlanner},
    },
    workflows::manager::{ProjectWorkflowInput, ScheduledWorkflowInput, WorkflowManager},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/{agent_id}/agents", get(list_agents))
        .route("/{agent_id}/tasks/batch", post(create_tasks))
        .route("/{agent_id}/tasks/{task_id}", get(get_task))
        .route("/{agent_id}/tasks/{task_id}/planning", patch(update_task))
        .route(
            "/{agent_id}/workflows",
            get(list_workflows).post(create_workflows),
        )
        .route("/{agent_id}/workflows/schedule", post(schedule_workflows))
        .route(
            "/{agent_id}/workflows/{workflow_id}",
            get(get_workflow).patch(update_workflow),
        )
}

fn require_caller(state: &AppState, agent: &str, headers: &HeaderMap) -> ApiResult<()> {
    authorize(state, agent, headers)?;
    require_agent(headers, agent)
}

#[derive(Deserialize)]
struct Page {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_true")]
    include_templates: bool,
}
fn default_limit() -> i64 {
    100
}
fn default_true() -> bool {
    true
}

async fn list_agents(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    let mut roster = AgentRegistry::new(state.db.clone())
        .project_roster(&agent, page.limit, page.offset)
        .map_err(planning_error)?;
    let config = state.config();
    for entry in roster["agents"].as_array_mut().unwrap() {
        if let Some(config) = config
            .agents
            .iter()
            .find(|config| entry["id"] == config.name)
        {
            entry["title"] = config.context_label().into();
            entry["harness"] = config.runner.kind.clone().into();
        }
    }
    Ok(Json(roster))
}

pub(super) async fn list_tasks(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(filter): Query<PlanningFilter>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    let page = TaskPlanner::new(state.db.clone())
        .list_for_agent(&filter, &agent)
        .map_err(planning_error)?;
    Ok(Json(json!(page)))
}

#[derive(Deserialize)]
pub(super) struct SingleTask {
    #[serde(flatten)]
    task: BatchTaskInput,
    parent_task_id: Option<String>,
}

pub(super) async fn create_task(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SingleTask>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    let tasks = TaskBoard::new(state.db.clone())
        .create_batch_for_agent(&[input.task], input.parent_task_id.as_deref(), &agent)
        .map_err(planning_error)?;
    Ok(Json(json!(tasks[0])))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskBatch {
    tasks: Vec<BatchTaskInput>,
    parent_task_id: Option<String>,
}

async fn create_tasks(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(input): Json<TaskBatch>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    let tasks = TaskBoard::new(state.db.clone())
        .create_batch_for_agent(&input.tasks, input.parent_task_id.as_deref(), &agent)
        .map_err(planning_error)?;
    let refs: serde_json::Map<String, Value> = input
        .tasks
        .iter()
        .zip(&tasks)
        .filter_map(|(input, task)| {
            input
                .ref_name
                .as_ref()
                .map(|name| (name.clone(), json!(task.id)))
        })
        .collect();
    Ok(Json(json!({"tasks":tasks,"refs":refs})))
}

async fn get_task(
    State(state): State<AppState>,
    Path((agent, task)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(
        TaskPlanner::new(state.db.clone())
            .detail_for_agent(&task, &agent)
            .map_err(planning_error)?,
    ))
}

async fn update_task(
    State(state): State<AppState>,
    Path((agent, task)): Path<(String, String)>,
    headers: HeaderMap,
    Json(change): Json<PlanningChange>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    let task = TaskPlanner::new(state.db.clone())
        .change_for_agent(&task, &change, &agent)
        .map_err(planning_error)?;
    Ok(Json(json!(task)))
}

async fn list_workflows(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(
        WorkflowManager::new(state.db.clone())
            .list_for_agent(&agent, page.limit, page.offset, page.include_templates)
            .map_err(planning_error)?,
    ))
}

async fn get_workflow(
    State(state): State<AppState>,
    Path((agent, workflow)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(json!(WorkflowManager::new(state.db.clone())
        .get_for_agent(&workflow, &agent)
        .map_err(planning_error)?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowBatch {
    workflows: Vec<ProjectWorkflowInput>,
}

async fn create_workflows(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(input): Json<WorkflowBatch>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(
        json!({"workflows":WorkflowManager::new(state.db.clone())
        .create_for_agent(&agent, &input.workflows).map_err(planning_error)?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowScheduleBatch {
    schedules: Vec<ScheduledWorkflowInput>,
}

async fn schedule_workflows(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(input): Json<WorkflowScheduleBatch>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(
        json!({"workflows":WorkflowManager::new(state.db.clone())
        .schedule_for_agent(&agent, &input.schedules).map_err(planning_error)?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowUpdate {
    expected_revision: i64,
    yaml_content: Option<String>,
    enabled: Option<bool>,
}

async fn update_workflow(
    State(state): State<AppState>,
    Path((agent, workflow)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<WorkflowUpdate>,
) -> ApiResult<Json<Value>> {
    require_caller(&state, &agent, &headers)?;
    Ok(Json(json!(WorkflowManager::new(state.db.clone())
        .update_for_agent(
            &workflow,
            &agent,
            input.expected_revision,
            input.yaml_content.as_deref(),
            input.enabled
        )
        .map_err(planning_error)?)))
}

fn planning_error(error: xpressclaw_core::error::Error) -> super::ApiError {
    if matches!(error, xpressclaw_core::error::Error::ProjectAccess(_)) {
        (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({"error":error.to_string()})),
        )
    } else {
        bad(error)
    }
}
