use axum::Json;
use serde_json::{json, Value};

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "git_hash": option_env!("XPRESSCLAW_GIT_HASH").unwrap_or("dev"),
        "name": "xpressclaw"
    }))
}
