//! Built-in Agent Client Protocol product catalog.
//!
//! Product metadata lives here so setup, readiness checks, and the worker
//! dispatcher cannot drift onto different image names, commands, or standard
//! host configuration locations.

use serde::Serialize;

use crate::config::ContainerEngineAccess;

/// Release builds set this to the immutable runner-image commit they tested.
/// Developer builds retain the convenient `latest` default.
pub const RUNNER_IMAGE_TAG: &str = env!("XPRESSCLAW_RUNNER_TAG");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AcpAuthMount {
    /// Path relative to the host user's home directory.
    pub source: &'static str,
    /// Matching path inside the Linux runner image.
    pub target: &'static str,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AcpAgentDefinition {
    pub kind: &'static str,
    pub name: &'static str,
    pub mark: &'static str,
    pub description: &'static str,
    pub command: &'static [&'static str],
    /// Host executables whose presence indicates that the user has installed
    /// the product. These are never executed during detection.
    pub host_executables: &'static [&'static str],
    pub login_command: &'static str,
    pub install_url: &'static str,
    pub minimal_image: &'static str,
    pub host_image: &'static str,
    pub local_minimal_image: &'static str,
    pub local_host_image: &'static str,
    pub auth_mounts: &'static [AcpAuthMount],
}

const CODEX_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".codex",
    target: "/home/node/.codex",
    read_only: false,
}];

const CLAUDE_AUTH: &[AcpAuthMount] = &[
    AcpAuthMount {
        source: ".claude",
        target: "/home/node/.claude",
        read_only: false,
    },
    AcpAuthMount {
        source: ".claude.json",
        target: "/home/node/.claude.json",
        read_only: false,
    },
];

const COPILOT_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".copilot",
    target: "/home/node/.copilot",
    read_only: false,
}];

const DEEPSEEK_HARNESS_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".dsh",
    target: "/home/node/.dsh",
    read_only: false,
}];

const JUNIE_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".junie",
    target: "/home/node/.junie",
    read_only: false,
}];

const KIMI_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".kimi",
    target: "/home/node/.kimi",
    read_only: false,
}];

const OPENCODE_AUTH: &[AcpAuthMount] = &[
    AcpAuthMount {
        source: ".local/share/opencode",
        target: "/home/node/.local/share/opencode",
        read_only: false,
    },
    AcpAuthMount {
        source: ".config/opencode",
        target: "/home/node/.config/opencode",
        read_only: false,
    },
];

const PI_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".pi",
    target: "/home/node/.pi",
    read_only: false,
}];

const QWEN_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".qwen",
    target: "/home/node/.qwen",
    read_only: false,
}];

const CLINE_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".cline",
    target: "/home/node/.cline",
    read_only: false,
}];

const CURSOR_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".cursor",
    target: "/home/node/.cursor",
    read_only: false,
}];

const GLM_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".config/glm-acp-agent",
    target: "/home/node/.config/glm-acp-agent",
    read_only: false,
}];

const GROK_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".grok",
    target: "/home/node/.grok",
    read_only: false,
}];

const KILO_AUTH: &[AcpAuthMount] = &[
    AcpAuthMount {
        source: ".config/kilo",
        target: "/home/node/.config/kilo",
        read_only: false,
    },
    AcpAuthMount {
        source: ".local/share/kilo",
        target: "/home/node/.local/share/kilo",
        read_only: false,
    },
];

const VIBE_AUTH: &[AcpAuthMount] = &[AcpAuthMount {
    source: ".vibe",
    target: "/home/node/.vibe",
    read_only: false,
}];

macro_rules! agent {
    (
        $kind:literal, $name:literal, $mark:literal, $description:literal,
        $command:expr, $host_executables:expr, $login_command:literal,
        $install_url:literal, $auth_mounts:expr
    ) => {
        AcpAgentDefinition {
            kind: $kind,
            name: $name,
            mark: $mark,
            description: $description,
            command: $command,
            host_executables: $host_executables,
            login_command: $login_command,
            install_url: $install_url,
            minimal_image: concat!(
                "ghcr.io/xpressai/xpressclaw-runner-",
                $kind,
                ":",
                env!("XPRESSCLAW_RUNNER_TAG")
            ),
            host_image: concat!(
                "ghcr.io/xpressai/xpressclaw-runner-",
                $kind,
                "-docker:",
                env!("XPRESSCLAW_RUNNER_TAG")
            ),
            local_minimal_image: concat!("xpressclaw-runner-", $kind, ":latest"),
            local_host_image: concat!("xpressclaw-runner-", $kind, "-docker:latest"),
            auth_mounts: $auth_mounts,
        }
    };
}

