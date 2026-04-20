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
    // Unified MCP server for all xpressclaw tools (tasks, memory, skills, apps).
    servers.insert(
        "xpressclaw".to_string(),
        McpServerConfig {
            server_type: "stdio".to_string(),
            command: Some("python3".to_string()),
            args: vec!["-u".into(), "/app/mcp_xpressclaw.py".into()],
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

/// Internal hook configuration. Always includes memory hooks.
/// Deserialization is skipped so YAML values are ignored — the
/// defaults always apply. Serialization is kept so the API can
/// report which hooks are active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(skip_deserializing, default = "default_before_hooks")]
    pub before_message: Vec<String>,
    #[serde(skip_deserializing, default = "default_after_hooks")]
    pub after_message: Vec<String>,
}

fn default_before_hooks() -> Vec<String> {
    vec!["memory_recall".to_string()]
}

fn default_after_hooks() -> Vec<String> {
    vec!["memory_remember".to_string()]
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            before_message: vec!["memory_recall".to_string()],
            after_message: vec!["memory_remember".to_string()],
        }
    }
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
    /// Harness image reference (ADR-023). Path to a local `.wasm`
    /// module, an OCI ref like `ghcr.io/xpressai/harnesses/pi:v0.1.0`
    /// (when task-10 phase 2 lands), or `None` to use the bundled
    /// fallback noop harness.
    pub image: Option<String>,
    pub model: Option<String>,
    /// Per-agent LLM provider/key/url override.
    #[serde(default)]
    pub llm: Option<AgentLlmConfig>,
    /// Human-friendly display name (e.g. "Avery (PA)").
    pub display_name: Option<String>,
    /// Short role title (e.g. "Personal Assistant").
    pub role_title: Option<String>,
    /// Longer description of what this agent does.
    pub responsibilities: Option<String>,
    /// Path or URL to avatar image.
    pub avatar: Option<String>,
    /// Raw system prompt. display_name, role_title, and responsibilities
    /// are prepended automatically when building the LLM messages.
    pub role: String,
    #[serde(default)]
    pub tools: Vec<String>,
    /// Skills available to this agent (names matching templates/skills/{name}/).
    #[serde(default)]
    pub skills: Vec<String>,
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
    /// Idle-task prompt. When set, the agent self-activates during idle
    /// periods with exponential backoff. The agent reads/writes a scratch
    /// pad at {data_dir}/{agent_id}/idle.md between cycles.
    pub idle_prompt: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            backend: "claude-sdk".to_string(),
            image: None,
            model: None,
            llm: None,
            display_name: None,
            role_title: None,
            responsibilities: None,
            avatar: None,
            role: String::new(),
            tools: Vec::new(),
            skills: Vec::new(),
            budget: None,
            rate_limit: None,
            wake_on: Vec::new(),
            container: HashMap::new(),
            hooks: HooksConfig::default(),
            volumes: Vec::new(),
            idle_prompt: None,
        }
    }
}

impl AgentConfig {
    /// Build the full system prompt by prepending profile fields
    /// (display_name, role_title, responsibilities) to the raw role.
    pub fn full_system_prompt(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref name) = self.display_name {
            parts.push(format!("Your name is {name}."));
        }
        if let Some(ref title) = self.role_title {
            parts.push(format!("Your role is: {title}."));
        }
        if let Some(ref resp) = self.responsibilities {
            parts.push(format!("Your responsibilities: {resp}"));
        }
        if parts.is_empty() {
            self.role.clone()
        } else {
            parts.push(String::new()); // blank line separator
            parts.push(self.role.clone());
            parts.join("\n")
        }
    }
}

/// Generate a URL-safe slug from a display name.
///
/// Handles Unicode (including Japanese) by:
/// 1. Lowercasing ASCII characters
/// 2. Replacing spaces/punctuation with hyphens
/// 3. Keeping alphanumeric ASCII and Unicode letters/numbers
/// 4. Collapsing multiple hyphens
/// 5. Trimming leading/trailing hyphens
///
/// If the result is empty (e.g. all emoji), falls back to "agent".
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_hyphen = true; // prevent leading hyphen

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if ch.is_alphanumeric() {
            // Keep non-ASCII letters/numbers (Japanese, etc.)
            slug.push(ch);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    // Trim trailing hyphen
    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}

