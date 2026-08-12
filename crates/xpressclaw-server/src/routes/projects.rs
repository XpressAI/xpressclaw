use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::projects::{CreateProject, ProjectManager, UpdateProject};
use xpressclaw_core::tasks::board::TaskBoard;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_projects).post(create_project))
        .route(
            "/{id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
        .route("/{id}/tasks", get(list_project_tasks))
        .route("/{id}/agents/{agent_id}", axum::routing::put(assign_agent))
}

async fn list_project_tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(project_error)?;
    let tasks = TaskBoard::new(state.db.clone())
        .list_for_project(&id, 100)
        .map_err(project_error)?;
    Ok(Json(json!(tasks)))
}

async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let projects = ProjectManager::new(state.db.clone())
        .list()
        .map_err(project_error)?;
    Ok(Json(json!(projects)))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProject>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .create(&request)
        .map_err(project_error)?;
    Ok((StatusCode::CREATED, Json(json!(project))))
}

async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProject>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .update(&id, &request)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    ProjectManager::new(state.db.clone())
        .delete(&id)
        .map_err(project_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Assignment {
    #[serde(default)]
    _acknowledge: bool,
}

async fn assign_agent(
    State(state): State<AppState>,
    Path((id, agent_id)): Path<(String, String)>,
    _request: Option<Json<Assignment>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .assign_agent(&id, &agent_id)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

fn project_error(error: xpressclaw_core::error::Error) -> (StatusCode, Json<Value>) {
    let status = match error {
        xpressclaw_core::error::Error::ProjectNotFound { .. }
        | xpressclaw_core::error::Error::AgentNotFound { .. } => StatusCode::NOT_FOUND,
        xpressclaw_core::error::Error::Project(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
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

    fn app() -> Router {
        let state = AppState::new(
            Arc::new(Config::load_default().unwrap()),
            Arc::new(Database::open_memory().unwrap()),
            None,
            "test.yaml".into(),
            true,
        );
        routes().with_state(state)
    }

    #[tokio::test]
    async fn creates_and_lists_projects() {
        let app = app();
        let response = app
            .clone()
            .oneshot(
                Request::post("/")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Website"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let projects: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(projects.as_array().unwrap().len(), 1);
    }
}
