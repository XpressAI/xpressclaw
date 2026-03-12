use std::collections::HashMap;
use std::time::Instant;

use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::tools::mcp::{McpClient, McpContent, McpToolDef, McpToolResult};
use crate::tools::policy::{ApprovalMode, PolicyDecision, ToolPolicyEngine};
use crate::tools::registry::{ToolCategory, ToolDefinition, ToolRegistry};

/// Maps MCP server names to their connection info.
#[derive(Debug, Clone)]
pub struct McpServerEntry {
    pub name: String,
    pub url: String,
}

/// Proxy/enforcer between agents and MCP server containers.
///
/// The proxy:
/// 1. Connects to MCP server containers and discovers their tools
/// 2. Registers discovered tools in the ToolRegistry
/// 3. Evaluates tool policy rules (glob-pattern allow/deny/approval)
/// 4. Enforces per-agent permissions before forwarding tool calls
/// 5. Logs all tool invocations for auditing
///
/// ```text
/// Agent Container ──► McpProxy ──► MCP Server Container
///                       │
///                  ToolPolicyEngine    (pattern-based rules)
///                  ToolRegistry        (per-agent permissions)
/// ```
pub struct McpProxy {
    /// Active MCP server connections: server_name → client
    clients: HashMap<String, McpClient>,
    /// Which server provides which tool: tool_name → server_name
    tool_routing: HashMap<String, String>,
    /// Policy engine for pattern-based tool rules.
    policy: ToolPolicyEngine,
}

