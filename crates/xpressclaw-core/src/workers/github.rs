//! Project-scoped Git and GitHub access for native ACP workers.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{EnvVariable, McpServer, McpServerStdio};
use serde_json::{json, Value};

use crate::connectors::manager::ConnectorManager;
use crate::db::Database;
use crate::error::{Error, Result};

const GITHUB_HOST: &str = "github.com";
const GITHUB_MCP_COMMAND: &str = "/opt/xpressclaw/mcp-github.mjs";
const CODEX_CONFIG_ENV: &str = "CODEX_CONFIG";
const GITHUB_GUIDANCE_MARKER: &str = "XpressClaw GitHub runtime:";
const GITHUB_CODEX_DEVELOPER_INSTRUCTIONS: &str = "\
XpressClaw GitHub runtime: An authenticated, project-scoped MCP server named \
`github` is attached. Its `gh` tool is XpressClaw's replacement for the shell \
GitHub CLI, which is intentionally absent from PATH. Do not run `gh --version` \
or `gh auth status`, ask the user to install or authenticate `gh`, or treat its \
absence as a blocker. When a skill or workflow requires `gh`, or bundles a \
script that shells out to it, treat the attached `github` MCP tool as satisfying \
that prerequisite and call the tool directly with the arguments that would \
follow `gh`. Use shell `git` for branches, commits, fetches, pushes, rebases, \
and other local Git operations. Use the MCP tool for pull requests, checks, \
Actions, issues, and review threads. Repository selection and authentication \
are already fixed by XpressClaw.";

/// Credentials and repository context made available to one short-lived
/// worker. Deliberately omit the token from `Debug` so future diagnostics do
/// not accidentally disclose it.
#[derive(Clone)]
pub struct GithubSessionAccess {
    pub owner: String,
    pub repo: String,
    token: String,
}

impl fmt::Debug for GithubSessionAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubSessionAccess")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl GithubSessionAccess {
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// ACP requires every agent to support stdio MCP servers. The real `gh`
    /// binary is kept out of the worker PATH and is only reachable through
    /// this argument-validating MCP process.
    pub fn mcp_server(&self) -> McpServer {
        McpServer::Stdio(McpServerStdio::new("github", GITHUB_MCP_COMMAND).env(vec![
            EnvVariable::new("GH_TOKEN", self.token.clone()),
            EnvVariable::new("GH_HOST", GITHUB_HOST),
            EnvVariable::new("GH_REPO", self.repository()),
            EnvVariable::new("NO_COLOR", "1"),
        ]))
    }
}

/// Discover GitHub access for the repository mounted into a worker.
///
/// Existing GitHub connector credentials take precedence. For the desktop
/// prototype, an existing host `gh` login is the zero-configuration fallback.
/// Neither source is mounted into the worker.
pub fn discover(db: &Arc<Database>, workspace: &Path) -> Option<GithubSessionAccess> {
    let (owner, repo) = origin_repository(workspace)?;
    let token = connector_token(db, &owner, &repo)
        .or_else(environment_token)
        .or_else(host_gh_token)?;
    Some(GithubSessionAccess { owner, repo, token })
}

/// Add sanitized Git identity plus the xpressclaw credential helper to the
/// container environment. Local Git remains unrestricted; only credential
/// acquisition is mediated.
pub fn extend_git_environment(environment: &mut Vec<String>, github: Option<&GithubSessionAccess>) {
    let mut config = Vec::<(String, String)>::new();

    if let Some(name) = host_git_config("user.name") {
        config.push(("user.name".into(), name));
    }
    if let Some(email) = host_git_config("user.email") {
        config.push(("user.email".into(), email));
    }

    if let Some(access) = github {
        config.extend([
            ("credential.helper".into(), "xpressclaw".into()),
            (
                "credential.https://github.com.useHttpPath".into(),
                "true".into(),
            ),
            (
                "url.https://github.com/.insteadOf".into(),
                "git@github.com:".into(),
            ),
            (
                "url.https://github.com/.insteadOf".into(),
                "ssh://git@github.com/".into(),
            ),
        ]);
        environment.push(format!("XPRESSCLAW_GITHUB_TOKEN={}", access.token));
        // Subscription-auth mounts intentionally preserve the harness's own
        // MCP configuration. Codex commonly references this conventional
        // variable from that config, so make the already-discovered project
        // credential available to it as well.
        environment.push(format!("GITHUB_PAT_TOKEN={}", access.token));
        environment.push("GIT_TERMINAL_PROMPT=0".into());
    }

    if config.is_empty() {
        return;
    }
    environment.push(format!("GIT_CONFIG_COUNT={}", config.len()));
    for (index, (key, value)) in config.into_iter().enumerate() {
        environment.push(format!("GIT_CONFIG_KEY_{index}={key}"));
        environment.push(format!("GIT_CONFIG_VALUE_{index}={value}"));
    }
}

