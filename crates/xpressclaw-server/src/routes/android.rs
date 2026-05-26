//! `/v1/android/*` — host-side device control over adb_client. The shared
//! foundation for BOTH the agent (via the MCP shim) and the human collaborative
//! live view: an agent tool call and a human's click hit the same endpoints,
//! driving the same device. Feature-gated behind `android`. See ADR-024.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::android::AndroidDevice;
use xpressclaw_core::connectors::manager::ConnectorManager;
use xpressclaw_core::db::Database;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", get(status))
        .route("/screenshot", get(screenshot))
        .route("/dump", get(dump))
        .route("/tap", post(tap))
        .route("/tap-text", post(tap_text))
        .route("/swipe", post(swipe))
        .route("/input-text", post(input_text))
        .route("/key", post(key))
}

/// Which adb endpoint to drive.
enum DeviceTarget {
    Server(String),
    Tcp(SocketAddr),
}

impl DeviceTarget {
    fn connect(self) -> xpressclaw_core::error::Result<AndroidDevice> {
        match self {
            DeviceTarget::Server(s) => AndroidDevice::via_server(&s),
            DeviceTarget::Tcp(a) => AndroidDevice::via_tcp(a),
        }
    }
}

/// Resolve the device from the configured `android` connector, else default to
/// the standard emulator serial.
fn resolve_target(db: Arc<Database>) -> DeviceTarget {
    let mgr = ConnectorManager::new(db);
    if let Ok(list) = mgr.list() {
        if let Some(rec) = list
            .iter()
            .find(|c| c.connector_type == "android" && c.enabled)
        {
            if let Some(tcp) = rec
                .config
                .get("tcp")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Ok(addr) = tcp.parse::<SocketAddr>() {
                    return DeviceTarget::Tcp(addr);
                }
            }
            if let Some(serial) = rec
                .config
                .get("serial")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return DeviceTarget::Server(serial.to_string());
            }
        }
    }
    DeviceTarget::Server("emulator-5554".to_string())
}

/// Connect (blocking adb_client) and run `f` against the device, off-thread.
async fn with_device<T, F>(db: Arc<Database>, f: F) -> Result<T, String>
where
    F: FnOnce(&mut AndroidDevice) -> xpressclaw_core::error::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let target = resolve_target(db);
    match tokio::task::spawn_blocking(move || {
        let mut device = target.connect()?;
        f(&mut device)
    })
    .await
    {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(format!("device task panicked: {e}")),
    }
}

fn err(e: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
}

async fn status(State(state): State<AppState>) -> Json<Value> {
    let reachable = with_device(state.db.clone(), |d| d.shell("echo ok").map(|_| ()))
        .await
        .is_ok();
    Json(json!({ "reachable": reachable }))
}

async fn screenshot(State(state): State<AppState>) -> Result<Response, (StatusCode, Json<Value>)> {
    let png = with_device(state.db.clone(), |d| d.screenshot_bytes())
        .await
        .map_err(err)?;
    Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response())
}

async fn dump(State(state): State<AppState>) -> Result<String, (StatusCode, Json<Value>)> {
    with_device(state.db.clone(), |d| d.ui_dump())
        .await
        .map_err(err)
}

#[derive(Deserialize)]
struct TapReq {
    x: i32,
    y: i32,
}

async fn tap(
    State(state): State<AppState>,
    Json(req): Json<TapReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    with_device(state.db.clone(), move |d| d.tap(req.x, req.y))
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TapTextReq {
    label: String,
}

async fn tap_text(
    State(state): State<AppState>,
    Json(req): Json<TapTextReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (x, y) = with_device(state.db.clone(), move |d| d.tap_text(&req.label))
        .await
        .map_err(err)?;
    Ok(Json(json!({ "x": x, "y": y })))
}

fn default_swipe_ms() -> u32 {
    300
}

#[derive(Deserialize)]
struct SwipeReq {
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    #[serde(default = "default_swipe_ms")]
    ms: u32,
}

async fn swipe(
    State(state): State<AppState>,
    Json(req): Json<SwipeReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    with_device(state.db.clone(), move |d| {
        d.swipe(req.x1, req.y1, req.x2, req.y2, req.ms)
    })
    .await
    .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TextReq {
    text: String,
}

async fn input_text(
    State(state): State<AppState>,
    Json(req): Json<TextReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    with_device(state.db.clone(), move |d| d.input_text(&req.text))
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct KeyReq {
    key: String,
}

async fn key(
    State(state): State<AppState>,
    Json(req): Json<KeyReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    with_device(state.db.clone(), move |d| d.key_event(&req.key))
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}