impl McpProxy {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_routing: HashMap::new(),
            policy: ToolPolicyEngine::default(),
        }
    }

    /// Create a proxy with policy rules.
    pub fn with_policy(policy: ToolPolicyEngine) -> Self {
        Self {
            clients: HashMap::new(),
            tool_routing: HashMap::new(),
            policy,
        }
    }

    /// Get a reference to the policy engine.
    pub fn policy(&self) -> &ToolPolicyEngine {
        &self.policy
    }

    /// Connect to an MCP server and discover its tools.
    ///
    /// Returns the list of tools that were discovered and registered.
    pub async fn connect_server(
        &mut self,
        name: &str,
        url: &str,
        registry: &mut ToolRegistry,
    ) -> Result<Vec<McpToolDef>> {
        info!(server = name, url, "connecting to MCP server");

        let mut client = McpClient::new(url);

        // Initialize
        client
            .initialize()
            .await
            .map_err(|e| Error::Tool(format!("failed to initialize MCP server '{name}': {e}")))?;

        // Discover tools
        let tools = client.list_tools().await.map_err(|e| {
            Error::Tool(format!(
                "failed to list tools from MCP server '{name}': {e}"
            ))
        })?;

        let discovered: Vec<McpToolDef> = tools.to_vec();

        // Register each tool in the tool registry
        for tool in &discovered {
            let tool_name = format!("{name}__{}", tool.name);
            registry.register_tool(ToolDefinition {
                name: tool_name.clone(),
                description: tool.description.clone(),
                category: ToolCategory::Mcp,
                input_schema: tool.input_schema.clone(),
                mcp_server: Some(name.to_string()),
                enabled: true,
            });
            self.tool_routing.insert(tool_name, name.to_string());
        }

        info!(
            server = name,
            tools = discovered.len(),
            "connected to MCP server"
        );

        self.clients.insert(name.to_string(), client);
        Ok(discovered)
    }

    /// Disconnect from an MCP server and remove its tools.
    pub fn disconnect_server(&mut self, name: &str, registry: &mut ToolRegistry) {
        self.clients.remove(name);

        // Remove tools from this server
        let tools_to_remove: Vec<String> = self
            .tool_routing
            .iter()
            .filter(|(_, server)| *server == name)
            .map(|(tool, _)| tool.clone())
            .collect();

        for tool_name in &tools_to_remove {
            registry.unregister_tool(tool_name);
            self.tool_routing.remove(tool_name);
        }

        info!(server = name, "disconnected from MCP server");
    }

    /// Call a tool through the proxy, enforcing policies and permissions.
    ///
    /// Enforcement order:
    /// 1. **Policy rules** — glob-pattern allow/deny/approval (coarse-grained)
    /// 2. **Registry permissions** — per-agent allow/deny (fine-grained)
    /// 3. **Execution** — forward to MCP server, log result
    ///
    /// The tool_name should be in the format "server__tool" as registered.
    pub async fn call_tool(
        &self,
        agent_id: &str,
        tool_name: &str,
        arguments: serde_json::Value,
        registry: &ToolRegistry,
    ) -> Result<McpToolResult> {
        // 1. Check policy rules (pattern-based, coarse-grained)
        match self.policy.evaluate(tool_name, agent_id) {
            PolicyDecision::Allow => { /* proceed to permission check */ }
            PolicyDecision::Deny { reason } => {
                warn!(agent_id, tool_name, %reason, "tool call denied by policy");
                registry.log_invocation(
                    agent_id,
                    tool_name,
                    Some(&arguments.to_string()),
                    None,
                    None,
                    false,
                    Some(&reason),
                )?;
                return Err(Error::ToolPermission(reason));
            }
            PolicyDecision::NeedsApproval {
                mode,
                matched_pattern,
            } => {
                match mode {
                    ApprovalMode::Script { ref command } => {
                        // Run the approval script — it gets only metadata, not arguments
                        match ToolPolicyEngine::run_approval_script(command, tool_name, agent_id)
                            .await
                        {
                            Ok(true) => {
                                debug!(tool_name, agent_id, "approval script approved tool call");
                            }
                            Ok(false) => {
                                let reason = format!(
                                    "denied by approval script for pattern '{matched_pattern}'"
                                );
                                warn!(agent_id, tool_name, %reason, "tool call denied by approval script");
                                registry.log_invocation(
                                    agent_id,
                                    tool_name,
                                    Some(&arguments.to_string()),
                                    None,
                                    None,
                                    false,
                                    Some(&reason),
                                )?;
                                return Err(Error::ToolPermission(reason));
                            }
                            Err(e) => {
                                let reason = format!("approval script failed: {e}");
                                warn!(agent_id, tool_name, %reason, "approval script error");
                                registry.log_invocation(
                                    agent_id,
                                    tool_name,
                                    Some(&arguments.to_string()),
                                    None,
                                    None,
                                    false,
                                    Some(&reason),
                                )?;
                                return Err(Error::ToolExecution(reason));
                            }
                        }
                    }
                    ApprovalMode::Manual => {
                        let reason = format!(
                            "tool '{tool_name}' requires manual approval (matched pattern '{matched_pattern}')"
                        );
                        registry.log_invocation(
                            agent_id,
                            tool_name,
                            Some(&arguments.to_string()),
                            None,
                            None,
                            false,
                            Some(&reason),
                        )?;
                        return Err(Error::ToolPermission(reason));
                    }
                    ApprovalMode::Agent {
                        agent_id: ref approver_id,
                    } => {
                        let reason = format!(
                            "tool '{tool_name}' requires approval from agent '{approver_id}' (not yet implemented)"
                        );
                        registry.log_invocation(
                            agent_id,
                            tool_name,
                            Some(&arguments.to_string()),
                            None,
                            None,
                            false,
                            Some(&reason),
                        )?;
                        return Err(Error::ToolPermission(reason));
                    }
                }
            }
        }

        // 2. Check per-agent registry permissions (fine-grained)
        if !registry.is_tool_allowed(agent_id, tool_name) {
            warn!(
                agent_id,
                tool_name, "tool call denied: agent does not have permission"
            );
            registry.log_invocation(
                agent_id,
                tool_name,
                Some(&arguments.to_string()),
                None,
                None,
                false,
                Some("permission denied"),
            )?;
            return Err(Error::ToolPermission(format!(
                "agent '{agent_id}' is not allowed to use tool '{tool_name}'"
            )));
        }

        // Find which server handles this tool
        let server_name = self
            .tool_routing
            .get(tool_name)
            .ok_or_else(|| Error::ToolNotFound {
                name: tool_name.to_string(),
            })?;

        let client = self
            .clients
            .get(server_name)
            .ok_or_else(|| Error::Tool(format!("MCP server '{server_name}' is not connected")))?;

        // Extract the actual tool name (strip the server prefix)
        let actual_tool_name = tool_name
            .strip_prefix(&format!("{server_name}__"))
            .unwrap_or(tool_name);

        debug!(
            agent_id,
            tool_name,
            server = server_name,
            "proxying tool call to MCP server"
        );

        let start = Instant::now();
        let result = client.call_tool(actual_tool_name, arguments.clone()).await;
        let duration_ms = start.elapsed().as_millis() as i64;

        // Log the invocation
        match &result {
            Ok(tool_result) => {
                let output = tool_result
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        McpContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                registry.log_invocation(
                    agent_id,
                    tool_name,
                    Some(&arguments.to_string()),
                    Some(&output),
                    Some(duration_ms),
                    !tool_result.is_error,
                    if tool_result.is_error {
                        Some(&output)
                    } else {
                        None
                    },
                )?;
            }
            Err(e) => {
                registry.log_invocation(
                    agent_id,
                    tool_name,
                    Some(&arguments.to_string()),
                    None,
                    Some(duration_ms),
                    false,
                    Some(&e.to_string()),
                )?;
            }
        }

        result
    }

    /// List all available tools for an agent (respecting permissions).
    pub fn available_tools(
        &self,
        agent_id: &str,
        registry: &ToolRegistry,
    ) -> Vec<serde_json::Value> {
        registry.get_tool_schemas(agent_id)
    }

    /// Get the names of connected MCP servers.
    pub fn connected_servers(&self) -> Vec<&str> {
        self.clients.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a specific MCP server is connected.
    pub fn is_server_connected(&self, name: &str) -> bool {
        self.clients.contains_key(name)
    }

    /// Get the number of tools routed through the proxy.
    pub fn routed_tool_count(&self) -> usize {
        self.tool_routing.len()
    }
}