pub const ACP_AGENTS: &[AcpAgentDefinition] = &[
    agent!(
        "claude",
        "Claude Agent",
        "A",
        "Anthropic's Claude Code through the official ACP adapter.",
        &["claude-agent-acp"],
        &["claude"],
        "claude",
        "https://docs.anthropic.com/en/docs/claude-code/setup",
        CLAUDE_AUTH
    ),
    agent!(
        "codex",
        "Codex",
        "C",
        "OpenAI Codex through the official ACP adapter.",
        &["codex-acp"],
        &["codex"],
        "codex login",
        "https://developers.openai.com/codex/cli/",
        CODEX_AUTH
    ),
    agent!(
        "deepseek-harness",
        "DeepSeek Harness",
        "DS",
        "DeepSeek Harness through openma-ai's maintained ACP adapter.",
        &["dsh-acp"],
        &["dsh-acp", "dsh"],
        "dsh-acp login",
        "https://github.com/openma-ai/deepseek-harness-acp",
        DEEPSEEK_HARNESS_AUTH
    ),
    agent!(
        "github-copilot",
        "GitHub Copilot",
        "GH",
        "GitHub Copilot CLI's built-in ACP server.",
        &["copilot", "--acp"],
        &["copilot"],
        "copilot login",
        "https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli",
        COPILOT_AUTH
    ),
    agent!(
        "junie",
        "Junie",
        "J",
        "JetBrains Junie in ACP mode.",
        &["junie", "--acp=true"],
        &["junie"],
        "junie",
        "https://www.jetbrains.com/help/junie/installation.html",
        JUNIE_AUTH
    ),
    agent!(
        "kimi",
        "Kimi CLI",
        "K",
        "Moonshot AI's Kimi coding agent in ACP mode.",
        &["kimi", "acp"],
        &["kimi"],
        "kimi login",
        "https://github.com/MoonshotAI/kimi-cli",
        KIMI_AUTH
    ),
    agent!(
        "opencode",
        "OpenCode",
        "O",
        "The open source OpenCode agent's built-in ACP server.",
        &["opencode", "acp"],
        &["opencode"],
        "opencode auth login",
        "https://opencode.ai/docs/",
        OPENCODE_AUTH
    ),
    agent!(
        "pi",
        "pi ACP",
        "π",
        "Pi coding agent with XpressClaw MCP integration.",
        &["pi-acp"],
        &["pi", "pi-acp"],
        "pi-acp --terminal-login",
        "https://github.com/svkozak/pi-acp",
        PI_AUTH
    ),
    agent!(
        "qwen",
        "Qwen Code",
        "Q",
        "Alibaba's Qwen Code agent in ACP mode.",
        &["qwen", "--acp", "--experimental-skills"],
        &["qwen"],
        "qwen",
        "https://qwenlm.github.io/qwen-code-docs/en/users/overview/",
        QWEN_AUTH
    ),
    agent!(
        "cline",
        "Cline",
        "CL",
        "Cline CLI's built-in ACP server.",
        &["cline", "--acp"],
        &["cline"],
        "cline auth",
        "https://docs.cline.bot/cline-cli/installation",
        CLINE_AUTH
    ),
    agent!(
        "cursor",
        "Cursor",
        "CU",
        "Cursor Agent's built-in ACP server.",
        &["cursor-agent", "acp"],
        &["cursor-agent"],
        "cursor-agent login",
        "https://cursor.com/docs/cli/installation",
        CURSOR_AUTH
    ),
    agent!(
        "glm",
        "GLM Agent",
        "G",
        "Zhipu AI's GLM coding agent ACP server.",
        &["glm-acp-agent"],
        &["glm-acp-agent"],
        "glm-acp-agent --setup",
        "https://github.com/zai-org/glm-acp-agent",
        GLM_AUTH
    ),
    agent!(
        "grok",
        "Grok Build",
        "X",
        "xAI's Grok Build agent over ACP stdio.",
        &["grok", "agent", "stdio"],
        &["grok"],
        "grok login",
        "https://github.com/xai-org/grok-cli",
        GROK_AUTH
    ),
    agent!(
        "kilo",
        "Kilo Code",
        "KI",
        "Kilo Code CLI's built-in ACP server.",
        &["kilo", "acp"],
        &["kilo"],
        "kilo auth login",
        "https://kilo.ai/docs/code-with-ai/platforms/cli",
        KILO_AUTH
    ),
    agent!(
        "mistral-vibe",
        "Mistral Vibe",
        "M",
        "Mistral's Vibe coding agent through its ACP executable.",
        &["vibe-acp"],
        &["vibe", "vibe-acp"],
        "vibe --setup",
        "https://docs.mistral.ai/mistral-vibe/introduction",
        VIBE_AUTH
    ),
];

pub fn agent_definition(kind: &str) -> Option<&'static AcpAgentDefinition> {
    ACP_AGENTS.iter().find(|agent| agent.kind == kind)
}

