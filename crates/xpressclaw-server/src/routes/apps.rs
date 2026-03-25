use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_apps).post(create_app))
        .route("/{id}", get(get_app).delete(delete_app))
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
    .map_err(|_| (StatusCode::NOT_FOUND, Json(json!({ "error": "App not found" }))))
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

    Ok(Json(json!({ "deleted": true })))
}