impl Default for McpProxy {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::db::Database;
    use crate::tools::policy::{PolicyAction, ToolPolicyRule};
    use crate::tools::registry::ToolPermission;

    fn setup() -> (Arc<Database>, ToolRegistry, McpProxy) {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = ToolRegistry::new(db.clone());
        let proxy = McpProxy::new();
        (db, registry, proxy)
    }

    #[test]
    fn test_proxy_creation() {
        let (_, _, proxy) = setup();
        assert_eq!(proxy.connected_servers().len(), 0);
        assert_eq!(proxy.routed_tool_count(), 0);
        assert!(proxy.policy().is_empty());
    }

    #[test]
    fn test_disconnect_removes_tools() {
        let (_, mut registry, mut proxy) = setup();

        // Manually register tools as if a server was connected
        registry.register_tool(ToolDefinition {
            name: "search__web_search".into(),
            description: "Search the web".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("search".into()),
            enabled: true,
        });
        proxy
            .tool_routing
            .insert("search__web_search".into(), "search".into());
        proxy
            .clients
            .insert("search".into(), McpClient::new("http://fake"));

        assert_eq!(proxy.connected_servers().len(), 1);
        assert_eq!(proxy.routed_tool_count(), 1);
        assert!(registry.get_tool("search__web_search").is_some());

        // Disconnect
        proxy.disconnect_server("search", &mut registry);

        assert_eq!(proxy.connected_servers().len(), 0);
        assert_eq!(proxy.routed_tool_count(), 0);
        assert!(registry.get_tool("search__web_search").is_none());
    }

