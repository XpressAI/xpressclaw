use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::memory::manager::MemoryManager;
use xpressclaw_core::memory::zettelkasten::CreateMemory;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct SearchParams {
    pub q: String,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct ListParams {
    pub tag: Option<String>,
    pub layer: Option<String>,
    pub agent_id: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateBody {
    pub content: Option<String>,
    pub summary: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_memories).post(create_memory))
        .route("/search", get(search_memories))
        .route("/stats", get(memory_stats))
        .route(
            "/{id}",
            get(get_memory).put(update_memory).delete(delete_memory),
        )
        .route("/{id}/related", get(find_related))
        .route("/slots/{agent_id}", get(get_slots))
}

async fn list_memories(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let limit = params.limit.unwrap_or(50);

    let results = if let Some(tag) = params.tag {
        mgr.search_by_tag(&tag, limit).map_err(internal_error)?
    } else {
        mgr.get_recent(params.layer.as_deref(), params.agent_id.as_deref(), limit)
            .map_err(internal_error)?
    };

    Ok(Json(json!(results)))
}

async fn create_memory(
    State(state): State<AppState>,
    Json(req): Json<CreateMemory>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let memory = mgr.add(&req).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(memory))))
}

async fn get_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let memory = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::MemoryNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(memory)))
}

async fn update_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let memory = mgr
        .update(&id, body.content.as_deref(), body.summary.as_deref())
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::MemoryNotFound { .. } => not_found(&e),
            _ => internal_error(e),
        })?;
    Ok(Json(json!(memory)))
}

async fn delete_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    mgr.delete(&id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn search_memories(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let limit = params.limit.unwrap_or(10);
    let results = mgr.search(&params.q, limit).map_err(internal_error)?;
    Ok(Json(json!(results)))
}

async fn find_related(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let results = mgr.find_related(&id, 10).map_err(|e| match &e {
        xpressclaw_core::error::Error::MemoryNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(results)))
}

async fn get_slots(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let slots = mgr.get_slots(&agent_id).map_err(internal_error)?;
    Ok(Json(json!(slots)))
}

async fn memory_stats(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = memory_manager(&state);
    let stats = mgr.get_stats().map_err(internal_error)?;
    Ok(Json(json!(stats)))
}

fn memory_manager(state: &AppState) -> MemoryManager {
    MemoryManager::new(state.db.clone(), &state.config.memory.eviction)
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

        Router::new().nest("/memory", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_create_and_get_memory() {
        let app = test_app();

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/memory")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "content": "Rust is a systems programming language",
                            "summary": "Rust overview",
                            "source": "user",
                            "tags": ["rust", "programming"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["summary"], "Rust overview");
        let memory_id = body["id"].as_str().unwrap().to_string();

        // Get
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/memory/{memory_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["id"], memory_id);
    }

    #[tokio::test]
    async fn test_search_memories() {
        let app = test_app();

        // Create a memory
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/memory")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "content": "Quantum computing uses qubits",
                            "summary": "Quantum computing basics",
                            "source": "user"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Search
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/memory/search?q=quantum&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(!body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_delete_memory() {
        let app = test_app();

        // Create
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/memory")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "content": "Deletable",
                            "summary": "To delete",
                            "source": "test"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let memory_id = body["id"].as_str().unwrap().to_string();

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/memory/{memory_id}"))
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
                    .uri(format!("/memory/{memory_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_memory_slots() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/memory/slots/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 8); // 8 slots
    }

    #[tokio::test]
    async fn test_memory_stats() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/memory/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["zettelkasten"]["total_memories"].is_number());
        assert!(body["vector"]["embedding_count"].is_number());
    }
}
