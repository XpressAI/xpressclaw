use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use xpressclaw_core::harness::types::{ContainerSpec, VolumeMount};

use crate::state::AppState;

// NB: Agent-app container launching is disabled for the ADR-023 spike.
// Docker was the only launch path and has been removed; WASM-based
// app containers are a separate concern (ADR-017) that needs its own
// c2w-flavored design once this spike lands. Until then, launch /
// logs / proxy endpoints return 503.

/// API routes for app management.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_apps).post(create_app))
        .route("/{id}", get(get_app).delete(delete_app))
        .route("/publish", axum::routing::post(publish_app))
        .route("/{id}/logs", get(get_app_logs))
}

/// Proxy routes mounted at /apps/{name}/ — forwards to app containers.
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
    container_id: Option<String>,
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
            "SELECT id, title, icon, description, agent_id, conversation_id, container_id,
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
                container_id: row.get(6)?,
                port: row.get(7)?,
                source_version: row.get(8)?,
                status: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
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
        "SELECT id, title, icon, description, agent_id, conversation_id, container_id,
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
                container_id: row.get(6)?,
                port: row.get(7)?,
                source_version: row.get(8)?,
                status: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
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
    // Look up container before any .await
    let container_id: Option<String> = {
        let db = state.db.conn();
        db.query_row(
            "SELECT container_id FROM apps WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .ok()
        .flatten()
    };

    // Stop container if running (ADR-023: launch path is disabled so
    // nothing is actually running; this is a no-op during the spike).
    if container_id.is_some() {
        info!(app_id = %id, "app container stop skipped: launch disabled in ADR-023 spike");
    }

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

    Ok(Json(json!({ "deleted": true })))
}

// ---------------------------------------------------------------------------
// Publish: register app + launch container
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PublishAppRequest {
    id: String,
    title: String,
    icon: Option<String>,
    description: Option<String>,
    agent_id: String,
    port: Option<i64>,
    source_dir: Option<String>,
    start_command: Option<String>,
}

async fn publish_app(
    State(state): State<AppState>,
    Json(req): Json<PublishAppRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let app_port = req.port.unwrap_or(3000) as u16;
    let port_str = app_port.to_string();

    // Register/update in database (scope the MutexGuard)
    {
        let icon = req.icon.as_deref().unwrap_or("");
        let desc = req.description.as_deref().unwrap_or("");
        let start_cmd = req.start_command.as_deref().unwrap_or("");
        let db = state.db.conn();
        db.execute(
            "INSERT INTO apps (id, title, icon, description, agent_id, port, status, start_command)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'starting', ?7)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                icon = excluded.icon,
                description = excluded.description,
                start_command = excluded.start_command,
                source_version = source_version + 1,
                status = 'starting',
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

    // If source_dir and start_command provided, launch a container
    if let (Some(source_dir), Some(start_command)) = (&req.source_dir, &req.start_command) {
        let app_id = req.id.clone();
        let _source = source_dir.clone();
        let cmd = start_command.clone();

        // The agent's workspace is a Docker named volume shared between the agent
        // and app containers. The app source at /workspace/apps/{name}/ is accessible
        // from the same volume mounted in the app container.
        let volume_name = workspace_volume_name(&req.agent_id);

        // Detect image from start command keywords (not just prefix,
        // since commands may be wrapped in "cd ... &&" or "sh -c ...")
        let image = if ["node", "npm", "npx"].iter().any(|k| cmd.contains(k)) {
            "node:20-alpine"
        } else if ["python", "pip"].iter().any(|k| cmd.contains(k)) {
            "python:3.11-slim"
        } else {
            "alpine:latest"
        };

        // Persist image for reconciler restarts
        {
            let db = state.db.conn();
            let _ = db.execute("UPDATE apps SET image = ?1 WHERE id = ?2", [image, &req.id]);
        }

        let spec = ContainerSpec {
            image: image.to_string(),
            memory_limit: Some(512 * 1024 * 1024), // 512MB
            cpu_limit: None,
            environment: vec![format!("APP_ID={app_id}"), format!("PORT={app_port}")],
            volumes: vec![VolumeMount {
                // Mount the agent's workspace volume — the app source is at
                // /workspace/apps/{name}/ inside this volume
                source: volume_name,
                target: "/workspace".to_string(),
                read_only: true,
            }],
            network_mode: Some("bridge".to_string()),
            expose_port: Some(app_port),
            cmd: None,         // Set by launch_app_container
            working_dir: None, // Set by launch_app_container
        };

        // Launch synchronously so the caller gets the real result.
        match launch_app_container(&app_id, &spec, &cmd, &state.db).await {
            Ok(container_id) => {
                info!(app_id = %app_id, container_id = &container_id[..12], "app container started");
                return Ok(Json(json!({
                    "id": req.id,
                    "published": true,
                    "status": "running",
                    "container_id": container_id,
                })));
            }
            Err(e) => {
                warn!(app_id = %app_id, error = %e, "failed to launch app container");
                let conn = state.db.conn();
                let _ = conn.execute(
                    "UPDATE apps SET status = 'error', updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [&app_id],
                );
                return Ok(Json(json!({
                    "id": req.id,
                    "published": true,
                    "status": "error",
                    "error": e.to_string(),
                })));
            }
        }
    }

    Ok(Json(json!({ "id": req.id, "published": true })))
}

async fn launch_app_container(
    _app_id: &str,
    _spec: &ContainerSpec,
    _start_command: &str,
    _db: &xpressclaw_core::db::Database,
) -> std::result::Result<String, String> {
    // ADR-023: app-container launching is disabled — Docker was the
    // only launch path and has been removed; a WASM-based flow for
    // published apps needs its own design (out of ADR-023 scope).
    Err("app container launching is disabled per ADR-023 spike".into())
}

async fn get_app_logs(Path(_id): Path<String>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Err((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "app container logs unavailable — app launching disabled per ADR-023 spike",
        })),
    ))
}

/// Get the workspace volume name for an agent.
/// The agent's /workspace is a Docker named volume shared between the agent
/// and its app containers, so apps can read the agent's source code directly.
fn workspace_volume_name(agent_id: &str) -> String {
    format!("xpressclaw-workspace-{agent_id}")
}

// ---------------------------------------------------------------------------
// App proxy: forward HTTP requests to app containers
// ---------------------------------------------------------------------------

async fn proxy_handler(
    State(state): State<AppState>,
    Path(rest): Path<String>,
    _req: axum::extract::Request,
) -> axum::response::Response {
    // Split rest into app_id and path: "myapp/foo/bar" → ("myapp", "foo/bar")
    let (app_id, _path) = rest.split_once('/').unwrap_or((&rest, ""));

    let err_response = |status: StatusCode, msg: &str| {
        axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"error": msg}).to_string()))
            .unwrap()
    };

    // ADR-023: app containers are not launched during the spike. Keep
    // the router mounted so frontend requests don't 404, but every
    // proxy attempt gets a consistent 503. Look up the app first so
    // we return NOT_FOUND vs SERVICE_UNAVAILABLE in the right cases.
    let row = {
        let db = state.db.conn();
        db.query_row("SELECT id FROM apps WHERE id = ?1", [app_id], |row| {
            row.get::<_, String>(0)
        })
    };
    if row.is_err() {
        return err_response(StatusCode::NOT_FOUND, "App not found");
    }
    err_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "App proxy unavailable — app launching disabled per ADR-023 spike",
    )
}
