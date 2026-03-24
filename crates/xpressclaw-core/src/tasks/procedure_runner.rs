//! Ready-based procedure execution.
//!
//! Bridges xpressclaw's MCP tool system with Ready's deterministic plan
//! interpreter. SOPs are translated to plans (via LLM) once, then executed
//! step-by-step without further LLM involvement.
//!
//! The MCP tools available to the agent (tasks, filesystem, shell, etc.)
//! are exposed to Ready as a `ToolsModule` so plans can call them.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, error, info};

use ready::execution::state::{ExecutionState, ExecutionStatus};
use ready::plan::AbstractPlan;
use ready::tools::registry::InMemoryToolRegistry;
use ready::tools::{
    ToolArgumentDescription, ToolDescription, ToolResult, ToolReturnDescription, ToolsModule,
};
use ready::workflow::executor::SopExecutor;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// MCP → Ready tool bridge
// ---------------------------------------------------------------------------

/// Bridges xpressclaw's MCP tools to Ready's ToolsModule trait.
///
/// Calls the xpressclaw server's `/v1/tools/call` endpoint to execute tools,
/// making all MCP tools (tasks, filesystem, shell, websearch, etc.) available
/// to Ready plans.
pub struct McpToolsBridge {
    descriptions: Vec<ToolDescription>,
    base_url: String,
    agent_id: String,
}

impl McpToolsBridge {
    /// Create a bridge that calls xpressclaw's tool proxy.
    ///
    /// `base_url` is the server URL (e.g., "http://127.0.0.1:8935").
    /// `tool_schemas` are the OpenAI-format tool schemas from `/v1/tools/list`.
    pub fn new(base_url: &str, agent_id: &str, tool_schemas: &[Value]) -> Self {
        let descriptions = tool_schemas
            .iter()
            .filter_map(convert_tool_schema)
            .collect();

        Self {
            descriptions,
            base_url: base_url.trim_end_matches('/').to_string(),
            agent_id: agent_id.to_string(),
        }
    }
}

#[async_trait]
impl ToolsModule for McpToolsBridge {
    fn tools(&self) -> &[ToolDescription] {
        &self.descriptions
    }

    async fn execute(&self, call: &ready::tools::models::ToolCall) -> ready::Result<ToolResult> {
        debug!(tool = call.tool_id, "executing MCP tool via bridge");

        // Convert positional args to a JSON object using the tool's parameter names
        let tool_desc = self
            .descriptions
            .iter()
            .find(|d| d.id == call.tool_id)
            .ok_or_else(|| ready::ReadyError::ToolNotFound(call.tool_id.clone()))?;

        let mut arguments = serde_json::Map::new();
        for (i, arg_desc) in tool_desc.arguments.iter().enumerate() {
            if let Some(val) = call.args.get(i) {
                arguments.insert(arg_desc.name.clone(), val.clone());
            }
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/v1/tools/call", self.base_url))
            .json(&serde_json::json!({
                "agent_id": self.agent_id,
                "tool_name": call.tool_id,
                "arguments": Value::Object(arguments),
            }))
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| ready::ReadyError::Tool {
                tool_id: call.tool_id.clone(),
                message: e.to_string(),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ready::ReadyError::Tool {
                tool_id: call.tool_id.clone(),
                message: format!("MCP tool call failed ({status}): {body}"),
            });
        }

        let result: Value = resp.json().await.map_err(|e| ready::ReadyError::Tool {
            tool_id: call.tool_id.clone(),
            message: e.to_string(),
        })?;

        // Extract text content from MCP result format
        let text = result["content"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .first()
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| result.to_string());

        let is_error = result["isError"].as_bool().unwrap_or(false);
        if is_error {
            return Err(ready::ReadyError::Tool {
                tool_id: call.tool_id.clone(),
                message: text,
            });
        }

        Ok(ToolResult::Success(Value::String(text)))
    }
}