/// Resolve an exact configured runner kind or supported product alias.
///
/// Keep this exact: callers use the result to decide which sensitive host
/// configuration directories may be mounted into a runner container.
pub fn canonical_agent_kind(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    if let Some(agent) = ACP_AGENTS.iter().find(|agent| normalized == agent.kind) {
        return Some(agent.kind);
    }
    match normalized.as_str() {
        "copilot" => Some("github-copilot"),
        "deepseek"
        | "dsh"
        | "dsh-acp"
        | "deepseek-harness-acp"
        | "@openma/deepseek-harness-acp" => Some("deepseek-harness"),
        "pi-acp" => Some("pi"),
        _ => None,
    }
}

/// Infer a built-in product from a legacy backend label.
///
/// Old configurations used backend strings such as package names instead of
/// catalog IDs, so this path deliberately retains fuzzy matching. Never use it
/// for an explicit `runner.kind`: doing so could grant a custom runner a
/// built-in product's host credential mounts.
pub fn infer_agent_kind_from_backend(value: &str) -> Option<&'static str> {
    canonical_agent_kind(value).or_else(|| {
        let normalized = value.trim().to_ascii_lowercase();
        ACP_AGENTS
            .iter()
            .find(|agent| {
                (agent.kind.len() >= 4 && normalized.contains(agent.kind))
                    || (agent.kind == "github-copilot" && normalized.contains("copilot"))
            })
            .map(|agent| agent.kind)
    })
}

pub fn default_runner_image(
    kind: &str,
    container_engine: ContainerEngineAccess,
) -> Option<&'static str> {
    let agent = agent_definition(kind)?;
    Some(match container_engine {
        ContainerEngineAccess::None => agent.minimal_image,
        ContainerEngineAccess::Host => agent.host_image,
    })
}

/// Mutable publication tag used only when an operator explicitly asks to
/// update a built-in runner. Release defaults remain on the immutable tag
/// compiled into the application.
pub fn latest_runner_image(kind: &str, container_engine: ContainerEngineAccess) -> Option<String> {
    let image = default_runner_image(kind, container_engine)?;
    let (repository, _) = image.rsplit_once(':')?;
    Some(format!("{repository}:latest"))
}

pub fn local_runner_image(image: &str) -> Option<&'static str> {
    local_runner_image_for_tag(image, RUNNER_IMAGE_TAG)
}

fn local_runner_image_for_tag(image: &str, runner_image_tag: &str) -> Option<&'static str> {
    if runner_image_tag != "latest" {
        return None;
    }
    ACP_AGENTS.iter().find_map(|agent| {
        if managed_image_matches(image, agent.minimal_image) {
            Some(agent.local_minimal_image)
        } else if managed_image_matches(image, agent.host_image) {
            Some(agent.local_host_image)
        } else {
            None
        }
    })
}

pub fn is_builtin_runner_image(image: &str) -> bool {
    ACP_AGENTS.iter().any(|agent| {
        managed_image_matches(image, agent.minimal_image)
            || managed_image_matches(image, agent.host_image)
            || published_digest_matches(image, agent.minimal_image)
            || published_digest_matches(image, agent.host_image)
            || managed_image_matches(image, agent.local_minimal_image)
            || managed_image_matches(image, agent.local_host_image)
    })
}

/// Whether an image belongs to the built-in product's published repositories.
/// Explicit digest pins count as built-in for compatibility enforcement, but
/// are not managed defaults and therefore are not rewritten on app upgrades.
pub fn is_builtin_runner_image_for_kind(image: &str, kind: &str) -> bool {
    agent_definition(kind).is_some_and(|agent| {
        managed_image_matches(image, agent.minimal_image)
            || managed_image_matches(image, agent.host_image)
            || published_digest_matches(image, agent.minimal_image)
            || published_digest_matches(image, agent.host_image)
            || managed_image_matches(image, agent.local_minimal_image)
            || managed_image_matches(image, agent.local_host_image)
    })
}

pub fn is_host_runner_image(image: &str) -> bool {
    ACP_AGENTS.iter().any(|agent| {
        managed_image_matches(image, agent.host_image)
            || published_digest_matches(image, agent.host_image)
            || managed_image_matches(image, agent.local_host_image)
    })
}

/// Whether an image is one of the managed images for a particular product.
///
/// Full commit tags from older releases and the legacy `latest` tag are
/// treated as defaults, so upgrading XpressClaw advances existing Agents to
/// the runner revision tested with the new release. Other tags and digests
/// remain explicit user pins.
pub fn is_managed_runner_image_for_kind(image: &str, kind: &str) -> bool {
    agent_definition(kind).is_some_and(|agent| {
        managed_image_matches(image, agent.minimal_image)
            || managed_image_matches(image, agent.host_image)
            || managed_image_matches(image, agent.local_minimal_image)
            || managed_image_matches(image, agent.local_host_image)
    })
}

