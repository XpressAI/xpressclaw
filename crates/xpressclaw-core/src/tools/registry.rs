use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};

/// Categories of tools.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Filesystem,
    Shell,
    Web,
    Database,
    Mcp,
    Custom,
}

impl std::fmt::Display for ToolCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Filesystem => write!(f, "filesystem"),
            Self::Shell => write!(f, "shell"),
            Self::Web => write!(f, "web"),
            Self::Database => write!(f, "database"),
            Self::Mcp => write!(f, "mcp"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// Definition of a tool that agents can invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub category: ToolCategory,
    /// JSON Schema describing the input parameters.
    pub input_schema: serde_json::Value,
    /// Which MCP server provides this tool (if any).
    pub mcp_server: Option<String>,
    /// Whether the tool is currently enabled.
    pub enabled: bool,
}

/// Per-agent tool permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPermission {
    pub agent_id: String,
    pub tool_name: String,
    pub allowed: bool,
    /// For filesystem tools: allowed paths.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// For filesystem tools: denied paths.
    #[serde(default)]
    pub denied_paths: Vec<String>,
    /// For shell tools: allowed commands.
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// For shell tools: denied commands.
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// Whether user confirmation is required before execution.
    pub confirmation_required: bool,
}

impl Default for ToolPermission {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            tool_name: String::new(),
            allowed: true,
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            confirmation_required: false,
        }
    }
}

/// Record of a tool invocation for auditing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLog {
    pub id: i64,
    pub agent_id: String,
    pub tool_name: String,
    pub input_data: Option<String>,
    pub output_data: Option<String>,
    pub duration_ms: Option<i64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub timestamp: String,
}

/// Manages tool definitions, per-agent permissions, and execution logging.
pub struct ToolRegistry {
    db: Arc<Database>,
    /// In-memory tool definitions (populated from MCP servers and config).
    tools: HashMap<String, ToolDefinition>,
    /// Per-agent permissions (agent_id -> tool_name -> permission).
    permissions: HashMap<String, HashMap<String, ToolPermission>>,
}

