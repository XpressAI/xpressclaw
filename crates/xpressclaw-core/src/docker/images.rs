use tracing::info;

use super::manager::{ContainerSpec, DockerManager, VolumeMount};
use crate::config::AgentConfig;
use crate::error::Result;

/// Known harness images.
pub const HARNESS_BASE: &str = "ghcr.io/xpressai/xpressclaw-harness-base:latest";
pub const HARNESS_CLAUDE_SDK: &str = "ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest";
pub const HARNESS_CLAUDE_SDK_ANDROID: &str =
    "ghcr.io/xpressai/xpressclaw-harness-claude-sdk-android:latest";
pub const HARNESS_XAIBO: &str = "ghcr.io/xpressai/xpressclaw-harness-xaibo:latest";
pub const HARNESS_LANGCHAIN: &str = "ghcr.io/xpressai/xpressclaw-harness-langchain:latest";

/// All harness images for pulling.
pub const ALL_HARNESS_IMAGES: &[&str] = &[HARNESS_CLAUDE_SDK, HARNESS_XAIBO, HARNESS_LANGCHAIN];

/// Resolve a backend name to its harness image.
pub fn image_for_backend(backend: &str) -> &'static str {
    match backend {
        "claude-code" | "claude-sdk" | "claude" => HARNESS_CLAUDE_SDK,
        "xaibo" => HARNESS_XAIBO,
        "langchain" | "crewai" => HARNESS_LANGCHAIN,
        _ => HARNESS_CLAUDE_SDK,
    }
}

/// Returns true if the MCP server config invokes scrcpy-mcp (Android control).
fn uses_scrcpy_mcp(server: &crate::config::McpServerConfig) -> bool {
    server.args.iter().any(|a| a.contains("scrcpy-mcp"))
}

/// Resolve the harness image for an agent. If any of the agent's MCP
/// servers uses scrcpy-mcp, select the Android-enabled variant of the
/// claude-sdk image (which has adb + scrcpy baked in). Otherwise fall
/// back to the regular per-backend lookup.
///
/// This is the implicit opt-in mechanism: enabling the scrcpy MCP server
/// in an agent's template (via the `android-pilot` preset) automatically
/// routes that agent to the heavier image. Users who never enable Android
/// pay zero size cost.
pub fn image_for_agent(
    backend: &str,
    mcp_servers: Option<&std::collections::HashMap<String, crate::config::McpServerConfig>>,
) -> &'static str {
    let base = image_for_backend(backend);
    if base == HARNESS_CLAUDE_SDK {
        if let Some(servers) = mcp_servers {
            if servers.values().any(uses_scrcpy_mcp) {
                return HARNESS_CLAUDE_SDK_ANDROID;
            }
        }
    }
    base
}

/// Build a container spec for an agent based on its configuration.
///
/// The harness inside the container always calls back to the server's `/v1/`
/// proxy. Real upstream API keys never leave the server — agent identity is
/// encoded in placeholder keys so the proxy can resolve per-agent providers.
pub fn build_container_spec(agent: &AgentConfig, server_port: u16) -> ContainerSpec {
    build_container_spec_with_mcp(agent, server_port, None)
}