/// Add environment-specific GitHub guidance at Codex's developer-instruction
/// priority. Generic GitHub plugin skills assume a shell `gh` binary, while
/// XpressClaw deliberately exposes the same supported operations only through
/// its constrained MCP server.
///
/// Preserve all user-supplied CODEX_CONFIG values and append to existing
/// developer instructions rather than replacing them.
pub fn add_codex_mcp_guidance(environment: &mut HashMap<String, String>) -> Result<()> {
    let mut config = match environment.get(CODEX_CONFIG_ENV) {
        Some(raw) => serde_json::from_str::<Value>(raw).map_err(|error| {
            Error::Backend(format!(
                "{CODEX_CONFIG_ENV} must contain valid JSON before XpressClaw can add GitHub MCP guidance: {error}"
            ))
        })?,
        None => json!({}),
    };
    let object = config.as_object_mut().ok_or_else(|| {
        Error::Backend(format!(
            "{CODEX_CONFIG_ENV} must be a JSON object before XpressClaw can add GitHub MCP guidance"
        ))
    })?;
    let existing = match object.get("developer_instructions") {
        Some(Value::String(instructions)) => instructions.as_str(),
        Some(Value::Null) | None => "",
        Some(_) => {
            return Err(Error::Backend(format!(
                "{CODEX_CONFIG_ENV}.developer_instructions must be a string"
            )));
        }
    };
    if !existing.contains(GITHUB_GUIDANCE_MARKER) {
        let combined = if existing.trim().is_empty() {
            GITHUB_CODEX_DEVELOPER_INSTRUCTIONS.to_string()
        } else {
            format!("{existing}\n\n{GITHUB_CODEX_DEVELOPER_INSTRUCTIONS}")
        };
        object.insert(
            "developer_instructions".to_string(),
            Value::String(combined),
        );
    }
    environment.insert(
        CODEX_CONFIG_ENV.to_string(),
        serde_json::to_string(&config).map_err(|error| {
            Error::Backend(format!(
                "failed to serialize {CODEX_CONFIG_ENV} with GitHub MCP guidance: {error}"
            ))
        })?,
    );
    Ok(())
}