impl ToolRegistry {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            tools: HashMap::new(),
            permissions: HashMap::new(),
        }
    }

    /// Register a tool definition.
    pub fn register_tool(&mut self, tool: ToolDefinition) {
        debug!(name = tool.name, category = %tool.category, "registered tool");
        self.tools.insert(tool.name.clone(), tool);
    }

    /// Unregister a tool by name.
    pub fn unregister_tool(&mut self, name: &str) -> Option<ToolDefinition> {
        self.tools.remove(name)
    }

    /// Get a tool definition by name.
    pub fn get_tool(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// List all registered tools, optionally filtered by category.
    pub fn list_tools(&self, category: Option<&ToolCategory>) -> Vec<&ToolDefinition> {
        self.tools
            .values()
            .filter(|t| {
                if let Some(cat) = category {
                    &t.category == cat
                } else {
                    true
                }
            })
            .filter(|t| t.enabled)
            .collect()
    }

    /// Set a permission for an agent on a specific tool.
    pub fn set_permission(&mut self, permission: ToolPermission) {
        debug!(
            agent_id = permission.agent_id,
            tool_name = permission.tool_name,
            allowed = permission.allowed,
            "set tool permission"
        );
        self.permissions
            .entry(permission.agent_id.clone())
            .or_default()
            .insert(permission.tool_name.clone(), permission);
    }

    /// Get the permission for an agent on a specific tool.
    pub fn get_permission(&self, agent_id: &str, tool_name: &str) -> Option<&ToolPermission> {
        self.permissions
            .get(agent_id)
            .and_then(|m| m.get(tool_name))
    }

    /// Check whether an agent is allowed to use a tool.
    ///
    /// If no explicit permission is set, the tool is allowed by default.
    pub fn is_tool_allowed(&self, agent_id: &str, tool_name: &str) -> bool {
        // Check if tool exists and is enabled
        if let Some(tool) = self.tools.get(tool_name) {
            if !tool.enabled {
                return false;
            }
        }

        // Check explicit permission
        match self.get_permission(agent_id, tool_name) {
            Some(perm) => perm.allowed,
            None => true, // default: allow
        }
    }

    /// Check if a command is allowed for an agent's shell tool usage.
    pub fn is_command_allowed(&self, agent_id: &str, command: &str) -> bool {
        // Extract the base command (first word)
        let base_cmd = command.split_whitespace().next().unwrap_or("");

        if let Some(perm) = self.get_permission(agent_id, "shell") {
            // Check denied list first
            if perm.denied_commands.iter().any(|c| c == base_cmd) {
                return false;
            }
            // If allowed list is non-empty, command must be in it
            if !perm.allowed_commands.is_empty() {
                return perm.allowed_commands.iter().any(|c| c == base_cmd);
            }
        }

        true
    }

    /// Check if a file path is allowed for an agent's filesystem tool usage.
    pub fn is_path_allowed(&self, agent_id: &str, path: &str) -> bool {
        if let Some(perm) = self.get_permission(agent_id, "filesystem") {
            // Check denied paths first
            if perm.denied_paths.iter().any(|p| path.starts_with(p)) {
                return false;
            }
            // If allowed paths are set, path must be under one of them
            if !perm.allowed_paths.is_empty() {
                return perm.allowed_paths.iter().any(|p| path.starts_with(p));
            }
        }

        true
    }

    /// Get all tool schemas in OpenAI function-calling format.
    ///
    /// This is used to build the tool list for LLM requests.
    pub fn get_tool_schemas(&self, agent_id: &str) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .filter(|t| t.enabled && self.is_tool_allowed(agent_id, &t.name))
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

    /// Log a tool invocation to the database.
    pub fn log_invocation(
        &self,
        agent_id: &str,
        tool_name: &str,
        input: Option<&str>,
        output: Option<&str>,
        duration_ms: Option<i64>,
        success: bool,
        error: Option<&str>,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tool_logs (agent_id, tool_name, input_data, output_data, duration_ms, success, error_message) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    agent_id,
                    tool_name,
                    input,
                    output,
                    duration_ms,
                    success as i32,
                    error,
                ],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Get tool invocation logs, optionally filtered by agent and/or tool name.
    pub fn get_logs(
        &self,
        agent_id: Option<&str>,
        tool_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ToolLog>> {
        self.db.with_conn(|conn| {
            let mut sql = "SELECT id, agent_id, tool_name, input_data, output_data, duration_ms, success, error_message, timestamp FROM tool_logs WHERE 1=1".to_string();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(aid) = agent_id {
                sql.push_str(" AND agent_id = ?");
                params.push(Box::new(aid.to_string()));
            }
            if let Some(tn) = tool_name {
                sql.push_str(" AND tool_name = ?");
                params.push(Box::new(tn.to_string()));
            }
            sql.push_str(" ORDER BY timestamp DESC LIMIT ?");
            params.push(Box::new(limit));

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| Error::Database(e.to_string()))?;

            let logs = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(ToolLog {
                        id: row.get(0)?,
                        agent_id: row.get(1)?,
                        tool_name: row.get(2)?,
                        input_data: row.get(3)?,
                        output_data: row.get(4)?,
                        duration_ms: row.get(5)?,
                        success: row.get::<_, i32>(6)? != 0,
                        error_message: row.get(7)?,
                        timestamp: row.get(8)?,
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(logs)
        })
    }

    /// Get the number of registered tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Get all tools provided by a specific MCP server.
    pub fn tools_from_server(&self, server_name: &str) -> Vec<&ToolDefinition> {
        self.tools
            .values()
            .filter(|t| t.mcp_server.as_deref() == Some(server_name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, ToolRegistry) {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = ToolRegistry::new(db.clone());
        (db, registry)
    }

    fn sample_tool(name: &str, category: ToolCategory) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("{name} tool"),
            category,
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }),
            mcp_server: None,
            enabled: true,
        }
    }

    #[test]
    fn test_register_and_list() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("read_file", ToolCategory::Filesystem));
        registry.register_tool(sample_tool("write_file", ToolCategory::Filesystem));
        registry.register_tool(sample_tool("execute_command", ToolCategory::Shell));

        assert_eq!(registry.tool_count(), 3);
        assert_eq!(registry.list_tools(None).len(), 3);
        assert_eq!(
            registry
                .list_tools(Some(&ToolCategory::Filesystem))
                .len(),
            2
        );
        assert_eq!(
            registry.list_tools(Some(&ToolCategory::Shell)).len(),
            1
        );
    }

    #[test]
    fn test_unregister() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("read_file", ToolCategory::Filesystem));
        assert_eq!(registry.tool_count(), 1);

        let removed = registry.unregister_tool("read_file");
        assert!(removed.is_some());
        assert_eq!(registry.tool_count(), 0);
    }

    #[test]
    fn test_disabled_tool_not_listed() {
        let (_, mut registry) = setup();

        let mut tool = sample_tool("disabled_tool", ToolCategory::Custom);
        tool.enabled = false;
        registry.register_tool(tool);

        assert_eq!(registry.tool_count(), 1);
        assert_eq!(registry.list_tools(None).len(), 0); // disabled not listed
    }

    #[test]
    fn test_permissions_default_allow() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("read_file", ToolCategory::Filesystem));

        // No explicit permission — should be allowed by default
        assert!(registry.is_tool_allowed("atlas", "read_file"));
    }

    #[test]
    fn test_permissions_deny() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("dangerous_tool", ToolCategory::Custom));
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "dangerous_tool".into(),
            allowed: false,
            ..Default::default()
        });

        assert!(!registry.is_tool_allowed("atlas", "dangerous_tool"));
        // Different agent still allowed (no explicit deny)
        assert!(registry.is_tool_allowed("hermes", "dangerous_tool"));
    }

    #[test]
    fn test_command_allowed() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("shell", ToolCategory::Shell));
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "shell".into(),
            allowed: true,
            allowed_commands: vec!["git".into(), "npm".into(), "python".into()],
            ..Default::default()
        });

        assert!(registry.is_command_allowed("atlas", "git status"));
        assert!(registry.is_command_allowed("atlas", "npm install"));
        assert!(!registry.is_command_allowed("atlas", "rm -rf /"));
        assert!(!registry.is_command_allowed("atlas", "sudo anything"));

        // No permission set for hermes — all allowed by default
        assert!(registry.is_command_allowed("hermes", "rm -rf /"));
    }

    #[test]
    fn test_command_denied() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("shell", ToolCategory::Shell));
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "shell".into(),
            allowed: true,
            denied_commands: vec!["rm".into(), "sudo".into()],
            ..Default::default()
        });

        assert!(registry.is_command_allowed("atlas", "git status"));
        assert!(!registry.is_command_allowed("atlas", "rm -rf /"));
        assert!(!registry.is_command_allowed("atlas", "sudo anything"));
    }

    #[test]
    fn test_path_allowed() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("filesystem", ToolCategory::Filesystem));
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "filesystem".into(),
            allowed: true,
            allowed_paths: vec!["/workspace".into(), "/tmp".into()],
            denied_paths: vec!["/workspace/.env".into()],
            ..Default::default()
        });

        assert!(registry.is_path_allowed("atlas", "/workspace/src/main.rs"));
        assert!(registry.is_path_allowed("atlas", "/tmp/output.txt"));
        assert!(!registry.is_path_allowed("atlas", "/etc/passwd"));
        assert!(!registry.is_path_allowed("atlas", "/workspace/.env"));
    }

    #[test]
    fn test_tool_schemas() {
        let (_, mut registry) = setup();

        registry.register_tool(sample_tool("read_file", ToolCategory::Filesystem));
        registry.register_tool(sample_tool("execute_command", ToolCategory::Shell));

        // Deny shell for atlas
        registry.set_permission(ToolPermission {
            agent_id: "atlas".into(),
            tool_name: "execute_command".into(),
            allowed: false,
            ..Default::default()
        });

        let schemas = registry.get_tool_schemas("atlas");
        assert_eq!(schemas.len(), 1); // only read_file
        assert_eq!(schemas[0]["function"]["name"], "read_file");

        // Hermes gets both
        let schemas = registry.get_tool_schemas("hermes");
        assert_eq!(schemas.len(), 2);
    }

    #[test]
    fn test_log_invocation() {
        let (_, registry) = setup();

        registry
            .log_invocation(
                "atlas",
                "read_file",
                Some(r#"{"path": "/workspace/main.rs"}"#),
                Some("file contents..."),
                Some(15),
                true,
                None,
            )
            .unwrap();

        registry
            .log_invocation(
                "atlas",
                "execute_command",
                Some(r#"{"command": "git status"}"#),
                None,
                Some(250),
                false,
                Some("command timed out"),
            )
            .unwrap();

        let logs = registry.get_logs(Some("atlas"), None, 10).unwrap();
        assert_eq!(logs.len(), 2);

        let logs = registry
            .get_logs(Some("atlas"), Some("read_file"), 10)
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].success);
        assert_eq!(logs[0].duration_ms, Some(15));
    }

    #[test]
    fn test_tools_from_server() {
        let (_, mut registry) = setup();

        let mut tool1 = sample_tool("search", ToolCategory::Mcp);
        tool1.mcp_server = Some("brave-search".into());
        registry.register_tool(tool1);

        let mut tool2 = sample_tool("fetch", ToolCategory::Mcp);
        tool2.mcp_server = Some("brave-search".into());
        registry.register_tool(tool2);

        registry.register_tool(sample_tool("read_file", ToolCategory::Filesystem));

        let server_tools = registry.tools_from_server("brave-search");
        assert_eq!(server_tools.len(), 2);

        let server_tools = registry.tools_from_server("nonexistent");
        assert_eq!(server_tools.len(), 0);
    }
}
