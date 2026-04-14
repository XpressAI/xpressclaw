//! Xpressclaw MCP HTTP server (streamable-HTTP transport).
//!
//! This endpoint is mounted by the pi-agent container via `mcpfs --http`,
//! exposing xpressclaw tasks/memory as files the agent can read and invoke.
//!
//! Protocol: JSON-RPC 2.0 over HTTP POST. Minimal subset:
//!   - initialize
//!   - tools/list
//!   - tools/call
//!
//! Transport: single POST per request (no SSE upgrade for now).
//!
//! This is a first-pass implementation — full MCP capability negotiation,
//! resources, prompts, and notifications can be layered on later.

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

use xpressclaw_core::memory::manager::MemoryManager;
use xpressclaw_core::memory::zettelkasten::CreateMemory;
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/mcp", post(handle))
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

async fn handle(State(state): State<AppState>, Json(req): Json<JsonRpcRequest>) -> impl IntoResponse {
    if req.jsonrpc != "2.0" {
        return Json(rpc_err(req.id.clone(), -32600, "invalid jsonrpc version"));
    }

    debug!(method = %req.method, "mcp request");
    let id = req.id.clone().unwrap_or(Value::Null);
    let result = match req.method.as_str() {
        "initialize" => initialize(),
        "tools/list" => tools_list(),
        "tools/call" => tools_call(&state, req.params).await,
        "notifications/initialized" => Ok(Value::Null),
        other => Err(format!("method not found: {other}")),
    };

    match result {
        Ok(value) => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        }),
        Err(msg) => Json(rpc_err(req.id, -32601, &msg)),
    }
}

fn rpc_err(id: Option<Value>, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.unwrap_or(Value::Null),
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

fn initialize() -> Result<Value, String> {
    Ok(json!({
        "protocolVersion": "2025-03-26",
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "xpressclaw",
            "version": env!("CARGO_PKG_VERSION"),
        },
    }))
}

fn tools_list() -> Result<Value, String> {
    Ok(json!({
        "tools": [
            {
                "name": "list_tasks",
                "description": "List tasks on the xpressclaw task board.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "description": "pending|in_progress|completed" },
                        "agent_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "create_task",
                "description": "Create a new task.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "agent_id": { "type": "string" }
                    },
                    "required": ["title"]
                }
            },
            {
                "name": "update_task",
                "description": "Update a task's status.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string" },
                        "status": { "type": "string" }
                    },
                    "required": ["task_id", "status"]
                }
            },
            {
                "name": "search_memory",
                "description": "Search the zettelkasten memory.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "save_memory",
                "description": "Save a memory note.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "tags": { "type": "string" },
                        "agent_id": { "type": "string" }
                    },
                    "required": ["content"]
                }
            }
        ]
    }))
}

async fn tools_call(state: &AppState, params: Value) -> Result<Value, String> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let text = match name {
        "list_tasks" => {
            let board = TaskBoard::new(state.db.clone());
            let status = args.get("status").and_then(|v| v.as_str());
            let agent_id = args.get("agent_id").and_then(|v| v.as_str());
            match board.list(status, agent_id, 50) {
                Ok(tasks) => {
                    if tasks.is_empty() {
                        "No tasks.".to_string()
                    } else {
                        tasks
                            .iter()
                            .map(|t| format!("- [{}] {} ({})", t.status.as_str(), t.title, t.id))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                }
                Err(e) => return Err(format!("list_tasks: {e}")),
            }
        }
        "create_task" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let board = TaskBoard::new(state.db.clone());
            match board.create(&CreateTask {
                title: title.to_string(),
                description,
                agent_id,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            }) {
                Ok(task) => format!("Created task {} ({})", task.id, task.title),
                Err(e) => return Err(format!("create_task: {e}")),
            }
        }
        "update_task" => {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or("missing task_id")?;
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .ok_or("missing status")?;
            let board = TaskBoard::new(state.db.clone());
            match board.update_status(task_id, status, None) {
                Ok(_) => format!("Updated {task_id} to {status}"),
                Err(e) => return Err(format!("update_task: {e}")),
            }
        }
        "search_memory" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or("missing query")?;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5) as usize;
            let mgr = MemoryManager::new(state.db.clone(), "least-recently-relevant");
            match mgr.search(query, limit) {
                Ok(results) => {
                    if results.is_empty() {
                        "No memories found.".to_string()
                    } else {
                        results
                            .iter()
                            .map(|r| format!("- {}", r.memory.content))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                }
                Err(e) => return Err(format!("search_memory: {e}")),
            }
        }
        "save_memory" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("missing content")?;
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_str())
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default();
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mgr = MemoryManager::new(state.db.clone(), "least-recently-relevant");
            match mgr.add(&CreateMemory {
                content: content.to_string(),
                summary: content.chars().take(100).collect(),
                source: "mcp".to_string(),
                layer: "shared".to_string(),
                agent_id,
                user_id: None,
                tags,
            }) {
                Ok(mem) => format!("Saved memory {}", mem.id),
                Err(e) => return Err(format!("save_memory: {e}")),
            }
        }
        other => {
            warn!(tool = %other, "unknown MCP tool call");
            return Err(format!("unknown tool: {other}"));
        }
    };

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "isError": false
    }))
}
