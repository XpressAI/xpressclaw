use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{Error, Result};

// ── JSON-RPC 2.0 types ──

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(id: i64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::Number(id.into())),
            method: method.to_string(),
            params,
        }
    }

    /// Create a notification (no id, no response expected).
    pub fn notification(method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.to_string(),
            params,
        }
    }
}

impl JsonRpcResponse {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<serde_json::Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

// ── MCP protocol types ──

/// An MCP tool definition as returned by tools/list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
}

/// Result of calling a tool via MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// Content block in an MCP tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(alias = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource {
        uri: String,
        #[serde(default)]
        text: Option<String>,
    },
}

/// Server capabilities exchanged during initialization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
    #[serde(default)]
    pub resources: Option<serde_json::Value>,
    #[serde(default)]
    pub prompts: Option<serde_json::Value>,
}

/// MCP server info returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Result of the MCP initialize handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

// ── MCP Client (HTTP transport) ──

/// Client for communicating with MCP servers over HTTP (SSE or streamable HTTP).
///
/// In the xpressclaw architecture, MCP servers run in Docker containers and
/// expose an HTTP endpoint. The server proxies requests from agents through
/// this client.
pub struct McpClient {
    client: Client,
    base_url: String,
    next_id: AtomicI64,
    server_info: Option<InitializeResult>,
    /// Cached tool list from the server.
    tools: Vec<McpToolDef>,
}

