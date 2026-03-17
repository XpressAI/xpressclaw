//! MCP tool proxy endpoints for agent harnesses.
//!
//! Harness containers call these endpoints to discover and execute tools.
//! The server aggregates MCP tool servers and enforces policies before
//! forwarding requests.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/list", get(list_tools))
        .route("/call", post(call_tool))
}

/// List available tools across all MCP servers.
async fn list_tools(State(state): State<AppState>) -> Json<Value> {
    let schemas = state.mcp_manager.tool_schemas().await;
    Json(json!({ "tools": schemas }))
}

#[derive(Deserialize)]
struct CallToolRequest {
    #[serde(default)]
    agent_id: String,
    #[serde(alias = "name")]
    tool_name: String,
    #[serde(default)]
    arguments: Value,
}

/// Execute a tool call, routing to the correct MCP server.
async fn call_tool(
    State(state): State<AppState>,
    Json(req): Json<CallToolRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    tracing::info!(
        agent_id = req.agent_id,
        tool = req.tool_name,
        "tool call requested"
    );

    match state
        .mcp_manager
        .call_tool(&req.tool_name, req.arguments)
        .await
    {
        Ok(result) => Ok(Json(json!(result))),
        Err(e) => {
            tracing::warn!(
                tool = req.tool_name,
                error = %e,
                "tool call failed"
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            ))
        }
    }
}