/// Generate a unique agent ID from a display name, given existing IDs.
/// Appends a numeric suffix if the slug already exists.
pub fn unique_agent_id(display_name: &str, existing_ids: &[&str]) -> String {
    let base = slugify(display_name);

    if !existing_ids.contains(&base.as_str()) {
        return base;
    }

    // Append numeric suffix
    for i in 2.. {
        let candidate = format!("{base}-{i}");
        if !existing_ids.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!()
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
    /// Custom per-model pricing (per 1M tokens). Overrides built-in pricing.
    /// Useful for proxy services (relay, OpenRouter) with different rates.
    /// Example: `{ "xpress-qwen-3.5-27b": { "input": 0.50, "output": 2.00 } }`
    #[serde(default)]
    pub custom_pricing: HashMap<String, crate::llm::pricing::ModelPricing>,
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
            custom_pricing: HashMap::new(),
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
    ///
    /// If the YAML is corrupt, attempts to load from the `.yaml.bak` backup.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::ConfigNotFound {
                path: path.display().to_string(),
            });
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read config: {e}")))?;

        match serde_yaml::from_str::<Config>(&contents) {
            Ok(config) => {
                config.validate()?;
                // Save a backup of the known-good config
                let backup = path.with_extension("yaml.bak");
                let _ = std::fs::copy(path, backup);
                Ok(config)
            }
            Err(e) => {
                // Try backup
                let backup = path.with_extension("yaml.bak");
                if backup.exists() {
                    tracing::warn!(
                        error = %e,
                        "config file is corrupt, loading from backup"
                    );
                    let backup_contents = std::fs::read_to_string(&backup)
                        .map_err(|e2| Error::Config(format!("failed to read backup: {e2}")))?;
                    let config: Config = serde_yaml::from_str(&backup_contents)?;
                    config.validate()?;
                    // Restore the good config
                    let _ = std::fs::copy(&backup, path);
                    Ok(config)
                } else {
                    Err(Error::Config(format!("failed to parse config: {e}")))
                }
            }
        }
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
    ///
    /// Validates the round-trip: serializes to YAML, parses it back, and only
    /// writes to disk if the parse succeeds. This prevents corrupted YAML from
    /// being written (e.g. if a prompt contains unescaped YAML symbols).
    /// Writes to a temp file first and renames atomically to avoid partial writes.
    pub fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(self)?;

        // Verify round-trip before writing to disk
        serde_yaml::from_str::<Config>(&yaml).map_err(|e| {
            Error::Config(format!(
                "config would produce invalid YAML (not saving): {e}"
            ))
        })?;

        // Atomic write: temp file + rename
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml)
            .map_err(|e| Error::Config(format!("failed to write config: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| Error::Config(format!("failed to rename config: {e}")))?;
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
    backend: claude-sdk
    role: |
      You are a helpful AI assistant.

      ## CRITICAL: YOU HAVE ANTEROGRADE AMNESIA
      You cannot form new long-term memories naturally. After each conversation ends,
      you will forget everything unless you explicitly save it.

      - **Before starting work:** Use `search_memory` to recall relevant context
      - **During conversations:** Use `create_memory` IMMEDIATELY when you learn important facts
      - **Be proactive:** If someone tells you about themselves or their work, SAVE IT

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
    backend: claude-sdk
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

    #[test]
    fn test_japanese_prompt_round_trip() {
        let role = "あなたは日本語のアシスタントです。\n\n## 責任\n- タスクの管理\n- メールの返信\n- スケジュールの確認";
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "eri".to_string(),
            role: role.to_string(),
            ..Default::default()
        });

        let yaml = serde_yaml::to_string(&config).unwrap();
        eprintln!("=== YAML ===\n{yaml}");

        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            parsed.agents[0].role, role,
            "Japanese role round-trip failed"
        );
    }

    #[test]
    fn test_markdown_list_prompt_round_trip() {
        let role = "You are a helpful assistant.\n\n## Guidelines\n- Write clean code\n- Ask clarifying questions\n* Use bullet points\n  - Nested items too";
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "test".to_string(),
            role: role.to_string(),
            ..Default::default()
        });

        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            parsed.agents[0].role, role,
            "Markdown list round-trip failed"
        );
    }

    #[test]
    fn test_prompt_save_load_file() {
        let role = "パーソナルファイナンスアシスタント\n\n## 責任\n- 予算管理\n- 投資アドバイス";
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "eri".to_string(),
            role: role.to_string(),
            ..Default::default()
        });

        let path = std::env::temp_dir().join("xpressclaw-test-jp.yaml");
        config.save(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        eprintln!("=== FILE ===\n{contents}");

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.agents[0].role, role, "File round-trip failed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xclaw59_exact_prompt() {
        // Exact prompt from XCLAW-59 bug report
        let role = "あなたは徹底的に調査を行うリサーチアシスタントです。\n\n\
            あなたの仕事は、ユーザーが求めるトピックに関する情報を見つけ、統合し、整理することです。\n\
            また、調査結果が会話をまたいでも保持されるよう、メモリシステムを使って詳細なメモを記録します。\n\
            ガイドライン\n\n\
            - まず広く調査し、その後有望な情報について深掘りする\n\
            - 常に情報源を明示する\n\
            - 重要な発見はすぐにメモリに保存する\n\
            - 情報は構造化され、読みやすい形式で提示する\n\
            - 情報が古い、または信頼性に疑問がある場合はその旨を明示する";

        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "eri".to_string(),
            role: role.to_string(),
            ..Default::default()
        });

        let path = std::env::temp_dir().join("xpressclaw-test-xclaw59.yaml");
        config.save(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        eprintln!("=== XCLAW-59 YAML ===\n{contents}");

        let loaded = Config::load(&path).unwrap();
        assert_eq!(
            loaded.agents[0].role, role,
            "XCLAW-59 prompt round-trip failed"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("yaml.bak"));
    }

    #[test]
    fn test_slugify_ascii() {
        assert_eq!(slugify("My Agent"), "my-agent");
        assert_eq!(slugify("Code Reviewer"), "code-reviewer");
        assert_eq!(slugify("  hello  world  "), "hello-world");
        assert_eq!(slugify("agent!@#$%name"), "agent-name");
    }

    #[test]
    fn test_slugify_japanese() {
        assert_eq!(slugify("エリ"), "エリ");
        assert_eq!(slugify("パーソナルアシスタント"), "パーソナルアシスタント");
        assert_eq!(slugify("My エージェント"), "my-エージェント");
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "agent");
        assert_eq!(slugify("!!!"), "agent");
        assert_eq!(slugify("   "), "agent");
    }

    #[test]
    fn test_unique_agent_id() {
        assert_eq!(unique_agent_id("Atlas", &[]), "atlas");
        assert_eq!(unique_agent_id("Atlas", &["atlas"]), "atlas-2");
        assert_eq!(unique_agent_id("Atlas", &["atlas", "atlas-2"]), "atlas-3");
    }

    #[test]
    fn test_unique_agent_id_japanese() {
        assert_eq!(unique_agent_id("エリ", &[]), "エリ");
        assert_eq!(unique_agent_id("エリ", &["エリ"]), "エリ-2");
    }
}