pub fn build_container_spec_with_mcp(
    agent: &AgentConfig,
    server_port: u16,
    mcp_servers: Option<&std::collections::HashMap<String, crate::config::McpServerConfig>>,
) -> ContainerSpec {
    let image = image_for_agent(&agent.backend, mcp_servers);

    let mut env = vec![
        format!("AGENT_ID={}", agent.name),
        format!("AGENT_NAME={}", agent.name),
        format!("AGENT_BACKEND={}", agent.backend),
        format!("XPRESSCLAW_PORT={server_port}"),
        "HOME=/workspace".to_string(),
        "WORKSPACE_DIR=/workspace".to_string(),
    ];

    // The harness always calls back to the server's /v1/ proxy. The server
    // holds per-agent provider config and dispatches to the right upstream.
    // OPENAI_BASE_URL mirrors LLM_BASE_URL for OpenAI-SDK-flavored harnesses.
    let llm_base_url = format!("http://host.docker.internal:{server_port}/v1");
    env.push(format!("LLM_BASE_URL={llm_base_url}"));
    env.push(format!("OPENAI_BASE_URL={llm_base_url}"));

    // Anthropic SDK appends /v1/messages to the base URL, so we must NOT include /v1 here.
    // The server exposes POST /v1/messages as an Anthropic-compatible endpoint.
    let anthropic_base_url = format!("http://host.docker.internal:{server_port}");
    env.push(format!("ANTHROPIC_BASE_URL={anthropic_base_url}"));

    // The harness uses the agent's logical name as the model identifier.
    // The server's LlmRouter resolves it to the real (provider, model) pair,
    // so budget controls can re-point an agent at a cheaper model at runtime
    // without touching env vars or restarting the container.
    env.push(format!("LLM_MODEL={}", agent.name));

    // Placeholder API keys — the server's /v1 endpoint doesn't authenticate,
    // but cloud SDKs refuse to start without something in these vars. We
    // encode the agent_id so the proxy can route requests per-agent.
    env.push(format!("ANTHROPIC_API_KEY=sk-ant-{}", agent.name));
    env.push(format!("OPENAI_API_KEY=sk-xpressclaw-{}", agent.name));
    env.push(format!("LLM_API_KEY=sk-xpressclaw-{}", agent.name));

    // Agent role as JSON config
    if !agent.role.is_empty() {
        if let Ok(json) = serde_json::to_string(&serde_json::json!({
            "role": agent.role,
            "tools": agent.tools,
        })) {
            env.push(format!("AGENT_CONFIG={json}"));
        }
    }

    // MCP servers — merge defaults with config-provided servers and inject as JSON env var.
    // Agents need these to call tasks, apps, memory, etc.
    if let Some(servers) = mcp_servers {
        let mut all_servers = crate::config::default_mcp_servers();
        for (name, cfg) in servers {
            all_servers.insert(name.clone(), cfg.clone());
        }
        if let Ok(json) = serde_json::to_string(&all_servers) {
            env.push(format!("MCP_SERVERS={json}"));
        }
    } else {
        // No servers provided — still inject defaults
        let defaults = crate::config::default_mcp_servers();
        if let Ok(json) = serde_json::to_string(&defaults) {
            env.push(format!("MCP_SERVERS={json}"));
        }
    }

    // Volume mounts from agent config (format: "host_path:container_path" or "host_path:container_path:ro")
    let mut volumes: Vec<VolumeMount> = agent
        .volumes
        .iter()
        .filter_map(|v| {
            let parts: Vec<&str> = v.split(':').collect();
            if parts.len() >= 2 {
                let source = expand_tilde(parts[0]);
                Some(VolumeMount {
                    source,
                    target: parts[1].to_string(),
                    read_only: parts.get(2).is_some_and(|&s| s == "ro"),
                })
            } else {
                None
            }
        })
        .collect();

    // Add a named volume for /workspace if the user hasn't already mounted something there.
    // This lets agents and their app containers share files via a Docker named volume.
    let has_workspace_mount = volumes.iter().any(|v| v.target == "/workspace");
    if !has_workspace_mount {
        let workspace_vol = format!("xpressclaw-workspace-{}", agent.name);
        volumes.push(VolumeMount {
            source: workspace_vol,
            target: "/workspace".to_string(),
            read_only: false,
        });
    }

    // Memory/CPU limits from agent container config
    let memory_limit = agent.container.get("memory_limit").and_then(|v| v.as_i64());
    let cpu_limit = agent.container.get("cpu_limit").and_then(|v| v.as_i64());

    ContainerSpec {
        image: image.to_string(),
        memory_limit: memory_limit.or(Some(2 * 1024 * 1024 * 1024)),
        cpu_limit,
        environment: env,
        volumes,
        network_mode: Some("bridge".to_string()),
        expose_port: Some(8080),
        cmd: None,
        working_dir: None,
    }
}

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

/// Pull all default harness images.
pub async fn pull_defaults(docker: &DockerManager) -> Result<()> {
    let images = [HARNESS_CLAUDE_SDK];

    for image in images {
        info!(image, "pulling default harness image");
        docker.pull_image(image).await?;
    }

    Ok(())
}

