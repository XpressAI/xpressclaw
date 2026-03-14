use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::tasks::board::TaskBoard;
use xpressclaw_core::tasks::scheduler::{CreateSchedule, ScheduleManager};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListParams {
    pub agent_id: Option<String>,
    pub enabled: Option<bool>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_schedules).post(create_schedule))
        .route("/{id}", get(get_schedule).delete(delete_schedule))
        .route("/{id}/enable", post(enable_schedule))
        .route("/{id}/disable", post(disable_schedule))
        .route("/{id}/trigger", post(trigger_schedule))
}

async fn list_schedules(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let enabled_only = params.enabled.unwrap_or(false);
    let schedules = mgr
        .list(params.agent_id.as_deref(), enabled_only)
        .map_err(internal_error)?;
    Ok(Json(json!(schedules)))
}

async fn create_schedule(
    State(state): State<AppState>,
    Json(req): Json<CreateSchedule>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let schedule = mgr.create(&req).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(schedule))))
}

async fn get_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let schedule = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ScheduleNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(schedule)))
}

async fn delete_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    mgr.delete(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ScheduleNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn enable_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let schedule = mgr.enable(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ScheduleNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(schedule)))
}

async fn disable_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let schedule = mgr.disable(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ScheduleNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(schedule)))
}

async fn trigger_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ScheduleManager::new(state.db.clone());
    let board = TaskBoard::new(state.db.clone());
    let task = mgr.trigger(&id, &board).map_err(|e| match &e {
        xpressclaw_core::error::Error::ScheduleNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
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

        Router::new().nest("/schedules", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn create_test_schedule(app: &Router) -> Value {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/schedules")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "Daily standup",
                            "cron": "0 9 * * *",
                            "agent_id": "atlas",
                            "title": "Standup {date}",
                            "description": "Run daily standup"
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

        create_test_schedule(&app).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/schedules")
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
    async fn test_get_schedule() {
        let app = test_app();

        let created = create_test_schedule(&app).await;
        let id = created["id"].as_str().unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/schedules/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["name"], "Daily standup");
    }

    #[tokio::test]
    async fn test_delete_schedule() {
        let app = test_app();

        let created = create_test_schedule(&app).await;
        let id = created["id"].as_str().unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/schedules/{id}"))
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
                    .uri(format!("/schedules/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_enable_disable() {
        let app = test_app();

        let created = create_test_schedule(&app).await;
        let id = created["id"].as_str().unwrap();

        // Disable
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/schedules/{id}/disable"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["enabled"], false);

        // Enable
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/schedules/{id}/enable"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["enabled"], true);
    }

    #[tokio::test]
    async fn test_trigger() {
        let app = test_app();

        let created = create_test_schedule(&app).await;
        let id = created["id"].as_str().unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/schedules/{id}/trigger"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let task = body_json(resp.into_body()).await;
        assert!(task["title"].as_str().unwrap().starts_with("Standup "));
        assert_eq!(task["agent_id"], "atlas");
    }
}
