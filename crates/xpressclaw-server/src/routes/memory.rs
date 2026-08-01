use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::error::Error;
use xpressclaw_core::memory::project::{
    CreateProjectMemoryLink, CreateProjectMemoryNote, ProjectMemoryStore, UpdateProjectMemoryNote,
};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct ListParams {
    state: Option<String>,
    tag: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<usize>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{project_id}/index", get(project_index))
        .route("/{project_id}/search", get(search_notes))
        .route("/{project_id}/notes", get(list_notes).post(create_note))
        .route(
            "/{project_id}/notes/{note_id}",
            get(get_note).patch(update_note),
        )
        .route("/{project_id}/notes/{note_id}/archive", post(archive_note))
        .route("/{project_id}/links", post(create_link))
}

async fn project_index(State(state): State<AppState>, Path(project_id): Path<String>) -> ApiResult {
    let store = ProjectMemoryStore::new(state.db.clone());
    Ok(Json(json!(store
        .briefing(&project_id)
        .map_err(memory_error)?)))
}

async fn list_notes(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult {
    let store = ProjectMemoryStore::new(state.db.clone());
    let notes = store
        .list(
            &project_id,
            params.state.as_deref(),
            params.tag.as_deref(),
            params.limit.unwrap_or(50),
        )
        .map_err(memory_error)?;
    Ok(Json(json!({ "notes": notes })))
}

async fn search_notes(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Query(params): Query<SearchParams>,
) -> ApiResult {
    if params.q.chars().count() > 2_000 {
        return Err(bad_request(
            "memory search must be 2000 characters or fewer",
        ));
    }
    let store = ProjectMemoryStore::new(state.db.clone());
    let results = store
        .search(&project_id, &params.q, params.limit.unwrap_or(10))
        .map_err(memory_error)?;
    Ok(Json(json!({ "results": results })))
}

async fn create_note(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(req): Json<CreateProjectMemoryNote>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let store = ProjectMemoryStore::new(state.db.clone());
    let note = store.create(&project_id, &req).map_err(memory_error)?;
    Ok((StatusCode::CREATED, Json(json!(note))))
}

async fn get_note(
    State(state): State<AppState>,
    Path((project_id, note_id)): Path<(String, String)>,
) -> ApiResult {
    let store = ProjectMemoryStore::new(state.db.clone());
    Ok(Json(json!(store
        .get(&project_id, &note_id)
        .map_err(memory_error)?)))
}

async fn update_note(
    State(state): State<AppState>,
    Path((project_id, note_id)): Path<(String, String)>,
    Json(req): Json<UpdateProjectMemoryNote>,
) -> ApiResult {
    let store = ProjectMemoryStore::new(state.db.clone());
    Ok(Json(json!(store
        .update(&project_id, &note_id, &req)
        .map_err(memory_error)?)))
}

async fn archive_note(
    State(state): State<AppState>,
    Path((project_id, note_id)): Path<(String, String)>,
) -> ApiResult {
    let store = ProjectMemoryStore::new(state.db.clone());
    Ok(Json(json!(store
        .archive(&project_id, &note_id)
        .map_err(memory_error)?)))
}

async fn create_link(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(req): Json<CreateProjectMemoryLink>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let store = ProjectMemoryStore::new(state.db.clone());
    let link = store.link(&project_id, &req).map_err(memory_error)?;
    Ok((StatusCode::CREATED, Json(json!(link))))
}

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn memory_error(error: Error) -> ApiError {
    match error {
        error @ Error::MemoryNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        ),
        error @ Error::Memory(_) => bad_request(error),
        error => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

fn bad_request(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
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

    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;
    use xpressclaw_core::sessions::SessionManager;

    use super::*;

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("alpha", Some("Alpha")).unwrap();
        sessions.ensure("beta", Some("Beta")).unwrap();
        let state = AppState::new(
            Arc::new(Config::load_default().unwrap()),
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        Router::new().nest("/memory", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn create_test_note(app: &Router, project_id: &str, title: &str) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/memory/{project_id}/notes"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "title": title,
                            "body": "本番環境ではｶﾀｶﾅ設定を確認する",
                            "note_type": "procedure",
                            "tags": ["deployment", "運用"]
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
    async fn create_search_and_archive_project_memory() {
        let app = test_app();
        let note = create_test_note(&app, "alpha", "デプロイ手順").await;
        let note_id = note["id"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/memory/alpha/search?q=%E3%82%AB%E3%82%BF%E3%82%AB%E3%83%8A")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let results = body_json(response.into_body()).await;
        assert_eq!(results["results"][0]["note"]["id"], note_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/memory/alpha/notes/{note_id}/archive"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response.into_body()).await["state"], "archived");
    }

    #[tokio::test]
    async fn project_scope_is_enforced_by_http_api() {
        let app = test_app();
        let note = create_test_note(&app, "beta", "Private beta note").await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/memory/alpha/notes/{}",
                        note["id"].as_str().unwrap()
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
