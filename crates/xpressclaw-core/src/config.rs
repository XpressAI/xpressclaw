use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::tools::policy::ToolPolicyRule;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub daily: Option<String>,
    pub monthly: Option<String>,
    pub per_task: Option<String>,
    pub on_exceeded: OnExceeded,
    pub fallback_model: String,
    pub warn_at_percent: u8,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily: None,
            monthly: None,
            per_task: None,
            on_exceeded: OnExceeded::Pause,
            fallback_model: "local".to_string(),
            warn_at_percent: 80,
        }
    }
}

impl BudgetConfig {
    /// Parse a dollar string like "$20.00" into cents as f64.
    pub fn daily_amount(&self) -> Option<f64> {
        self.daily.as_ref().and_then(|s| parse_dollar_amount(s))
    }

    pub fn monthly_amount(&self) -> Option<f64> {
        self.monthly.as_ref().and_then(|s| parse_dollar_amount(s))
    }

    pub fn per_task_amount(&self) -> Option<f64> {
        self.per_task.as_ref().and_then(|s| parse_dollar_amount(s))
    }
}

fn parse_dollar_amount(s: &str) -> Option<f64> {
    let cleaned = s.trim().trim_start_matches('$');
    cleaned.parse().ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OnExceeded {
    Pause,
    Alert,
    Degrade,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub tokens_per_minute: u32,
    pub concurrent_requests: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 60,
            tokens_per_minute: 100_000,
            concurrent_requests: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    pub enabled: bool,
    #[serde(default)]
    pub config: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    pub confirmation_required: bool,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            config: HashMap::new(),
            allowed_commands: Vec::new(),
            paths: Vec::new(),
            confirmation_required: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    #[serde(rename = "type")]
    pub server_type: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            server_type: "stdio".to_string(),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
        }
    }
}

/// MCP servers that every agent gets regardless of configuration.
/// Shell and filesystem are always available inside the container.
pub fn default_mcp_servers() -> HashMap<String, McpServerConfig> {
    let mut servers = HashMap::new();
    servers.insert(
        "shell".to_string(),
        McpServerConfig {
            server_type: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".into(), "@mako10k/mcp-shell-server".into()],
            ..Default::default()
        },
    );
    servers.insert(
        "filesystem".to_string(),
        McpServerConfig {
            server_type: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
                "/workspace".into(),
            ],
            ..Default::default()
        },
    );
    servers
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WakeOnConfig {
    pub schedule: Option<String>,
    pub event: Option<String>,
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub before_message: Vec<String>,
    #[serde(default)]
    pub after_message: Vec<String>,
}

/// Per-agent LLM override. When set, the agent uses this provider/key/url
/// instead of the global LLM config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentLlmConfig {
    /// Provider name: "openai", "anthropic", or "local".
    pub provider: Option<String>,
    /// API key for this agent (overrides global key for the chosen provider).
    pub api_key: Option<String>,
    /// Base URL for this agent (overrides global base URL).
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub name: String,
    pub backend: String,
    pub model: Option<String>,
    /// Per-agent LLM provider/key/url override.
    #[serde(default)]
    pub llm: Option<AgentLlmConfig>,
    pub role: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub budget: Option<BudgetConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub wake_on: Vec<WakeOnConfig>,
    #[serde(default)]
    pub container: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub volumes: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            backend: "generic".to_string(),
            model: None,
            llm: None,
            role: String::new(),
            tools: Vec::new(),
            budget: None,
            rate_limit: None,
            wake_on: Vec::new(),
            container: HashMap::new(),
            hooks: HooksConfig::default(),
            volumes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemConfig {
    pub isolation: String,
    pub budget: BudgetConfig,
    pub rate_limit: RateLimitConfig,
    pub data_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

impl Default for SystemConfig {
    fn default() -> Self {
        let home = dirs_home();
        Self {
            isolation: "docker".to_string(),
            budget: BudgetConfig::default(),
            rate_limit: RateLimitConfig::default(),
            data_dir: home.join(".xpressclaw"),
            workspace_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub near_term_slots: u8,
    pub eviction: String,
    pub retention: String,
    pub embedding_model: String,
    pub embedding_dim: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            near_term_slots: 8,
            eviction: "least-recently-relevant".to_string(),
            retention: "none".to_string(),
            embedding_model: "all-MiniLM-L6-v2".to_string(),
            embedding_dim: 384,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub default_provider: String,
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub local_model: Option<String>,
    pub local_model_path: Option<String>,
    /// Base URL for the local LLM server (Ollama, llama.cpp, vLLM, etc.).
    /// Defaults to Ollama's address (http://localhost:11434) if not set.
    pub local_base_url: Option<String>,
    pub context_length: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: "local".to_string(),
            openai_api_key: None,
            openai_base_url: None,
            anthropic_api_key: None,
            local_model: Some("qwen3.5:latest".to_string()),
            local_model_path: None,
            local_base_url: None,
            context_length: 32768,
        }
    }
}

/// Root configuration for xpressclaw.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub system: SystemConfig,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub tools: HashMap<String, ToolConfig>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// Pattern-based tool policy rules. Evaluated in order — first match wins.
    /// If no rule matches, the tool call is allowed by default.
    #[serde(default)]
    pub tool_policies: Vec<ToolPolicyRule>,
    pub memory: MemoryConfig,
    pub llm: LlmConfig,
}

