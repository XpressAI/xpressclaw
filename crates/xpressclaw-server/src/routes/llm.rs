use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use xpressclaw_core::llm::router::ChatCompletionRequest;

use crate::state::AppState;

/// Routes for the built-in LLM router (OpenAI-compatible).
/// Mounted at /v1/ (not /api/).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/chat/completions", post(chat_completions))
        .route("/models", get(list_models))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let router = state.llm_router().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "LLM router not configured" })),
        )
    })?;

    let response = router.chat(&req).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!(response)))
}

async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let router = state.llm_router().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "LLM router not configured" })),
        )
    })?;

    let models = router.models();
    Ok(Json(json!({ "object": "list", "data": models })))
}