impl McpClient {
    /// Create a new MCP client pointing to an HTTP endpoint.
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            next_id: AtomicI64::new(1),
            server_info: None,
            tools: Vec::new(),
        }
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and get the response.
    async fn send(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        let resp = self
            .client
            .post(&self.base_url)
            .json(request)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| Error::Tool(format!("MCP request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Tool(format!("MCP server error {status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| Error::Tool(format!("invalid MCP response: {e}")))
    }

    /// Perform the MCP initialization handshake.
    pub async fn initialize(&mut self) -> Result<&InitializeResult> {
        let request = JsonRpcRequest::new(
            self.next_id(),
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "xpressclaw",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        );

        let response = self.send(&request).await?;

        if let Some(error) = response.error {
            return Err(Error::Tool(format!(
                "MCP initialize failed: {} (code {})",
                error.message, error.code
            )));
        }

        let result: InitializeResult = serde_json::from_value(
            response.result.ok_or_else(|| {
                Error::Tool("MCP initialize returned no result".into())
            })?,
        )
        .map_err(|e| Error::Tool(format!("invalid initialize result: {e}")))?;

        debug!(
            server = result.server_info.name,
            version = result.server_info.version,
            protocol = result.protocol_version,
            "MCP server initialized"
        );

        self.server_info = Some(result);

        // Send initialized notification
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        // Fire and forget — notifications don't expect a response
        let _ = self
            .client
            .post(&self.base_url)
            .json(&notif)
            .send()
            .await;

        Ok(self.server_info.as_ref().unwrap())
    }

    /// List tools available on the MCP server.
    pub async fn list_tools(&mut self) -> Result<&[McpToolDef]> {
        let request = JsonRpcRequest::new(self.next_id(), "tools/list", None);
        let response = self.send(&request).await?;

        if let Some(error) = response.error {
            return Err(Error::Tool(format!(
                "tools/list failed: {} (code {})",
                error.message, error.code
            )));
        }

        let result = response
            .result
            .ok_or_else(|| Error::Tool("tools/list returned no result".into()))?;

        let tools_value = result
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new()));

        self.tools = serde_json::from_value(tools_value)
            .map_err(|e| Error::Tool(format!("invalid tools list: {e}")))?;

        debug!(count = self.tools.len(), "fetched tools from MCP server");
        Ok(&self.tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult> {
        let request = JsonRpcRequest::new(
            self.next_id(),
            "tools/call",
            Some(serde_json::json!({
                "name": name,
                "arguments": arguments,
            })),
        );

        let response = self.send(&request).await?;

        if let Some(error) = response.error {
            return Err(Error::ToolExecution(format!(
                "tool '{}' failed: {} (code {})",
                name, error.message, error.code
            )));
        }

        let result = response
            .result
            .ok_or_else(|| Error::ToolExecution(format!("tool '{name}' returned no result")))?;

        serde_json::from_value(result)
            .map_err(|e| Error::ToolExecution(format!("invalid tool result: {e}")))
    }

    /// Get cached tools (call list_tools first).
    pub fn cached_tools(&self) -> &[McpToolDef] {
        &self.tools
    }

    /// Get server info (call initialize first).
    pub fn server_info(&self) -> Option<&InitializeResult> {
        self.server_info.as_ref()
    }
}

// ── MCP Server (handles incoming requests from harness containers) ──

/// Handles MCP protocol requests from agent harness containers.
///
/// This acts as the server side — agents in containers call tools through us,
/// and we route them to the actual MCP server containers or built-in tools.
pub struct McpServer {
    /// Tools that this server exposes to harness containers.
    tools: HashMap<String, McpToolDef>,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool that this server exposes.
    pub fn register_tool(&mut self, tool: McpToolDef) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// Handle an incoming JSON-RPC request and produce a response.
    pub fn handle_request(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "tools/list" => self.handle_list_tools(request),
            "tools/call" => self.handle_call_tool(request),
            _ => JsonRpcResponse::error(
                request.id.clone(),
                -32601,
                &format!("method not found: {}", request.method),
            ),
        }
    }

    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "xpressclaw",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
    }

    fn handle_list_tools(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let tools: Vec<&McpToolDef> = self.tools.values().collect();
        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::json!({ "tools": tools }),
        )
    }

    fn handle_call_tool(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    request.id.clone(),
                    -32602,
                    "missing params for tools/call",
                );
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return JsonRpcResponse::error(
                    request.id.clone(),
                    -32602,
                    "missing 'name' in tools/call params",
                );
            }
        };

        if !self.tools.contains_key(name) {
            return JsonRpcResponse::error(
                request.id.clone(),
                -32602,
                &format!("tool not found: {name}"),
            );
        }

        // For now, tool execution happens through the proxy layer.
        // This server just validates the request structure.
        warn!(
            tool = name,
            "tool call received but execution is handled by proxy"
        );

        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "Tool execution is handled by the proxy layer"
                }],
                "isError": false
            }),
        )
    }

    /// Get the number of registered tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_request() {
        let req = JsonRpcRequest::new(1, "tools/list", None);
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.method, "tools/list");
        assert!(req.params.is_none());

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let resp = JsonRpcResponse::success(
            Some(serde_json::Value::Number(1.into())),
            serde_json::json!({"tools": []}),
        );
        assert!(!resp.is_error());
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let resp = JsonRpcResponse::error(
            Some(serde_json::Value::Number(1.into())),
            -32601,
            "method not found",
        );
        assert!(resp.is_error());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn test_notification() {
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        assert!(notif.id.is_none());
    }

    #[test]
    fn test_mcp_server_initialize() {
        let server = McpServer::new();
        let req = JsonRpcRequest::new(1, "initialize", None);
        let resp = server.handle_request(&req);

        assert!(!resp.is_error());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "xpressclaw");
    }

    #[test]
    fn test_mcp_server_list_tools() {
        let mut server = McpServer::new();
        server.register_tool(McpToolDef {
            name: "search".into(),
            description: "Search the web".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        });

        let req = JsonRpcRequest::new(2, "tools/list", None);
        let resp = server.handle_request(&req);

        assert!(!resp.is_error());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
    }

    #[test]
    fn test_mcp_server_call_tool_not_found() {
        let server = McpServer::new();
        let req = JsonRpcRequest::new(
            3,
            "tools/call",
            Some(serde_json::json!({"name": "nonexistent", "arguments": {}})),
        );
        let resp = server.handle_request(&req);

        assert!(resp.is_error());
        assert!(resp
            .error
            .unwrap()
            .message
            .contains("tool not found"));
    }

    #[test]
    fn test_mcp_server_unknown_method() {
        let server = McpServer::new();
        let req = JsonRpcRequest::new(4, "unknown/method", None);
        let resp = server.handle_request(&req);

        assert!(resp.is_error());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_mcp_tool_result_deserialize() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "Hello, world!"},
                {"type": "image", "data": "base64...", "mimeType": "image/png"}
            ],
            "isError": false
        });

        let result: McpToolResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.content.len(), 2);
        assert!(!result.is_error);

        match &result.content[0] {
            McpContent::Text { text } => assert_eq!(text, "Hello, world!"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_mcp_client_creation() {
        let client = McpClient::new("http://localhost:8080/mcp");
        assert_eq!(client.cached_tools().len(), 0);
        assert!(client.server_info().is_none());
    }
}