impl Config {
    /// Load config from a YAML file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::ConfigNotFound {
                path: path.display().to_string(),
            });
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read config: {e}")))?;

        let config: Config = serde_yaml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Load config from the default location (./xpressclaw.yaml).
    pub fn load_default() -> Result<Self> {
        let path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("xpressclaw.yaml");

        if path.exists() {
            Self::load(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to a YAML file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml)
            .map_err(|e| Error::Config(format!("failed to write config: {e}")))?;
        Ok(())
    }

    /// Validate the config.
    fn validate(&self) -> Result<()> {
        let valid_isolation = ["docker", "none"];
        if !valid_isolation.contains(&self.system.isolation.as_str()) {
            return Err(Error::ConfigValidation(format!(
                "invalid isolation mode: {}. Valid: docker, none.",
                self.system.isolation
            )));
        }

        if self.memory.near_term_slots < 1 || self.memory.near_term_slots > 16 {
            return Err(Error::ConfigValidation(
                "near_term_slots must be between 1 and 16".to_string(),
            ));
        }

        Ok(())
    }
}

/// Environment variable overrides for secrets.
pub fn env_overrides(config: &mut Config) {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        config.llm.anthropic_api_key = Some(key);
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        config.llm.openai_api_key = Some(key);
    }
    if let Ok(url) = std::env::var("OPENAI_BASE_URL") {
        config.llm.openai_base_url = Some(url);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Default config template written by `xpressclaw init`.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# xpressclaw Configuration
# Generated by `xpressclaw init`
# Docs: https://xpressclaw.ai

# System-wide settings
system:
  # Container isolation for agents (docker required)
  isolation: docker

  # Budget controls
  budget:
    daily: $20.00
    on_exceeded: pause  # pause | alert | degrade | stop

# Agent definitions
agents:
  - name: atlas
    backend: generic
    role: |
      You are a helpful AI assistant.

      ## CRITICAL: YOU HAVE ANTEROGRADE AMNESIA
      You cannot form new long-term memories naturally. After each conversation ends,
      you will forget everything unless you explicitly save it.

      - **Before starting work:** Use `search_memory` to recall relevant context
      - **During conversations:** Use `create_memory` IMMEDIATELY when you learn important facts
      - **Be proactive:** If someone tells you about themselves or their work, SAVE IT

    hooks:
      before_message:
        - memory_recall
      after_message:
        - memory_remember

    # Volumes mounted into the agent container
    volumes:
      - ~/agent-workspace:/workspace

# Memory settings
memory:
  near_term_slots: 8
  eviction: least-recently-relevant

# LLM configuration
llm:
  default_provider: local
  # local_model: qwen3.5:latest
  # openai_api_key: (set OPENAI_API_KEY env var)
  # anthropic_api_key: (set ANTHROPIC_API_KEY env var)

# Tool policy rules (evaluated in order, first match wins)
# tool_policies:
#   - pattern: "dangerous_*"
#     action: deny
#   - pattern: "github__*"
#     action: allow
#   - pattern: "*"
#     action: require_approval
#     approval:
#       type: script
#       command: /usr/local/bin/approve-tool

# MCP (Model Context Protocol) servers
# mcp_servers:
#   github:
#     type: stdio
#     command: npx
#     args: ["-y", "@modelcontextprotocol/server-github"]
#     env:
#       GITHUB_PERSONAL_ACCESS_TOKEN: ${GITHUB_TOKEN}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.system.isolation, "docker");
        assert!(config.agents.is_empty());
        assert_eq!(config.memory.near_term_slots, 8);
    }

    #[test]
    fn test_parse_dollar_amount() {
        assert_eq!(parse_dollar_amount("$20.00"), Some(20.0));
        assert_eq!(parse_dollar_amount("20.00"), Some(20.0));
        assert_eq!(parse_dollar_amount("$0.50"), Some(0.50));
        assert_eq!(parse_dollar_amount("invalid"), None);
    }

    #[test]
    fn test_config_from_yaml() {
        let yaml = r#"
system:
  isolation: docker
  budget:
    daily: "$10.00"
agents:
  - name: test-agent
    backend: generic
    role: "Test role"
memory:
  near_term_slots: 4
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents[0].name, "test-agent");
        assert_eq!(config.memory.near_term_slots, 4);
        assert_eq!(config.system.budget.daily_amount(), Some(10.0));
    }

    #[test]
    fn test_validation_accepts_none_isolation() {
        let mut config = Config::default();
        config.system.isolation = "none".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_rejects_invalid_isolation() {
        let mut config = Config::default();
        config.system.isolation = "podman".to_string();
        assert!(config.validate().is_err());
    }
}
