//! Manages MCP stdio server lifecycle on the host.
//!
//! **Used in containerless mode only.** When Docker isolation is enabled,
//! MCP servers run inside the agent container instead. This module is for
//! `isolation: none` where the server needs to spawn and manage MCP
//! processes directly on the host machine (requires Node.js/npx).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use super::mcp::{McpToolDef, McpToolResult};
use super::mcp_stdio::McpStdioClient;
use crate::config::McpServerConfig;
use crate::error::{Error, Result};

/// Manages all MCP stdio servers and routes tool calls.
pub struct McpManager {
    /// Running MCP server clients, keyed by server name.
    servers: Arc<RwLock<HashMap<String, Arc<McpStdioClient>>>>,
    /// Tool name → server name mapping.
    tool_routing: Arc<RwLock<HashMap<String, String>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            tool_routing: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start all MCP servers from config.
    /// Servers that fail to start are logged and skipped.
    pub async fn start_servers(&self, configs: &HashMap<String, McpServerConfig>) {
        for (name, config) in configs {
            if config.server_type != "stdio" {
                info!(
                    name,
                    server_type = config.server_type,
                    "skipping non-stdio MCP server"
                );
                continue;
            }

            let command = match config.command.as_ref() {
                Some(cmd) => cmd.clone(),
                None => {
                    warn!(name, "MCP server has no command, skipping");
                    continue;
                }
            };

            let args = config.args.clone();
            let env = config.env.clone();

            match McpStdioClient::spawn(name, &command, &args, &env).await {
                Ok(client) => {
                    // Register all tools from this server
                    let tools = client.tools().await;
                    let mut routing = self.tool_routing.write().await;
                    for tool in &tools {
                        routing.insert(tool.name.clone(), name.clone());
                    }
                    info!(name, tools = tools.len(), "MCP server started");

                    let mut servers = self.servers.write().await;
                    servers.insert(name.clone(), Arc::new(client));
                }
                Err(e) => {
                    warn!(name, error = %e, "failed to start MCP server");
                }
            }
        }
    }

    /// Get all available tools across all servers.
    pub async fn list_tools(&self) -> Vec<McpToolDef> {
        let servers = self.servers.read().await;
        let mut all_tools = Vec::new();
        for server in servers.values() {
            all_tools.extend(server.tools().await);
        }
        all_tools
    }

    /// Get tool schemas in OpenAI function-calling format.
    pub async fn tool_schemas(&self) -> Vec<serde_json::Value> {
        self.list_tools()
            .await
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }

    /// Call a tool, routing to the correct MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        let server_name = {
            let routing = self.tool_routing.read().await;
            routing
                .get(tool_name)
                .cloned()
                .ok_or_else(|| Error::Tool(format!("unknown tool: {tool_name}")))?
        };

        let server = {
            let servers = self.servers.read().await;
            servers
                .get(&server_name)
                .cloned()
                .ok_or_else(|| Error::Tool(format!("MCP server '{server_name}' not running")))?
        };

        server.call_tool(tool_name, arguments).await
    }

    /// Check if a tool is available.
    pub async fn has_tool(&self, tool_name: &str) -> bool {
        let routing = self.tool_routing.read().await;
        routing.contains_key(tool_name)
    }

    /// Get the number of running servers.
    pub async fn server_count(&self) -> usize {
        self.servers.read().await.len()
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
