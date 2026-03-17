//! MCP client using stdio transport (spawns a child process).
//!
//! MCP stdio servers communicate via JSON-RPC over stdin/stdout.
//! Each line on stdout is a complete JSON-RPC response.
//!
//! **Used in containerless mode only.** When Docker isolation is enabled,
//! MCP servers run inside the agent container instead (the harness starts
//! them from the `MCP_SERVERS` environment variable). This module is for
//! advanced users who opt into `isolation: none` and want tool access
//! without Docker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

use super::mcp::{InitializeResult, JsonRpcRequest, JsonRpcResponse, McpToolDef, McpToolResult};
use crate::error::{Error, Result};

/// An MCP client that communicates with a server process via stdin/stdout.
pub struct McpStdioClient {
    name: String,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicI64,
    tools: Mutex<Vec<McpToolDef>>,
    _child: Arc<Mutex<Child>>,
}

impl McpStdioClient {
    /// Spawn an MCP stdio server and establish communication.
    pub async fn spawn(
        name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        info!(name, command, ?args, "spawning MCP stdio server");

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        for (k, v) in env {
            if !v.is_empty() {
                cmd.env(k, v);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Tool(format!("failed to spawn MCP server '{name}': {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Tool(format!("no stdin for MCP server '{name}'")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Tool(format!("no stdout for MCP server '{name}'")))?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Background task: read stdout lines and dispatch responses
        let pending_clone = pending.clone();
        let server_name = name.to_string();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<JsonRpcResponse>(&line) {
                    Ok(resp) => {
                        if let Some(ref id) = resp.id {
                            if let Some(id_num) = id.as_i64() {
                                let mut map = pending_clone.lock().await;
                                if let Some(tx) = map.remove(&id_num) {
                                    let _ = tx.send(resp);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            server = server_name,
                            line,
                            error = %e,
                            "non-JSON line from MCP server"
                        );
                    }
                }
            }
            warn!(server = server_name, "MCP server stdout closed");
        });

        let client = Self {
            name: name.to_string(),
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            next_id: AtomicI64::new(1),
            tools: Mutex::new(Vec::new()),
            _child: Arc::new(Mutex::new(child)),
        };

        // Initialize the MCP connection
        client.initialize().await?;

        // Discover tools
        client.refresh_tools().await?;

        Ok(client)
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn send(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        let id = request
            .id
            .as_ref()
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::Tool("request has no id".into()))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, tx);
        }

        let mut line = serde_json::to_string(request)
            .map_err(|e| Error::Tool(format!("failed to serialize request: {e}")))?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await.map_err(|e| {
                Error::Tool(format!(
                    "failed to write to MCP server '{}': {e}",
                    self.name
                ))
            })?;
            stdin.flush().await.ok();
        }

        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| Error::Tool(format!("MCP server '{}' timed out", self.name)))?
            .map_err(|_| Error::Tool(format!("MCP server '{}' dropped response", self.name)))?;

        if let Some(ref err) = resp.error {
            return Err(Error::Tool(format!(
                "MCP server '{}' error: {} ({})",
                self.name, err.message, err.code
            )));
        }

        Ok(resp)
    }

    /// MCP initialize handshake.
    async fn initialize(&self) -> Result<InitializeResult> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(
            id,
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "xpressclaw",
                    "version": "0.1.0"
                }
            })),
        );

        let resp = self.send(&req).await?;
        let result: InitializeResult = serde_json::from_value(
            resp.result
                .ok_or_else(|| Error::Tool("initialize returned no result".into()))?,
        )
        .map_err(|e| Error::Tool(format!("failed to parse initialize result: {e}")))?;

        info!(
            server = self.name,
            protocol = result.protocol_version,
            server_name = result.server_info.name,
            "MCP server initialized"
        );

        // Send initialized notification
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        let mut line = serde_json::to_string(&notif).unwrap();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.ok();
        stdin.flush().await.ok();

        Ok(result)
    }

    /// Discover tools from the server.
    pub async fn refresh_tools(&self) -> Result<()> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(id, "tools/list", None);
        let resp = self.send(&req).await?;

        if let Some(result) = resp.result {
            if let Some(tools_array) = result.get("tools") {
                let tools: Vec<McpToolDef> =
                    serde_json::from_value(tools_array.clone()).unwrap_or_default();
                info!(
                    server = self.name,
                    count = tools.len(),
                    "discovered MCP tools"
                );
                *self.tools.lock().await = tools;
            }
        }

        Ok(())
    }

    /// Get the list of tools this server provides.
    pub async fn tools(&self) -> Vec<McpToolDef> {
        self.tools.lock().await.clone()
    }

    /// Call a tool on this server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(
            id,
            "tools/call",
            Some(serde_json::json!({
                "name": name,
                "arguments": arguments
            })),
        );

        let resp = self.send(&req).await?;
        let result: McpToolResult = serde_json::from_value(
            resp.result
                .ok_or_else(|| Error::Tool(format!("tool '{}' returned no result", name)))?,
        )
        .map_err(|e| Error::Tool(format!("failed to parse tool result: {e}")))?;

        Ok(result)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}