/// Pull a specific harness image for a backend.
pub async fn pull_for_backend(docker: &DockerManager, backend: &str) -> Result<()> {
    let image = image_for_backend(backend);
    info!(image, backend, "pulling harness image for backend");
    docker.pull_image(image).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_for_backend_claude() {
        assert_eq!(image_for_backend("claude-code"), HARNESS_CLAUDE_SDK);
        assert_eq!(image_for_backend("claude-sdk"), HARNESS_CLAUDE_SDK);
        assert_eq!(image_for_backend("claude"), HARNESS_CLAUDE_SDK);
    }

    #[test]
    fn test_image_for_backend_langchain() {
        assert_eq!(image_for_backend("langchain"), HARNESS_LANGCHAIN);
        assert_eq!(image_for_backend("crewai"), HARNESS_LANGCHAIN);
    }

    #[test]
    fn test_image_for_backend_xaibo() {
        assert_eq!(image_for_backend("xaibo"), HARNESS_XAIBO);
    }

    #[test]
    fn test_image_for_backend_fallback() {
        assert_eq!(image_for_backend("anything-else"), HARNESS_CLAUDE_SDK);
        assert_eq!(image_for_backend(""), HARNESS_CLAUDE_SDK);
    }

    #[test]
    fn test_image_for_agent_without_scrcpy_stays_default() {
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "shell".to_string(),
            crate::config::McpServerConfig {
                command: Some("npx".to_string()),
                args: vec!["-y".into(), "@mako10k/mcp-shell-server".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            image_for_agent("claude-sdk", Some(&servers)),
            HARNESS_CLAUDE_SDK
        );
    }

    #[test]
    fn test_image_for_agent_with_scrcpy_picks_android_variant() {
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "scrcpy".to_string(),
            crate::config::McpServerConfig {
                command: Some("npx".to_string()),
                args: vec!["-y".into(), "scrcpy-mcp".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            image_for_agent("claude-sdk", Some(&servers)),
            HARNESS_CLAUDE_SDK_ANDROID
        );
    }

    #[test]
    fn test_image_for_agent_non_claude_backend_ignores_scrcpy() {
        // The android variant is built on the claude-sdk harness; non-claude
        // backends keep their own images even if they reference scrcpy-mcp.
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "scrcpy".to_string(),
            crate::config::McpServerConfig {
                command: Some("npx".to_string()),
                args: vec!["-y".into(), "scrcpy-mcp".into()],
                ..Default::default()
            },
        );
        assert_eq!(image_for_agent("xaibo", Some(&servers)), HARNESS_XAIBO);
    }

    #[test]
    fn test_build_container_spec_basic() {
        let agent = AgentConfig {
            name: "test-agent".to_string(),
            backend: "claude-sdk".to_string(),
            role: "Test role".to_string(),
            llm: Some(crate::config::AgentLlmConfig {
                provider: Some("openai".into()),
                model: Some("gpt-4o".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let spec = build_container_spec(&agent, 6969);

        assert_eq!(spec.image, HARNESS_CLAUDE_SDK);
        assert_eq!(spec.expose_port, Some(8080));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "AGENT_NAME=test-agent"));
        // The harness uses the agent's logical name as the model — the
        // server's LlmRouter resolves to the real upstream model.
        assert!(spec.environment.iter().any(|e| e == "LLM_MODEL=test-agent"));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "LLM_BASE_URL=http://host.docker.internal:6969/v1"));
        // OPENAI_BASE_URL mirrors LLM_BASE_URL for SDK compatibility
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "OPENAI_BASE_URL=http://host.docker.internal:6969/v1"));
        // ANTHROPIC_BASE_URL does NOT include /v1 — the SDK appends it
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "ANTHROPIC_BASE_URL=http://host.docker.internal:6969"));
        // Placeholder API keys encode the agent_id so the proxy can route per-agent.
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "OPENAI_API_KEY=sk-xpressclaw-test-agent"));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "ANTHROPIC_API_KEY=sk-ant-test-agent"));
    }

    #[test]
    fn test_build_container_spec_with_volumes() {
        let agent = AgentConfig {
            name: "worker".to_string(),
            backend: "claude-sdk".to_string(),
            volumes: vec![
                "/home/user/code:/workspace".to_string(),
                "/tmp/data:/data:ro".to_string(),
            ],
            ..Default::default()
        };

        let spec = build_container_spec(&agent, 6969);

        assert_eq!(spec.image, HARNESS_CLAUDE_SDK);
        assert_eq!(spec.volumes.len(), 2);
        assert_eq!(spec.volumes[0].source, "/home/user/code");
        assert_eq!(spec.volumes[0].target, "/workspace");
        assert!(!spec.volumes[0].read_only);
        assert_eq!(spec.volumes[1].source, "/tmp/data");
        assert_eq!(spec.volumes[1].target, "/data");
        assert!(spec.volumes[1].read_only);
        // Real API keys never leave the server — even when the agent has them
        // configured. The container only sees agent-id-encoded placeholders.
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "ANTHROPIC_API_KEY=sk-ant-worker"));
    }

    #[test]
    fn test_build_container_spec_no_real_keys_in_container() {
        // Even if the agent's LLM config has a real API key, it must not be
        // exposed to the container — the server is the single source of truth
        // for upstream credentials.
        let agent = AgentConfig {
            name: "secured".to_string(),
            backend: "claude-sdk".to_string(),
            llm: Some(crate::config::AgentLlmConfig {
                provider: Some("anthropic".into()),
                model: Some("claude-sonnet-4-20250514".into()),
                api_key: Some("sk-ant-REAL-SECRET".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let spec = build_container_spec(&agent, 6969);

        let key_line = spec
            .environment
            .iter()
            .find(|e| e.starts_with("ANTHROPIC_API_KEY="))
            .expect("ANTHROPIC_API_KEY should always be set");
        assert!(
            !key_line.contains("REAL-SECRET"),
            "Real API key leaked into container env: {key_line}"
        );
        assert_eq!(key_line, "ANTHROPIC_API_KEY=sk-ant-secured");
    }
}