fn managed_image_matches(candidate: &str, current: &str) -> bool {
    if candidate == current {
        return true;
    }
    let (candidate_repository, candidate_tag) = match candidate.rsplit_once(':') {
        Some(parts) => parts,
        None => return false,
    };
    let (current_repository, _) = match current.rsplit_once(':') {
        Some(parts) => parts,
        None => return false,
    };
    candidate_repository == current_repository
        && (candidate_tag == "latest"
            || (candidate_tag.len() == 40
                && candidate_tag
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())))
}

fn published_digest_matches(candidate: &str, current: &str) -> bool {
    let (candidate_repository, digest) = match candidate.rsplit_once("@sha256:") {
        Some(parts) => parts,
        None => return false,
    };
    let (current_repository, _) = match current.rsplit_once(':') {
        Some(parts) => parts,
        None => return false,
    };
    candidate_repository == current_repository
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn catalog_ids_and_images_are_unique() {
        let mut kinds = HashSet::new();
        let mut images = HashSet::new();
        for agent in ACP_AGENTS {
            assert!(kinds.insert(agent.kind), "duplicate kind {}", agent.kind);
            assert!(images.insert(agent.minimal_image));
            assert!(images.insert(agent.host_image));
            assert!(!agent.command.is_empty());
            assert!(!agent.host_executables.is_empty());
        }
    }

    #[test]
    fn resolves_published_and_local_images() {
        let codex = agent_definition("codex").unwrap();
        assert!(codex.minimal_image.ends_with(RUNNER_IMAGE_TAG));
        assert_eq!(
            default_runner_image("codex", ContainerEngineAccess::Host),
            Some(codex.host_image)
        );
        assert_eq!(
            local_runner_image_for_tag(codex.minimal_image, "latest"),
            Some(codex.local_minimal_image)
        );
        assert_eq!(
            local_runner_image_for_tag(
                codex.minimal_image,
                "0123456789abcdef0123456789abcdef01234567"
            ),
            None
        );
        assert!(is_builtin_runner_image(codex.local_host_image));
        assert!(is_host_runner_image(codex.host_image));
        assert!(!is_host_runner_image(codex.minimal_image));
        assert_eq!(
            latest_runner_image("codex", ContainerEngineAccess::Host).as_deref(),
            Some("ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest")
        );
        let digest = format!(
            "ghcr.io/xpressai/xpressclaw-runner-codex@sha256:{}",
            "a".repeat(64)
        );
        assert!(is_builtin_runner_image(&digest));
        assert!(is_builtin_runner_image_for_kind(&digest, "codex"));
        assert!(!is_builtin_runner_image_for_kind(&digest, "claude"));
        assert!(!is_managed_runner_image_for_kind(&digest, "codex"));
        assert!(is_managed_runner_image_for_kind(
            "ghcr.io/xpressai/xpressclaw-runner-codex:0123456789abcdef0123456789abcdef01234567",
            "codex"
        ));
        assert!(is_managed_runner_image_for_kind(
            "ghcr.io/xpressai/xpressclaw-runner-codex:latest",
            "codex"
        ));
        assert!(!is_managed_runner_image_for_kind(
            "ghcr.io/xpressai/xpressclaw-runner-codex:manually-pinned",
            "codex"
        ));
    }

    #[test]
    fn deepseek_harness_catalog_contract_is_stable() {
        let dsh = agent_definition("deepseek-harness").unwrap();
        assert_eq!(dsh.command, ["dsh-acp"]);
        assert_eq!(dsh.login_command, "dsh-acp login");
        assert_eq!(dsh.host_executables, ["dsh-acp", "dsh"]);
        assert_eq!(dsh.auth_mounts, DEEPSEEK_HARNESS_AUTH);
        assert!(!dsh.auth_mounts[0].read_only);
        assert_eq!(canonical_agent_kind("dsh"), Some("deepseek-harness"));
        assert_eq!(
            canonical_agent_kind("@openma/deepseek-harness-acp"),
            Some("deepseek-harness")
        );
        assert_eq!(canonical_agent_kind("my-codex-runner"), None);
        assert_eq!(
            infer_agent_kind_from_backend("legacy-my-codex-runner"),
            Some("codex")
        );
        assert_eq!(
            default_runner_image("deepseek-harness", ContainerEngineAccess::None),
            Some(dsh.minimal_image)
        );
        assert_eq!(
            default_runner_image("deepseek-harness", ContainerEngineAccess::Host),
            Some(dsh.host_image)
        );
        assert_eq!(
            local_runner_image_for_tag(dsh.minimal_image, "latest"),
            Some(dsh.local_minimal_image)
        );
        assert!(is_host_runner_image(dsh.local_host_image));
    }
}
