use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use xpressclaw_core::docker::manager::{ContainerSpec, DockerManager, VolumeMount};

use crate::state::AppState;

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

    // Stop container if running (now safe to .await — MutexGuard dropped)
    if let Some(_cid) = &container_id {
        if let Ok(docker) = DockerManager::connect().await {
            let _ = docker.stop(&format!("app-{id}")).await;
            info!(app_id = %id, "stopped app container");
        }
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
        let db = state.db.conn();
        db.execute(
            "INSERT INTO apps (id, title, icon, description, agent_id, port, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'starting')
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                icon = excluded.icon,
                description = excluded.description,
                source_version = source_version + 1,
                status = 'starting',
                updated_at = CURRENT_TIMESTAMP",
            [&req.id, &req.title, icon, desc, &req.agent_id, &port_str],
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
        let source = source_dir.clone();
        let cmd = start_command.clone();
        let db_clone = state.db.clone();

        // The agent's workspace is a Docker named volume shared between the agent
        // and app containers. The app source at /workspace/apps/{name}/ is accessible
        // from the same volume mounted in the app container.
        let volume_name = workspace_volume_name(&req.agent_id);

        // Detect image from start command
        let image = if cmd.starts_with("node") || cmd.starts_with("npm") || cmd.starts_with("npx") {
            "node:20-alpine"
        } else if cmd.starts_with("python") || cmd.starts_with("pip") {
            "python:3.11-slim"
        } else {
            "alpine:latest"
        };

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

        // Launch in background
        tokio::spawn(async move {
            match launch_app_container(&app_id, &spec, &cmd, &db_clone).await {
                Ok(container_id) => {
                    info!(app_id = %app_id, container_id = &container_id[..12], "app container started");
                }
                Err(e) => {
                    warn!(app_id = %app_id, error = %e, "failed to launch app container");
                    let conn = db_clone.conn();
                    let _ = conn.execute(
                        "UPDATE apps SET status = 'error', updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                        [&app_id],
                    );
                }
            }
        });
    }

    Ok(Json(json!({ "id": req.id, "published": true })))
}

async fn launch_app_container(
    app_id: &str,
    spec: &ContainerSpec,
    start_command: &str,
    db: &xpressclaw_core::db::Database,
) -> std::result::Result<String, String> {
    let docker = DockerManager::connect()
        .await
        .map_err(|e| format!("docker connect: {e}"))?;

    // Stop existing container if any
    let _ = docker.stop(&format!("app-{app_id}")).await;

    let mut launch_spec = spec.clone();
    // Set the command and working directory for the app
    // The app source is at /workspace/apps/{app_id}/ in the shared volume
    launch_spec.cmd = Some(vec![
        "sh".to_string(),
        "-c".to_string(),
        start_command.to_string(),
    ]);
    launch_spec.working_dir = Some(format!("/workspace/apps/{app_id}"));

    let info = docker
        .launch(&format!("app-{app_id}"), &launch_spec)
        .await
        .map_err(|e| format!("launch: {e}"))?;

    // Update DB with container info
    let conn = db.conn();
    let _host_port = info.host_port;
    let _ = conn.execute(
        "UPDATE apps SET container_id = ?1, status = 'running', updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        [&info.container_id, app_id],
    );

    Ok(info.container_id)
}

async fn get_app_logs(Path(id): Path<String>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let docker = DockerManager::connect().await.map_err(|e| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    let logs = docker.logs(&format!("app-{id}"), 100).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(json!({ "logs": logs })))
}

/// Get the workspace volume name for an agent.
/// The agent's /workspace is a Docker named volume shared between the agent
/// and its app containers, so apps can read the agent's source code directly.
fn workspace_volume_name(agent_id: &str) -> String {
    format!("xpressclaw-workspace-{agent_id}")
}

/// Resolve a container path to the host path by checking agent volume mounts.
async fn resolve_host_path(state: &AppState, agent_id: &str, container_path: &str) -> String {
    // The agent's workspace is mounted from the host. Look at the agent config
    // to find the volume mapping that contains this path.
    let config = state.config();
    if let Some(agent_cfg) = config.agents.iter().find(|a| a.name == agent_id) {
        for vol in &agent_cfg.volumes {
            // Volumes are in format "host_path:container_path" or just "host_path"
            if let Some((host, container)) = vol.split_once(':') {
                if container_path.starts_with(container) {
                    let relative = container_path.strip_prefix(container).unwrap_or("");
                    return format!("{host}{relative}");
                }
            }
        }
    }
    // Fallback: assume path is already a host path
    container_path.to_string()
}

// ---------------------------------------------------------------------------
// App proxy: forward HTTP requests to app containers
// ---------------------------------------------------------------------------

async fn proxy_handler(
    State(state): State<AppState>,
    Path(rest): Path<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    // Split rest into app_id and path: "myapp/foo/bar" → ("myapp", "foo/bar")
    let (app_id, path) = rest.split_once('/').unwrap_or((&rest, ""));

    let method = req.method().clone();
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let err_response = |status: StatusCode, msg: &str| {
        axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"error": msg}).to_string()))
            .unwrap()
    };

    let row = {
        let db = state.db.conn();
        db.query_row(
            "SELECT container_id, port, status FROM apps WHERE id = ?1",
            [app_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
    };

    let (container_id, _port, status) = match row {
        Ok(r) => r,
        Err(_) => return err_response(StatusCode::NOT_FOUND, "App not found"),
    };

    if status != "running" {
        return err_response(StatusCode::SERVICE_UNAVAILABLE, &format!("App is {status}"));
    }

    let container_id = match container_id {
        Some(cid) => cid,
        None => return err_response(StatusCode::SERVICE_UNAVAILABLE, "App has no container"),
    };

    let docker = match DockerManager::connect().await {
        Ok(d) => d,
        Err(e) => return err_response(StatusCode::SERVICE_UNAVAILABLE, &format!("Docker: {e}")),
    };

    let host_port = match docker.inspect(&container_id).await {
        Ok(Some(p)) => p,
        _ => {
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Container port not available",
            )
        }
    };

    let target_url = format!("http://127.0.0.1:{host_port}/{path}");

    let client = reqwest::Client::new();
    let mut proxy_req = client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &target_url,
    );

    for (key, val) in headers.iter() {
        if !matches!(key.as_str(), "host" | "connection" | "transfer-encoding") {
            if let Ok(v) = reqwest::header::HeaderValue::from_bytes(val.as_bytes()) {
                proxy_req = proxy_req.header(key.as_str(), v);
            }
        }
    }

    if !body.is_empty() {
        proxy_req = proxy_req.body(body.to_vec());
    }

    let resp = match proxy_req.send().await {
        Ok(r) => r,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, &format!("proxy: {e}")),
    };

    let resp_status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let resp_headers = resp.headers().clone();
    let resp_body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, &format!("body: {e}")),
    };

    let mut response = axum::response::Response::builder().status(resp_status);
    for (key, val) in resp_headers.iter() {
        if !matches!(key.as_str(), "transfer-encoding" | "connection") {
            response = response.header(key, val);
        }
    }
    response
        .body(axum::body::Body::from(resp_body))
        .unwrap_or_else(|_| {
            err_response(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        })
}