fn origin_repository(workspace: &Path) -> Option<(String, String)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_remote(String::from_utf8_lossy(&output.stdout).trim())
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = remote.strip_prefix("ssh://git@github.com/") {
        path
    } else if let Some(path) = remote.strip_prefix("https://github.com/") {
        path
    } else if let Some(path) = remote.strip_prefix("http://github.com/") {
        path
    } else {
        return None;
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn connector_token(db: &Arc<Database>, owner: &str, repo: &str) -> Option<String> {
    ConnectorManager::new(db.clone())
        .list()
        .ok()?
        .into_iter()
        .find(|connector| {
            connector.enabled
                && connector.connector_type == "github"
                && connector
                    .config
                    .get("owner")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(owner))
                && connector
                    .config
                    .get("repo")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case(repo))
        })?
        .config
        .get("token")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn environment_token() -> Option<String> {
    std::env::var("GH_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn host_gh_token() -> Option<String> {
    let output = Command::new("gh")
        .args(["auth", "token", "--hostname", GITHUB_HOST])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn host_git_config(key: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--global", "--get", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_github_remote_forms() {
        for remote in [
            "git@github.com:XpressAI/xpressclaw.git",
            "ssh://git@github.com/XpressAI/xpressclaw.git",
            "https://github.com/XpressAI/xpressclaw.git",
            "http://github.com/XpressAI/xpressclaw/",
        ] {
            assert_eq!(
                parse_github_remote(remote),
                Some(("XpressAI".into(), "xpressclaw".into()))
            );
        }
        assert_eq!(parse_github_remote("git@gitlab.com:group/repo.git"), None);
        assert_eq!(
            parse_github_remote("https://github.com/too/many/parts"),
            None
        );
    }

    #[test]
    fn mcp_configuration_contains_only_the_scoped_repository_context() {
        let access = GithubSessionAccess {
            owner: "XpressAI".into(),
            repo: "xpressclaw".into(),
            token: "secret".into(),
        };
        let value = serde_json::to_value(access.mcp_server()).unwrap();
        assert!(value.get("type").is_none());
        assert_eq!(value["name"], "github");
        assert_eq!(value["command"], GITHUB_MCP_COMMAND);
        assert!(value["env"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "GH_REPO" && entry["value"] == "XpressAI/xpressclaw"));
    }

    #[test]
    fn github_access_is_redacted_in_debug_output() {
        let access = GithubSessionAccess {
            owner: "owner".into(),
            repo: "repo".into(),
            token: "do-not-print-me".into(),
        };
        let debug = format!("{access:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("do-not-print-me"));
    }

    #[test]
    fn git_environment_supports_mounted_harness_github_mcp_configuration() {
        let access = GithubSessionAccess {
            owner: "owner".into(),
            repo: "repo".into(),
            token: "secret".into(),
        };
        let mut environment = Vec::new();

        extend_git_environment(&mut environment, Some(&access));

        assert!(environment
            .iter()
            .any(|entry| entry == "XPRESSCLAW_GITHUB_TOKEN=secret"));
        assert!(environment
            .iter()
            .any(|entry| entry == "GITHUB_PAT_TOKEN=secret"));
    }

    #[test]
    fn codex_config_explains_that_github_mcp_replaces_shell_gh() {
        let mut environment = HashMap::new();

        add_codex_mcp_guidance(&mut environment).unwrap();

        let config: Value =
            serde_json::from_str(environment.get(CODEX_CONFIG_ENV).unwrap()).unwrap();
        let instructions = config["developer_instructions"].as_str().unwrap();
        assert!(instructions.contains("MCP server named `github`"));
        assert!(instructions.contains("intentionally absent from PATH"));
        assert!(instructions.contains("Do not run `gh --version`"));
        assert!(instructions.contains("Use shell `git`"));
    }

    #[test]
    fn codex_config_preserves_user_values_and_developer_instructions() {
        let mut environment = HashMap::from([(
            CODEX_CONFIG_ENV.to_string(),
            json!({
                "features": { "example": true },
                "developer_instructions": "Keep responses concise."
            })
            .to_string(),
        )]);

        add_codex_mcp_guidance(&mut environment).unwrap();
        add_codex_mcp_guidance(&mut environment).unwrap();

        let config: Value =
            serde_json::from_str(environment.get(CODEX_CONFIG_ENV).unwrap()).unwrap();
        assert_eq!(config["features"]["example"], true);
        let instructions = config["developer_instructions"].as_str().unwrap();
        assert!(instructions.starts_with("Keep responses concise."));
        assert_eq!(instructions.matches(GITHUB_GUIDANCE_MARKER).count(), 1);
    }

    #[test]
    fn codex_config_rejects_shapes_the_adapter_cannot_use() {
        let mut environment = HashMap::from([(CODEX_CONFIG_ENV.to_string(), "[]".to_string())]);
        assert!(add_codex_mcp_guidance(&mut environment)
            .unwrap_err()
            .to_string()
            .contains("must be a JSON object"));

        let mut environment = HashMap::from([(
            CODEX_CONFIG_ENV.to_string(),
            json!({ "developer_instructions": 42 }).to_string(),
        )]);
        assert!(add_codex_mcp_guidance(&mut environment)
            .unwrap_err()
            .to_string()
            .contains("must be a string"));
    }
}