    #[tokio::test]
    async fn test_call_tool_permission_denied() {
        let (_, mut registry, mut proxy) = setup();

        // Register a tool
        registry.register_tool(ToolDefinition {
            name: "search__web_search".into(),
            description: "Search the web".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("search".into()),
            enabled: true,
        });
        proxy
            .tool_routing
            .insert("search__web_search".into(), "search".into());

        // Deny permission
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "search__web_search".into(),
            allowed: false,
            ..Default::default()
        });

        let result = proxy
            .call_tool(
                "atlas",
                "search__web_search",
                serde_json::json!({"query": "test"}),
                &registry,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::ToolPermission(msg) => {
                assert!(msg.contains("atlas"));
                assert!(msg.contains("search__web_search"));
            }
            e => panic!("expected ToolPermission, got: {e}"),
        }

        // Verify it was logged as denied
        let logs = registry.get_logs(Some("atlas"), None, 10).unwrap();
        assert_eq!(logs.len(), 1);
        assert!(!logs[0].success);
        assert_eq!(logs[0].error_message.as_deref(), Some("permission denied"));
    }

    #[tokio::test]
    async fn test_call_tool_not_found() {
        let (_, registry, proxy) = setup();

        let result = proxy
            .call_tool(
                "atlas",
                "nonexistent_tool",
                serde_json::json!({}),
                &registry,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::ToolNotFound { name } => assert_eq!(name, "nonexistent_tool"),
            e => panic!("expected ToolNotFound, got: {e}"),
        }
    }

    #[test]
    fn test_available_tools_respects_permissions() {
        let (_, mut registry, proxy) = setup();

        registry.register_tool(ToolDefinition {
            name: "tool_a".into(),
            description: "Tool A".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("server".into()),
            enabled: true,
        });
        registry.register_tool(ToolDefinition {
            name: "tool_b".into(),
            description: "Tool B".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("server".into()),
            enabled: true,
        });

        // Deny tool_b for atlas
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "tool_b".into(),
            allowed: false,
            ..Default::default()
        });

        let tools = proxy.available_tools("atlas", &registry);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "tool_a");

        // Hermes gets both
        let tools = proxy.available_tools("hermes", &registry);
        assert_eq!(tools.len(), 2);
    }

    // ── Policy integration tests ──

    #[tokio::test]
    async fn test_policy_deny_blocks_tool_call() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut registry = ToolRegistry::new(db.clone());
        let mut proxy = McpProxy::with_policy(ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "dangerous_*".into(),
            action: PolicyAction::Deny,
            approval: None,
        }]));

        // Register and route the tool
        registry.register_tool(ToolDefinition {
            name: "dangerous_delete".into(),
            description: "Delete things".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("server".into()),
            enabled: true,
        });
        proxy
            .tool_routing
            .insert("dangerous_delete".into(), "server".into());

        let result = proxy
            .call_tool(
                "atlas",
                "dangerous_delete",
                serde_json::json!({}),
                &registry,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::ToolPermission(msg) => {
                assert!(msg.contains("denied by policy"));
                assert!(msg.contains("dangerous_*"));
            }
            e => panic!("expected ToolPermission, got: {e}"),
        }

        // Verify logged
        let logs = registry.get_logs(Some("atlas"), None, 10).unwrap();
        assert_eq!(logs.len(), 1);
        assert!(!logs[0].success);
    }

    #[tokio::test]
    async fn test_policy_allow_with_denied_catch_all() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut registry = ToolRegistry::new(db.clone());
        let mut proxy = McpProxy::with_policy(ToolPolicyEngine::new(vec![
            ToolPolicyRule {
                pattern: "safe_*".into(),
                action: PolicyAction::Allow,
                approval: None,
            },
            ToolPolicyRule {
                pattern: "*".into(),
                action: PolicyAction::Deny,
                approval: None,
            },
        ]));

        // Register tools
        for name in ["safe_read", "unsafe_write"] {
            registry.register_tool(ToolDefinition {
                name: name.into(),
                description: name.into(),
                category: ToolCategory::Mcp,
                input_schema: serde_json::json!({}),
                mcp_server: Some("server".into()),
                enabled: true,
            });
            proxy.tool_routing.insert(name.into(), "server".into());
        }

        // safe_read: policy allows, but will fail at routing (no real server)
        // — that's fine, the point is it passes the policy check
        let result = proxy
            .call_tool("atlas", "safe_read", serde_json::json!({}), &registry)
            .await;
        // Should fail with "server not connected", not "policy denied"
        match result.unwrap_err() {
            Error::Tool(msg) => assert!(msg.contains("not connected")),
            e => panic!("expected Tool(not connected), got: {e}"),
        }

        // unsafe_write: policy denies
        let result = proxy
            .call_tool("atlas", "unsafe_write", serde_json::json!({}), &registry)
            .await;
        match result.unwrap_err() {
            Error::ToolPermission(msg) => assert!(msg.contains("denied by policy")),
            e => panic!("expected ToolPermission, got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_policy_manual_approval_required() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = ToolRegistry::new(db.clone());
        let mut proxy = McpProxy::with_policy(ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "*".into(),
            action: PolicyAction::RequireApproval,
            approval: None, // defaults to Manual
        }]));

        proxy.tool_routing.insert("tool".into(), "server".into());

        let result = proxy
            .call_tool("atlas", "tool", serde_json::json!({}), &registry)
            .await;

        match result.unwrap_err() {
            Error::ToolPermission(msg) => {
                assert!(msg.contains("requires manual approval"));
            }
            e => panic!("expected ToolPermission(manual approval), got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_policy_script_approval() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut registry = ToolRegistry::new(db.clone());

        // Policy that approves via "true" (always approves)
        let mut proxy = McpProxy::with_policy(ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "*".into(),
            action: PolicyAction::RequireApproval,
            approval: Some(crate::tools::policy::ApprovalMode::Script {
                command: "true".into(),
            }),
        }]));

        registry.register_tool(ToolDefinition {
            name: "some_tool".into(),
            description: "Test".into(),
            category: ToolCategory::Mcp,
            input_schema: serde_json::json!({}),
            mcp_server: Some("server".into()),
            enabled: true,
        });
        proxy
            .tool_routing
            .insert("some_tool".into(), "server".into());

        // Should pass policy (script approves) but fail at routing (no real server)
        let result = proxy
            .call_tool("atlas", "some_tool", serde_json::json!({}), &registry)
            .await;
        match result.unwrap_err() {
            Error::Tool(msg) => assert!(msg.contains("not connected")),
            e => panic!("expected Tool(not connected) after script approval, got: {e}"),
        }
    }

    #[tokio::test]
    async fn test_policy_script_denial() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = ToolRegistry::new(db.clone());

        // Policy that denies via "false" (always denies)
        let mut proxy = McpProxy::with_policy(ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "*".into(),
            action: PolicyAction::RequireApproval,
            approval: Some(crate::tools::policy::ApprovalMode::Script {
                command: "false".into(),
            }),
        }]));

        proxy.tool_routing.insert("tool".into(), "server".into());

        let result = proxy
            .call_tool("atlas", "tool", serde_json::json!({}), &registry)
            .await;
        match result.unwrap_err() {
            Error::ToolPermission(msg) => {
                assert!(msg.contains("denied by approval script"));
            }
            e => panic!("expected ToolPermission(approval denied), got: {e}"),
        }
    }
}
