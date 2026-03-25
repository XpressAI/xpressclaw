use tracing::info;

use super::manager::{ContainerSpec, DockerManager, VolumeMount};
use crate::config::AgentConfig;
use crate::error::Result;

/// Known harness images.
pub const HARNESS_BASE: &str = "ghcr.io/xpressai/xpressclaw-harness-base:latest";
pub const HARNESS_GENERIC: &str = "ghcr.io/xpressai/xpressclaw-harness-generic:latest";
pub const HARNESS_CLAUDE_SDK: &str = "ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest";
pub const HARNESS_XAIBO: &str = "ghcr.io/xpressai/xpressclaw-harness-xaibo:latest";
pub const HARNESS_LANGCHAIN: &str = "ghcr.io/xpressai/xpressclaw-harness-langchain:latest";

/// All harness images for pulling.
pub const ALL_HARNESS_IMAGES: &[&str] = &[
    HARNESS_GENERIC,
    HARNESS_CLAUDE_SDK,
    HARNESS_XAIBO,
    HARNESS_LANGCHAIN,
];

/// Resolve a backend name to its harness image.
pub fn image_for_backend(backend: &str) -> &'static str {
    match backend {
        "claude-code" | "claude-sdk" | "claude" => HARNESS_CLAUDE_SDK,
        "xaibo" => HARNESS_XAIBO,
        "langchain" | "crewai" => HARNESS_LANGCHAIN,
        _ => HARNESS_GENERIC,
    }
}

/// Build a container spec for an agent based on its configuration.
///
/// Resolves the harness image, sets up environment variables (API keys, model,
/// agent identity), and configures volume mounts from the agent's config.
pub fn build_container_spec(
    agent: &AgentConfig,
    server_port: u16,
    anthropic_api_key: Option<&str>,
    openai_api_key: Option<&str>,
    openai_base_url: Option<&str>,
) -> ContainerSpec {
    build_container_spec_with_mcp(agent, server_port, anthropic_api_key, openai_api_key, openai_base_url, None)
}

pub fn build_container_spec_with_mcp(
    agent: &AgentConfig,
    server_port: u16,
    anthropic_api_key: Option<&str>,
    openai_api_key: Option<&str>,
    openai_base_url: Option<&str>,
    mcp_servers: Option<&std::collections::HashMap<String, crate::config::McpServerConfig>>,
) -> ContainerSpec {
    let image = image_for_backend(&agent.backend);

    let mut env = vec![
        format!("AGENT_ID={}", agent.name),
        format!("AGENT_NAME={}", agent.name),
        format!("AGENT_BACKEND={}", agent.backend),
    ];

    // Per-agent LLM overrides take precedence over global config.
    let agent_llm = agent.llm.as_ref();
    let effective_base_url = agent_llm
        .and_then(|l| l.base_url.as_deref())
        .or(openai_base_url);
    let effective_openai_key = agent_llm
        .and_then(|l| l.api_key.as_deref())
        .or(openai_api_key);
    let effective_anthropic_key = agent_llm
        .and_then(|l| {
            // Only use agent key for anthropic if agent provider is anthropic
            if l.provider.as_deref() == Some("anthropic") {
                l.api_key.as_deref()
            } else {
                None
            }
        })
        .or(anthropic_api_key);

    // LLM routing — harnesses call back to the server's built-in /v1/ router by default.
    // We set both the custom LLM_BASE_URL and the standard OPENAI_BASE_URL so that
    // any OpenAI-compatible SDK inside the container works out of the box.
    // Rewrite localhost URLs to host.docker.internal so they work inside containers.
    let llm_base_url = effective_base_url
        .map(rewrite_localhost_for_docker)
        .unwrap_or_else(|| format!("http://host.docker.internal:{server_port}/v1"));
    env.push(format!("LLM_BASE_URL={llm_base_url}"));
    env.push(format!("OPENAI_BASE_URL={llm_base_url}"));

    // Anthropic SDK appends /v1/messages to the base URL, so we must NOT include /v1 here.
    // The server exposes POST /v1/messages as an Anthropic-compatible endpoint.
    let anthropic_base_url = format!("http://host.docker.internal:{server_port}");
    env.push(format!("ANTHROPIC_BASE_URL={anthropic_base_url}"));

    if let Some(model) = &agent.model {
        env.push(format!("LLM_MODEL={model}"));
    }

    // API keys for harnesses that call cloud APIs directly.
    // Set placeholder keys when none are provided — SDKs refuse to start without them.
    if let Some(key) = effective_anthropic_key {
        env.push(format!("ANTHROPIC_API_KEY={key}"));
    } else {
        // Encode agent name in the placeholder key so the proxy can identify the agent.
        env.push(format!("ANTHROPIC_API_KEY=sk-ant-{}", agent.name));
    }
    if let Some(key) = effective_openai_key {
        env.push(format!("OPENAI_API_KEY={key}"));
        env.push(format!("LLM_API_KEY={key}"));
    } else {
        // Placeholder key — the server's /v1 endpoint doesn't require auth,
        // but OpenAI SDKs refuse to start without an API key set.
        env.push("OPENAI_API_KEY=sk-xpressclaw".to_string());
        env.push("LLM_API_KEY=sk-xpressclaw".to_string());
    }

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
    let volumes: Vec<VolumeMount> = agent
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
    }
}

