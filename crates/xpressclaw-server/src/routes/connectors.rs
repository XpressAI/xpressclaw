use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::connectors::manager::{ConnectorManager, CreateChannel, CreateConnector};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_connectors).post(create_connector))
        .route(
            "/{id}",
            get(get_connector)
                .patch(update_connector)
                .delete(delete_connector),
        )
        .route("/{id}/test", post(test_connector))
        .route("/{id}/channels", get(list_channels).post(create_channel))
        .route(
            "/{connector_id}/channels/{channel_id}",
            axum::routing::patch(update_channel).delete(delete_channel),
        )
}

/// Webhook receiver — separate from the main connector CRUD routes.
pub fn webhook_routes() -> Router<AppState> {
    Router::new().route("/webhook/{channel_id}", post(receive_webhook))
}

// -- Handlers --

async fn list_connectors(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let list = mgr.list().map_err(internal_error)?;
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct CreateConnectorReq {
    name: String,
    connector_type: String,
    #[serde(default)]
    config: Value,
}

async fn create_connector(
    State(state): State<AppState>,
    Json(req): Json<CreateConnectorReq>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let c = mgr
        .create(&CreateConnector {
            name: req.name,
            connector_type: req.connector_type,
            config: req.config,
        })
        .map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(c))))
}

async fn get_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let c = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConnectorNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(c)))
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct UpdateConnectorReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    enabled: Option<bool>,
}

async fn update_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateConnectorReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    // Get current values for fields not provided
    let current = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConnectorNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    let config = req.config.unwrap_or(current.config);
    let enabled = req.enabled.unwrap_or(current.enabled);
    let c = mgr.update(&id, config, enabled).map_err(internal_error)?;
    Ok(Json(json!(c)))
}

async fn delete_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    mgr.delete(&id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_connector(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let c = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConnectorNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    // Basic validation: check config is well-formed for the type
    let ok = match c.connector_type.as_str() {
        "telegram" => c.config.get("bot_token").and_then(|v| v.as_str()).is_some(),
        "webhook" => true, // Webhooks always work
        "file_watcher" => c.config.get("paths").and_then(|v| v.as_array()).is_some(),
        _ => true,
    };

    if ok {
        mgr.set_status(&id, "connected", None)
            .map_err(internal_error)?;
        Ok(Json(json!({ "ok": true })))
    } else {
        let err = "Invalid configuration for this connector type";
        mgr.set_status(&id, "error", Some(err))
            .map_err(internal_error)?;
        Ok(Json(json!({ "ok": false, "error": err })))
    }
}

async fn list_channels(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let list = mgr.list_channels(&connector_id).map_err(internal_error)?;
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct CreateChannelReq {
    name: String,
    #[serde(default = "default_channel_type")]
    channel_type: String,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    agent_id: Option<String>,
}

fn default_channel_type() -> String {
    "both".to_string()
}

async fn create_channel(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
    Json(req): Json<CreateChannelReq>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let ch = mgr
        .create_channel(
            &connector_id,
            &CreateChannel {
                name: req.name,
                channel_type: req.channel_type,
                config: req.config,
                agent_id: req.agent_id,
            },
        )
        .map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(ch))))
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct UpdateChannelReq {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    config: Option<Value>,
}

async fn update_channel(
    State(state): State<AppState>,
    Path((_, channel_id)): Path<(String, String)>,
    Json(req): Json<UpdateChannelReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    let ch = mgr
        .update_channel(&channel_id, req.agent_id.as_deref())
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::ChannelNotFound { .. } => not_found(&e),
            _ => internal_error(e),
        })?;
    Ok(Json(json!(ch)))
}

async fn delete_channel(
    State(state): State<AppState>,
    Path((_, channel_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());
    mgr.delete_channel(&channel_id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Receive an incoming webhook and store it as a connector event.
async fn receive_webhook(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConnectorManager::new(state.db.clone());

    // Look up channel to find connector_id
    let channel = mgr.get_channel(&channel_id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ChannelNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    // Store as connector event
    mgr.record_event(&channel.connector_id, &channel_id, "webhook", &payload)
        .map_err(internal_error)?;

    Ok(Json(json!({ "received": true })))
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
