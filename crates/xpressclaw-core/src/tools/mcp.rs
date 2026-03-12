use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

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
            response
                .result
                .ok_or_else(|| Error::Tool("MCP initialize returned no result".into()))?,
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
        let _ = self.client.post(&self.base_url).json(&notif).send().await;

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

/// A validated tool call request extracted from JSON-RPC.
///
/// When `McpServer::handle_request()` returns `McpRequestResult::ToolCall`,
/// the caller should execute this through the `McpProxy` (which enforces
/// policies and permissions) and then wrap the result in a `JsonRpcResponse`.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    /// The JSON-RPC request ID (needed to construct the response).
    pub request_id: Option<serde_json::Value>,
    /// The tool name to execute.
    pub tool_name: String,
    /// The tool arguments (JSON object).
    pub arguments: serde_json::Value,
}

impl ToolCallRequest {
    /// Wrap a successful tool result in a JSON-RPC response.
    pub fn success_response(&self, result: &McpToolResult) -> JsonRpcResponse {
        JsonRpcResponse::success(
            self.request_id.clone(),
            serde_json::to_value(result).unwrap_or_else(|_| {
                serde_json::json!({
                    "content": [{"type": "text", "text": "failed to serialize result"}],
                    "isError": true
                })
            }),
        )
    }

    /// Wrap an error in a JSON-RPC response.
    pub fn error_response(&self, message: &str) -> JsonRpcResponse {
        JsonRpcResponse::success(
            self.request_id.clone(),
            serde_json::json!({
                "content": [{"type": "text", "text": message}],
                "isError": true
            }),
        )
    }
}

/// Result of handling an incoming JSON-RPC request.
pub enum McpRequestResult {
    /// Request was handled synchronously (initialize, tools/list, errors).
    Response(JsonRpcResponse),
    /// A tool call that needs async execution through the proxy/policy engine.
    /// The caller should:
    /// 1. Execute via `McpProxy::call_tool()`
    /// 2. Convert the result with `ToolCallRequest::success_response()` or `error_response()`
    ToolCall(ToolCallRequest),
}

/// Handles MCP protocol requests from agent harness containers.
///
/// This acts as the server side — agents in containers call tools through us.
/// For `tools/call`, the server validates the request and returns a
/// `ToolCallRequest` for the caller to execute asynchronously through the
/// proxy (which enforces policies and permissions).
///
/// ```text
/// Agent ──JSON-RPC──► McpServer ──ToolCallRequest──► McpProxy ──► MCP Server
///                                                       │
///                                                  ToolPolicyEngine
///                                                  ToolRegistry
/// ```
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

    /// Handle an incoming JSON-RPC request.
    ///
    /// Returns `McpRequestResult::Response` for requests handled synchronously
    /// (initialize, tools/list, validation errors).
    /// Returns `McpRequestResult::ToolCall` for tool calls that need async
    /// execution through the proxy.
    pub fn handle_request(&self, request: &JsonRpcRequest) -> McpRequestResult {
        match request.method.as_str() {
            "initialize" => McpRequestResult::Response(self.handle_initialize(request)),
            "tools/list" => McpRequestResult::Response(self.handle_list_tools(request)),
            "tools/call" => self.handle_call_tool(request),
            // Notifications — no response needed
            "notifications/initialized" | "notifications/cancelled" => McpRequestResult::Response(
                JsonRpcResponse::success(request.id.clone(), serde_json::json!({})),
            ),
            _ => McpRequestResult::Response(JsonRpcResponse::error(
                request.id.clone(),
                -32601,
                &format!("method not found: {}", request.method),
            )),
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
        JsonRpcResponse::success(request.id.clone(), serde_json::json!({ "tools": tools }))
    }

    fn handle_call_tool(&self, request: &JsonRpcRequest) -> McpRequestResult {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return McpRequestResult::Response(JsonRpcResponse::error(
                    request.id.clone(),
                    -32602,
                    "missing params for tools/call",
                ));
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return McpRequestResult::Response(JsonRpcResponse::error(
                    request.id.clone(),
                    -32602,
                    "missing 'name' in tools/call params",
                ));
            }
        };

        if !self.tools.contains_key(name) {
            return McpRequestResult::Response(JsonRpcResponse::error(
                request.id.clone(),
                -32602,
                &format!("tool not found: {name}"),
            ));
        }

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        debug!(tool = name, "validated tool call, delegating to proxy");

        McpRequestResult::ToolCall(ToolCallRequest {
            request_id: request.id.clone(),
            tool_name: name.to_string(),
            arguments,
        })
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
        let resp = match server.handle_request(&req) {
            McpRequestResult::Response(r) => r,
            McpRequestResult::ToolCall(_) => panic!("expected response"),
        };

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
        let resp = match server.handle_request(&req) {
            McpRequestResult::Response(r) => r,
            McpRequestResult::ToolCall(_) => panic!("expected response"),
        };

        assert!(!resp.is_error());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
    }

    #[test]
    fn test_mcp_server_call_tool_returns_tool_call() {
        let mut server = McpServer::new();
        server.register_tool(McpToolDef {
            name: "search".into(),
            description: "Search".into(),
            input_schema: serde_json::json!({}),
        });

        let req = JsonRpcRequest::new(
            3,
            "tools/call",
            Some(serde_json::json!({"name": "search", "arguments": {"query": "test"}})),
        );
        match server.handle_request(&req) {
            McpRequestResult::ToolCall(call) => {
                assert_eq!(call.tool_name, "search");
                assert_eq!(call.arguments["query"], "test");
            }
            McpRequestResult::Response(_) => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn test_mcp_server_call_tool_not_found() {
        let server = McpServer::new();
        let req = JsonRpcRequest::new(
            3,
            "tools/call",
            Some(serde_json::json!({"name": "nonexistent", "arguments": {}})),
        );
        let resp = match server.handle_request(&req) {
            McpRequestResult::Response(r) => r,
            McpRequestResult::ToolCall(_) => panic!("expected error response"),
        };

        assert!(resp.is_error());
        assert!(resp.error.unwrap().message.contains("tool not found"));
    }

    #[test]
    fn test_mcp_server_unknown_method() {
        let server = McpServer::new();
        let req = JsonRpcRequest::new(4, "unknown/method", None);
        let resp = match server.handle_request(&req) {
            McpRequestResult::Response(r) => r,
            McpRequestResult::ToolCall(_) => panic!("expected error response"),
        };

        assert!(resp.is_error());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_tool_call_request_response_helpers() {
        let call = ToolCallRequest {
            request_id: Some(serde_json::Value::Number(1.into())),
            tool_name: "test".into(),
            arguments: serde_json::json!({}),
        };

        let result = McpToolResult {
            content: vec![McpContent::Text {
                text: "hello".into(),
            }],
            is_error: false,
        };

        let resp = call.success_response(&result);
        assert!(!resp.is_error());

        let resp = call.error_response("something went wrong");
        // error_response wraps as tool result with isError: true, not JSON-RPC error
        assert!(!resp.is_error()); // not a JSON-RPC error
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
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