/// Rewrite localhost/127.0.0.1 URLs to host.docker.internal so they
/// work inside Docker containers.
fn rewrite_localhost_for_docker(url: &str) -> String {
    url.replace("://localhost", "://host.docker.internal")
        .replace("://127.0.0.1", "://host.docker.internal")
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
    let images = [HARNESS_GENERIC];

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
        assert_eq!(image_for_backend("anything-else"), HARNESS_GENERIC);
        assert_eq!(image_for_backend(""), HARNESS_GENERIC);
    }

    #[test]
    fn test_build_container_spec_basic() {
        let agent = AgentConfig {
            name: "test-agent".to_string(),
            backend: "generic".to_string(),
            role: "Test role".to_string(),
            model: Some("gpt-4o".to_string()),
            ..Default::default()
        };

        let spec = build_container_spec(&agent, 6969, None, None, None);

        assert_eq!(spec.image, HARNESS_GENERIC);
        assert_eq!(spec.expose_port, Some(8080));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "AGENT_NAME=test-agent"));
        assert!(spec.environment.iter().any(|e| e == "LLM_MODEL=gpt-4o"));
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
        // Placeholder API keys when no real keys are provided
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "OPENAI_API_KEY=sk-xpressclaw"));
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

        let spec = build_container_spec(&agent, 6969, Some("sk-ant-123"), None, None);

        assert_eq!(spec.image, HARNESS_CLAUDE_SDK);
        assert_eq!(spec.volumes.len(), 2);
        assert_eq!(spec.volumes[0].source, "/home/user/code");
        assert_eq!(spec.volumes[0].target, "/workspace");
        assert!(!spec.volumes[0].read_only);
        assert_eq!(spec.volumes[1].source, "/tmp/data");
        assert_eq!(spec.volumes[1].target, "/data");
        assert!(spec.volumes[1].read_only);
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "ANTHROPIC_API_KEY=sk-ant-123"));
    }

    #[test]
    fn test_build_container_spec_with_api_keys() {
        let agent = AgentConfig::default();

        let spec = build_container_spec(
            &agent,
            6969,
            Some("ant-key"),
            Some("oai-key"),
            Some("https://api.openai.com/v1"),
        );

        assert!(spec
            .environment
            .iter()
            .any(|e| e == "ANTHROPIC_API_KEY=ant-key"));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "OPENAI_API_KEY=oai-key"));
        assert!(spec.environment.iter().any(|e| e == "LLM_API_KEY=oai-key"));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "LLM_BASE_URL=https://api.openai.com/v1"));
        assert!(spec
            .environment
            .iter()
            .any(|e| e == "OPENAI_BASE_URL=https://api.openai.com/v1"));
    }
}
