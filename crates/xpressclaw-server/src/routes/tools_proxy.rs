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

use xpressclaw_core::tools::registry::ToolRegistry;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/list", get(list_tools))
        .route("/call", post(call_tool))
}

/// List available tools for an agent.
/// Called by harnesses to discover what MCP tools they can use.
async fn list_tools(State(state): State<AppState>) -> Json<Value> {
    let registry = ToolRegistry::new(state.db.clone());

    // Return all tools — the harness passes its agent_id and we filter by permissions.
    // For now, return all registered tool schemas.
    let schemas = registry.get_tool_schemas("*");

    Json(json!({
        "tools": schemas
    }))
}

#[derive(Deserialize)]
struct CallToolRequest {
    agent_id: String,
    tool_name: String,
    arguments: Value,
}

/// Execute a tool call on behalf of an agent.
/// The server checks policies and routes to the appropriate MCP server.
async fn call_tool(
    State(state): State<AppState>,
    Json(req): Json<CallToolRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = ToolRegistry::new(state.db.clone());

    // Check if the tool exists and is allowed for this agent
    if !registry.is_tool_allowed(&req.agent_id, &req.tool_name) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": format!("tool '{}' is not allowed for agent '{}'", req.tool_name, req.agent_id)
            })),
        ));
    }

    // TODO: Route to actual MCP server via McpProxy
    // For now, return a stub response indicating the tool was called
    // but MCP server execution is not yet wired up.
    tracing::info!(
        agent_id = req.agent_id,
        tool = req.tool_name,
        "tool call requested (MCP execution pending)"
    );

    Ok(Json(json!({
        "content": [{
            "type": "text",
            "text": format!("Tool '{}' called with arguments: {}. (MCP execution not yet connected)", req.tool_name, req.arguments)
        }],
        "isError": false
    })))
}
