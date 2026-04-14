use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use crate::state::AppState;

/// API routes for app management.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_apps).post(create_app))
        .route("/{id}", get(get_app).delete(delete_app))
        .route("/publish", axum::routing::post(publish_app))
        .route("/{id}/logs", get(get_app_logs))
}

/// Proxy routes mounted at /apps/{name}/ — will be served from Wanix in the future.
pub fn proxy_routes() -> Router<AppState> {
    Router::new().route("/{*rest}", get(proxy_handler).post(proxy_handler))
}

#[derive(Debug, Serialize)]
struct App {
    id: String,
    title: String,
    icon: Option<String>,
    description: Option<String>,
    agent_id: String,
    conversation_id: Option<String>,
    port: i64,
    source_version: i64,
    status: String,
    created_at: String,
    updated_at: String,
}

async fn list_apps(State(state): State<AppState>) -> Json<Vec<App>> {
    let db = state.db.conn();
    let mut stmt = db
        .prepare(
            "SELECT id, title, icon, description, agent_id, conversation_id,
                    port, source_version, status, created_at, updated_at
             FROM apps ORDER BY created_at ASC",
        )
        .unwrap();

    let apps = stmt
        .query_map([], |row| {
            Ok(App {
                id: row.get(0)?,
                title: row.get(1)?,
                icon: row.get(2)?,
                description: row.get(3)?,
                agent_id: row.get(4)?,
                conversation_id: row.get(5)?,
                port: row.get(6)?,
                source_version: row.get(7)?,
                status: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    Json(apps)
}

async fn get_app(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<App>, (StatusCode, Json<Value>)> {
    let db = state.db.conn();
    db.query_row(
        "SELECT id, title, icon, description, agent_id, conversation_id,
                port, source_version, status, created_at, updated_at
         FROM apps WHERE id = ?1",
        [&id],
        |row| {
            Ok(App {
                id: row.get(0)?,
                title: row.get(1)?,
                icon: row.get(2)?,
                description: row.get(3)?,
                agent_id: row.get(4)?,
                conversation_id: row.get(5)?,
                port: row.get(6)?,
                source_version: row.get(7)?,
                status: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        },
    )
    .map(Json)
    .map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "App not found" })),
        )
    })
}

#[derive(Debug, Deserialize)]
struct CreateAppRequest {
    id: String,
    title: String,
    icon: Option<String>,
    description: Option<String>,
    agent_id: String,
    port: Option<i64>,
}

async fn create_app(
    State(state): State<AppState>,
    Json(req): Json<CreateAppRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = state.db.conn();
    let port = req.port.unwrap_or(3000).to_string();
    let icon = req.icon.as_deref().unwrap_or("");
    let desc = req.description.as_deref().unwrap_or("");

    db.execute(
        "INSERT INTO apps (id, title, icon, description, agent_id, port)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            icon = excluded.icon,
            description = excluded.description,
            source_version = source_version + 1,
            updated_at = CURRENT_TIMESTAMP",
        [&req.id, &req.title, icon, desc, &req.agent_id, &port],
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "id": req.id, "created": true })))
}

async fn delete_app(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = state.db.conn();
    let affected = db
        .execute("DELETE FROM apps WHERE id = ?1", [&id])
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;

    if affected == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "App not found" })),
        ));
    }

    info!(app_id = %id, "deleted app");
    Ok(Json(json!({ "deleted": true })))
}

// ---------------------------------------------------------------------------
// Publish: register app + mark as running
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PublishAppRequest {
    id: String,
    title: String,
    icon: Option<String>,
    description: Option<String>,
    agent_id: String,
    port: Option<i64>,
    #[allow(dead_code)]
    source_dir: Option<String>,
    start_command: Option<String>,
}

async fn publish_app(
    State(state): State<AppState>,
    Json(req): Json<PublishAppRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let app_port = req.port.unwrap_or(3000) as u16;
    let port_str = app_port.to_string();

    // Register/update in database
    {
        let icon = req.icon.as_deref().unwrap_or("");
        let desc = req.description.as_deref().unwrap_or("");
        let start_cmd = req.start_command.as_deref().unwrap_or("");
        let db = state.db.conn();
        db.execute(
            "INSERT INTO apps (id, title, icon, description, agent_id, port, status, start_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                icon = excluded.icon,
                description = excluded.description,
                start_command = excluded.start_command,
                source_version = source_version + 1,
                status = 'running',
                updated_at = CURRENT_TIMESTAMP",
            [
                &req.id,
                &req.title,
                icon,
                desc,
                &req.agent_id,
                &port_str,
                start_cmd,
            ],
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;
    }

    info!(app_id = %req.id, "published app");
    Ok(Json(json!({
        "id": req.id,
        "published": true,
        "status": "running",
    })))
}

async fn get_app_logs(Path(id): Path<String>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Logs will be served from Wanix in the future; return empty for now
    let _ = id;
    Ok(Json(json!({ "logs": [] })))
}

// ---------------------------------------------------------------------------
// App proxy: will be served from Wanix in the future
// ---------------------------------------------------------------------------

/// Proxy app requests to the Wanix server which serves files from the
/// agent's workspace filesystem.
async fn proxy_handler(
    State(_state): State<AppState>,
    Path(rest): Path<String>,
    _req: axum::extract::Request,
) -> axum::response::Response {
    // Route: /apps/{app_id}/{path} → Wanix server GET /app/{app_id}/{path}
    let wanix_url = format!("http://localhost:9100/app/{rest}");

    let client = reqwest::Client::new();
    match client
        .get(&wanix_url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .to_string();
            let body = resp.bytes().await.unwrap_or_default();

            axum::response::Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(axum::body::Body::from(body))
                .unwrap()
        }
        Err(e) => axum::response::Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"error": format!("Wanix server not available: {e}")}).to_string(),
            ))
            .unwrap(),
    }
}