/// Convert an OpenAI-format tool schema to a Ready ToolDescription.
fn convert_tool_schema(schema: &Value) -> Option<ToolDescription> {
    let func = schema.get("function")?;
    let name = func.get("name")?.as_str()?;
    let description = func
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("");

    let params = func.get("parameters");
    let properties = params
        .and_then(|p| p.get("properties"))
        .and_then(|p| p.as_object());
    let required: Vec<String> = params
        .and_then(|p| p.get("required"))
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let arguments = if let Some(props) = properties {
        // Sort: required first, then optional
        let mut args: Vec<ToolArgumentDescription> = props
            .iter()
            .map(|(key, val)| {
                let type_name = val
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("str")
                    .to_string();
                let desc = val
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let default = if required.contains(key) {
                    None
                } else {
                    Some("None".to_string())
                };
                ToolArgumentDescription {
                    name: key.clone(),
                    description: desc,
                    type_name,
                    default,
                }
            })
            .collect();
        // Required params first
        args.sort_by_key(|a| a.default.is_some());
        args
    } else {
        vec![]
    };

    Some(ToolDescription {
        id: name.to_string(),
        description: description.to_string(),
        arguments,
        returns: ToolReturnDescription {
            name: None,
            description: "Tool result".to_string(),
            type_name: Some("str".to_string()),
            fields: vec![],
        },
    })
}

// ---------------------------------------------------------------------------
// Procedure runner
// ---------------------------------------------------------------------------

/// Run a Ready plan, returning the final execution state.
///
/// `plan_json` is the serialized AbstractPlan. `inputs` are pre-filled variables.
/// `tool_schemas` are the MCP tool schemas for the agent.
pub async fn run_plan(
    plan_json: &str,
    inputs: HashMap<String, Value>,
    base_url: &str,
    agent_id: &str,
    tool_schemas: &[Value],
) -> Result<ExecutionState> {
    let plan: AbstractPlan = serde_json::from_str(plan_json)
        .map_err(|e| Error::Sop(format!("invalid plan JSON: {e}")))?;

    let mut registry = InMemoryToolRegistry::new();

    // Register MCP tools bridge
    let bridge = McpToolsBridge::new(base_url, agent_id, tool_schemas);
    registry
        .register_module(Box::new(bridge))
        .map_err(|e| Error::Sop(format!("failed to register MCP tools: {e}")))?;

    // Register Ready's built-in tools (delegate_to_large_language_model, etc.)
    let llm_client = ready::llm::client::OpenAiClient::new(None, None, None);
    let builtins = ready::tools::BuiltinToolsModule::new(Arc::new(llm_client));
    registry
        .register_module(Box::new(builtins))
        .map_err(|e| Error::Sop(format!("failed to register builtin tools: {e}")))?;

    let executor = SopExecutor::new(Arc::new(registry), None);

    info!(
        plan_name = plan.name,
        agent_id,
        steps = plan.steps.len(),
        "executing procedure"
    );

    let state = executor
        .execute(&plan, inputs, None)
        .await
        .map_err(|e| Error::Sop(format!("plan execution failed: {e}")))?;

    match state.status {
        ExecutionStatus::Completed => {
            info!(plan_name = plan.name, "procedure completed");
        }
        ExecutionStatus::Failed => {
            let err = state
                .error
                .as_ref()
                .map(|e| format!("{:?}", e))
                .unwrap_or_default();
            error!(plan_name = plan.name, error = err, "procedure failed");
        }
        ExecutionStatus::Suspended => {
            info!(
                plan_name = plan.name,
                reason = state.suspension_reason.as_deref().unwrap_or("unknown"),
                "procedure suspended (waiting for input)"
            );
        }
        _ => {}
    }

    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_tool_schema() {
        let schema = serde_json::json!({
            "type": "function",
            "function": {
                "name": "create_task",
                "description": "Create a new task",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Task title" },
                        "priority": { "type": "integer", "description": "Priority", "default": 1 }
                    },
                    "required": ["title"]
                }
            }
        });

        let desc = convert_tool_schema(&schema).unwrap();
        assert_eq!(desc.id, "create_task");
        assert_eq!(desc.arguments.len(), 2);
        // Required param (title) should come first
        assert_eq!(desc.arguments[0].name, "title");
        assert!(desc.arguments[0].default.is_none());
        // Optional param (priority) has default
        assert_eq!(desc.arguments[1].name, "priority");
        assert!(desc.arguments[1].default.is_some());
    }

    #[test]
    fn test_convert_tool_schema_no_params() {
        let schema = serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "List all tasks"
            }
        });

        let desc = convert_tool_schema(&schema).unwrap();
        assert_eq!(desc.id, "list_tasks");
        assert!(desc.arguments.is_empty());
    }
}
