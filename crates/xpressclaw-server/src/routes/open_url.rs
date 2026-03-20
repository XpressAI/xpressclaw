use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct OpenUrlRequest {
    url: String,
}

pub async fn open_url(
    Json(req): Json<OpenUrlRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Only allow http and https URLs
    if !req.url.starts_with("http://") && !req.url.starts_with("https://") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "only http:// and https:// URLs are allowed" })),
        ));
    }

    open::that(&req.url).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to open URL: {e}") })),
        )
    })?;

    Ok(Json(json!({ "ok": true })))
}
