//! Read-only credential discovery and quota fetching across built-in harnesses.
//!
//! Supported sources:
//! - Claude (`~/.claude/.credentials.json`, macOS Keychain, `~/.pi/agent/auth.json`)
//! - Codex (`$CODEX_HOME/auth.json`, `~/.codex/auth.json`, OpenCode/Pi auth stores)
//! - GitHub Copilot (`COPILOT_TOKEN`/`GITHUB_TOKEN`/`GITHUB_PAT`, `gh/hosts.yml`)
//! - Cursor (`CURSOR_ACCESS_TOKEN`/`CURSOR_TOKEN`, `~/.config/cursor/auth.json`, `state.vscdb`)
//! - Kimi (`KIMI_API_KEY`, `~/.kimi/config.toml` + `~/.kimi/credentials/*.json`)
//! - Grok (`GROK_API_KEY`/`GROK_TOKEN`, `~/.grok/auth.json`)
//! - OpenCode Go (`~/.local/share/opencode/auth.json`)
//! - GLM / Z.ai (`ZAI_API_KEY`/`GLM_API_KEY`, `~/.config/glm-acp-agent`)
//! - Antigravity (`agy -p /usage --output-format json`, gated on `agy --version >= 1.1.11`)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde_json::Value;

use super::{
    balance_tone_from_remaining, hash_account_key, parse_retry_after, tone_from_used_pct,
    used_pct_of, window_from_reported_duration, window_from_used_pct, UsageAccountReport,
    UsageBalance, UsageBalanceUnit, UsageDetail, UsageProblem, UsageReportStatus, UsageTone,
    UsageWindow,
};

const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const COPILOT_USAGE_URL: &str = "https://api.github.com/copilot_internal/user";
const CURSOR_USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const KIMI_DEFAULT_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const ZAI_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const ZAI_SUBSCRIPTION_URL: &str = "https://api.z.ai/api/biz/subscription/list";

#[derive(Debug, Clone)]
pub struct DiscoveredAccount {
    pub id: String,
    pub provider: String,
    pub provider_label: String,
    pub harness: String,
    pub account_label: Option<String>,
    pub source: AccountSource,
}

#[derive(Debug, Clone)]
pub enum AccountSource {
    ClaudeFile {
        path: PathBuf,
        store: String,
    },
    #[cfg(target_os = "macos")]
    ClaudeKeychain {
        service: String,
        account: String,
    },
    CodexFile {
        path: PathBuf,
        store: String,
    },
    CopilotEnv {
        var: String,
    },
    CopilotHostsFile {
        path: PathBuf,
    },
    CursorEnv {
        var: String,
    },
    CursorAuthJson {
        path: PathBuf,
    },
    CursorSqlite {
        path: PathBuf,
    },
    KimiEnv {
        var: String,
    },
    KimiCredentialsFile {
        path: PathBuf,
        usage_url: String,
    },
    GrokEnv {
        var: String,
    },
    GrokAuthFile {
        path: PathBuf,
    },
    OpenCodeAuthFile {
        path: PathBuf,
    },
    GlmEnv {
        var: String,
    },
    GlmConfigFile {
        path: PathBuf,
    },
    AntigravityCli {
        binary: String,
    },
}

pub struct FetchOutcome {
    pub report: UsageAccountReport,
    pub retry_at: Option<DateTime<Utc>>,
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

fn xdg_data_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".local").join("share"))
}

fn pi_auth_path(home: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("auth.json");
        }
    }
    home.join(".pi").join("agent").join("auth.json")
}

fn read_json_file(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn wants_provider(filter: Option<&str>, provider: &str) -> bool {
    filter.is_none_or(|f| f == provider)
}

/// Discover all locally configured harness credential stores (strictly read-only).
pub fn discover_accounts(provider_filter: Option<&str>) -> Vec<DiscoveredAccount> {
    let Some(home) = user_home() else {
        return Vec::new();
    };
    let mut accounts = Vec::new();
    let mut seen_ids = HashSet::new();

    let mut push_unique = |account: DiscoveredAccount| {
        if seen_ids.insert(account.id.clone()) {
            accounts.push(account);
        }
    };

    if wants_provider(provider_filter, "claude") {
        for account in discover_claude(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "codex") {
        for account in discover_codex(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "github-copilot") {
        if let Some(account) = discover_copilot(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "cursor") {
        if let Some(account) = discover_cursor(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "kimi") {
        if let Some(account) = discover_kimi(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "grok") {
        if let Some(account) = discover_grok(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "opencode") {
        if let Some(account) = discover_opencode(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "glm") {
        if let Some(account) = discover_glm(&home) {
            push_unique(account);
        }
    }
    if wants_provider(provider_filter, "antigravity") {
        if let Some(account) = discover_antigravity(&home) {
            push_unique(account);
        }
    }

    accounts
}

// ---------------------------------------------------------------------------
// Discovery per provider
// ---------------------------------------------------------------------------

fn discover_claude(home: &Path) -> Vec<DiscoveredAccount> {
    let mut found = Vec::new();
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".claude"));
    let creds_path = claude_dir.join(".credentials.json");

    #[cfg(target_os = "macos")]
    {
        if let Some((service, account)) = discover_claude_keychain(&claude_dir) {
            let key = hash_account_key(&format!("keychain:{service}:{account}"));
            found.push(DiscoveredAccount {
                id: format!("claude:{key}"),
                provider: "claude".into(),
                provider_label: "Claude".into(),
                harness: "Claude".into(),
                account_label: None,
                source: AccountSource::ClaudeKeychain { service, account },
            });
        }
    }

    if found.is_empty() && read_claude_file_credentials(&creds_path).is_some() {
        let key = hash_account_key(&format!("claude:{}", creds_path.display()));
        found.push(DiscoveredAccount {
            id: format!("claude:{key}"),
            provider: "claude".into(),
            provider_label: "Claude".into(),
            harness: "Claude".into(),
            account_label: None,
            source: AccountSource::ClaudeFile {
                path: creds_path,
                store: "claude".into(),
            },
        });
    }

    // Also inspect Pi's auth store if it holds an Anthropic OAuth login.
    let pi_path = pi_auth_path(home);
    if read_pi_oauth(&pi_path, &["anthropic", "claude"]).is_some() {
        let key = hash_account_key(&format!("pi:anthropic:{}", pi_path.display()));
        found.push(DiscoveredAccount {
            id: format!("claude:{key}"),
            provider: "claude".into(),
            provider_label: "Claude".into(),
            harness: "Pi".into(),
            account_label: None,
            source: AccountSource::ClaudeFile {
                path: pi_path,
                store: "pi".into(),
            },
        });
    }

    found
}

#[cfg(target_os = "macos")]
fn discover_claude_keychain(claude_dir: &Path) -> Option<(String, String)> {
    let base = "Claude Code-credentials";
    let service = if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        let suffix = hash_account_key(&claude_dir.to_string_lossy());
        format!("{base}-{}", &suffix[..8])
    } else {
        base.to_string()
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "claude-code-user".into());
    if read_claude_keychain_json(&service, &user).is_some() {
        return Some((service, user));
    }
    None
}

#[cfg(target_os = "macos")]
fn read_claude_keychain_json(service: &str, account: &str) -> Option<Value> {
    let lookups = [
        vec!["find-generic-password", "-a", account, "-w", "-s", service],
        vec!["find-generic-password", "-w", "-s", service],
    ];
    for args in lookups {
        let output = Command::new("security").args(&args).output().ok()?;
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim()).ok()?;
        if parsed
            .get("claudeAiOauth")
            .and_then(|v| v.get("accessToken"))
            .and_then(Value::as_str)
            .is_some_and(|t| !t.is_empty())
        {
            return Some(parsed);
        }
    }
    None
}

fn discover_codex(home: &Path) -> Vec<DiscoveredAccount> {
    let mut found = Vec::new();
    let mut codex_paths = Vec::new();
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        if !dir.is_empty() {
            codex_paths.push(PathBuf::from(dir).join("auth.json"));
        }
    }
    let default_codex = home.join(".codex").join("auth.json");
    if !codex_paths.contains(&default_codex) {
        codex_paths.push(default_codex);
    }

    for path in codex_paths {
        if let Some(auth) = read_codex_auth_file(&path) {
            let (key, label) = codex_account_identity(&auth, &path);
            found.push(DiscoveredAccount {
                id: format!("codex:{key}"),
                provider: "codex".into(),
                provider_label: "Codex".into(),
                harness: "Codex".into(),
                account_label: label,
                source: AccountSource::CodexFile {
                    path,
                    store: "codex".into(),
                },
            });
        }
    }

    // OpenCode / Pi shared OpenAI OAuth logins.
    let opencode_path = xdg_data_home(home).join("opencode").join("auth.json");
    if let Some(oauth) = read_pi_oauth(&opencode_path, &["openai", "openai-codex"]) {
        let auth = CodexAuth {
            access_token: oauth.access,
            account_id: oauth.account_id,
            id_token: None,
            expires_ms: oauth.expires_ms,
        };
        let (key, label) = codex_account_identity(&auth, &opencode_path);
        found.push(DiscoveredAccount {
            id: format!("codex:{key}"),
            provider: "codex".into(),
            provider_label: "Codex".into(),
            harness: "OpenCode".into(),
            account_label: label,
            source: AccountSource::CodexFile {
                path: opencode_path,
                store: "opencode".into(),
            },
        });
    }

    let pi_path = pi_auth_path(home);
    if let Some(oauth) = read_pi_oauth(&pi_path, &["openai", "openai-codex"]) {
        let auth = CodexAuth {
            access_token: oauth.access,
            account_id: oauth.account_id,
            id_token: None,
            expires_ms: oauth.expires_ms,
        };
        let (key, label) = codex_account_identity(&auth, &pi_path);
        found.push(DiscoveredAccount {
            id: format!("codex:{key}"),
            provider: "codex".into(),
            provider_label: "Codex".into(),
            harness: "Pi".into(),
            account_label: label,
            source: AccountSource::CodexFile {
                path: pi_path,
                store: "pi".into(),
            },
        });
    }

    found
}

fn discover_copilot(home: &Path) -> Option<DiscoveredAccount> {
    for var in ["COPILOT_TOKEN", "GITHUB_TOKEN", "GITHUB_PAT"] {
        if non_empty_env(var).is_some() {
            return Some(DiscoveredAccount {
                id: format!("github-copilot:{}", hash_account_key(&format!("env:{var}"))),
                provider: "github-copilot".into(),
                provider_label: "GitHub Copilot".into(),
                harness: "Copilot".into(),
                account_label: None,
                source: AccountSource::CopilotEnv { var: var.into() },
            });
        }
    }
    let mut paths = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        if !appdata.is_empty() {
            paths.push(PathBuf::from(appdata).join("GitHub CLI").join("hosts.yml"));
        }
    }
    paths.push(home.join(".config").join("gh").join("hosts.yml"));
    for path in paths {
        if read_gh_hosts_token(&path).is_some() {
            return Some(DiscoveredAccount {
                id: format!(
                    "github-copilot:{}",
                    hash_account_key(&format!("gh:{}", path.display()))
                ),
                provider: "github-copilot".into(),
                provider_label: "GitHub Copilot".into(),
                harness: "Copilot".into(),
                account_label: None,
                source: AccountSource::CopilotHostsFile { path },
            });
        }
    }
    None
}

fn discover_cursor(home: &Path) -> Option<DiscoveredAccount> {
    for var in ["CURSOR_ACCESS_TOKEN", "CURSOR_TOKEN"] {
        if let Some(token) = non_empty_env(var) {
            let key = cursor_account_key(&token, &format!("env:{var}"));
            return Some(DiscoveredAccount {
                id: format!("cursor:{key}"),
                provider: "cursor".into(),
                provider_label: "Cursor".into(),
                harness: "Cursor".into(),
                account_label: None,
                source: AccountSource::CursorEnv { var: var.into() },
            });
        }
    }

    let mut sqlite_paths = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        if !appdata.is_empty() {
            sqlite_paths.push(
                PathBuf::from(appdata)
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            );
        }
    }
    sqlite_paths.push(
        home.join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    );
    sqlite_paths.push(
        home.join(".config")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    );
    for path in sqlite_paths {
        if let Some(token) = read_cursor_token_from_sqlite(&path) {
            let key = cursor_account_key(&token, &format!("sqlite:{}", path.display()));
            return Some(DiscoveredAccount {
                id: format!("cursor:{key}"),
                provider: "cursor".into(),
                provider_label: "Cursor".into(),
                harness: "Cursor".into(),
                account_label: None,
                source: AccountSource::CursorSqlite { path },
            });
        }
    }

    let auth_json = home.join(".config").join("cursor").join("auth.json");
    if let Some(token) = read_cursor_token_from_auth_json(&auth_json) {
        let key = cursor_account_key(&token, &format!("file:{}", auth_json.display()));
        return Some(DiscoveredAccount {
            id: format!("cursor:{key}"),
            provider: "cursor".into(),
            provider_label: "Cursor".into(),
            harness: "Cursor".into(),
            account_label: None,
            source: AccountSource::CursorAuthJson { path: auth_json },
        });
    }
    None
}

fn discover_kimi(home: &Path) -> Option<DiscoveredAccount> {
    if non_empty_env("KIMI_API_KEY").is_some() {
        return Some(DiscoveredAccount {
            id: format!("kimi:{}", hash_account_key("env:KIMI_API_KEY")),
            provider: "kimi".into(),
            provider_label: "Kimi".into(),
            harness: "Kimi".into(),
            account_label: None,
            source: AccountSource::KimiEnv {
                var: "KIMI_API_KEY".into(),
            },
        });
    }
    let kimi_home = std::env::var_os("KIMI_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".kimi"));
    let creds_dir = kimi_home.join("credentials");
    let Ok(entries) = std::fs::read_dir(&creds_dir) else {
        return None;
    };
    let mut json_files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    json_files.sort();
    for path in json_files {
        if read_kimi_credentials_file(&path).is_some() {
            return Some(DiscoveredAccount {
                id: format!(
                    "kimi:{}",
                    hash_account_key(&format!("kimi:{}", path.display()))
                ),
                provider: "kimi".into(),
                provider_label: "Kimi".into(),
                harness: "Kimi".into(),
                account_label: None,
                source: AccountSource::KimiCredentialsFile {
                    path,
                    usage_url: KIMI_DEFAULT_USAGE_URL.into(),
                },
            });
        }
    }
    None
}

fn discover_grok(home: &Path) -> Option<DiscoveredAccount> {
    for var in ["GROK_API_KEY", "GROK_TOKEN"] {
        if non_empty_env(var).is_some() {
            return Some(DiscoveredAccount {
                id: format!("grok:{}", hash_account_key(&format!("env:{var}"))),
                provider: "grok".into(),
                provider_label: "Grok".into(),
                harness: "Grok".into(),
                account_label: None,
                source: AccountSource::GrokEnv { var: var.into() },
            });
        }
    }
    let auth_path = home.join(".grok").join("auth.json");
    if read_json_file(&auth_path)
        .and_then(|v| extract_grok_token(&v))
        .is_some()
    {
        return Some(DiscoveredAccount {
            id: format!(
                "grok:{}",
                hash_account_key(&format!("grok:{}", auth_path.display()))
            ),
            provider: "grok".into(),
            provider_label: "Grok".into(),
            harness: "Grok".into(),
            account_label: None,
            source: AccountSource::GrokAuthFile { path: auth_path },
        });
    }
    None
}

fn discover_opencode(home: &Path) -> Option<DiscoveredAccount> {
    let auth_path = xdg_data_home(home).join("opencode").join("auth.json");
    if read_opencode_go_key(&auth_path).is_some() {
        return Some(DiscoveredAccount {
            id: format!(
                "opencode:{}",
                hash_account_key(&format!("opencode:{}", auth_path.display()))
            ),
            provider: "opencode".into(),
            provider_label: "OpenCode Go".into(),
            harness: "OpenCode".into(),
            account_label: None,
            source: AccountSource::OpenCodeAuthFile { path: auth_path },
        });
    }
    None
}

fn discover_glm(home: &Path) -> Option<DiscoveredAccount> {
    for var in ["ZAI_API_KEY", "GLM_API_KEY"] {
        if non_empty_env(var).is_some() {
            return Some(DiscoveredAccount {
                id: format!("glm:{}", hash_account_key(&format!("env:{var}"))),
                provider: "glm".into(),
                provider_label: "GLM / Z.ai".into(),
                harness: "GLM".into(),
                account_label: None,
                source: AccountSource::GlmEnv { var: var.into() },
            });
        }
    }
    let config_path = home
        .join(".config")
        .join("glm-acp-agent")
        .join("config.json");
    if read_glm_config_key(&config_path).is_some() {
        return Some(DiscoveredAccount {
            id: format!(
                "glm:{}",
                hash_account_key(&format!("glm:{}", config_path.display()))
            ),
            provider: "glm".into(),
            provider_label: "GLM / Z.ai".into(),
            harness: "GLM".into(),
            account_label: None,
            source: AccountSource::GlmConfigFile { path: config_path },
        });
    }
    None
}

fn discover_antigravity(home: &Path) -> Option<DiscoveredAccount> {
    // Only discover Antigravity when credentials exist AND `agy --version >= 1.1.11`
    // (older versions treat `/usage` in print mode as a billed LLM turn).
    let agy_home = std::env::var_os("ANTIGRAVITY_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".gemini").join("antigravity-cli"));
    if !agy_home.exists() && !home.join(".config").join("antigravity").exists() {
        return None;
    }
    let output = Command::new("agy")
        .arg("--version")
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version_text = String::from_utf8_lossy(&output.stdout);
    if !is_antigravity_usage_supported(&version_text) {
        return None;
    }
    let key = hash_account_key(&format!("antigravity:{}", agy_home.display()));
    Some(DiscoveredAccount {
        id: format!("antigravity:{key}"),
        provider: "antigravity".into(),
        provider_label: "Antigravity".into(),
        harness: "Antigravity".into(),
        account_label: None,
        source: AccountSource::AntigravityCli {
            binary: "agy".into(),
        },
    })
}

/// Returns true iff `agy --version` output contains a semver `>= 1.1.11`.
pub fn is_antigravity_usage_supported(version_output: &str) -> bool {
    for token in version_output.split(|c: char| !c.is_ascii_digit() && c != '.') {
        let parts: Vec<u64> = token
            .split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect();
        if parts.len() >= 3 {
            let (maj, min, pat) = (parts[0], parts[1], parts[2]);
            return (maj, min, pat) >= (1, 1, 11);
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Credential readers (strictly read-only)
// ---------------------------------------------------------------------------

struct ClaudeCreds {
    access_token: String,
    expires_ms: Option<i64>,
    plan_label: Option<String>,
}

fn read_claude_file_credentials(path: &Path) -> Option<ClaudeCreds> {
    let json = read_json_file(path)?;
    parse_claude_credentials_json(&json)
}

fn parse_claude_credentials_json(json: &Value) -> Option<ClaudeCreds> {
    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let expires_ms = oauth.get("expiresAt").and_then(Value::as_i64);
    let sub = oauth.get("subscriptionType").and_then(Value::as_str);
    let tier = oauth.get("rateLimitTier").and_then(Value::as_str);
    let plan_label = sub.map(|s| {
        let mut chars = s.chars();
        let cap = match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => String::new(),
        };
        if let Some(t) = tier
            .and_then(|t| t.split('_').next_back())
            .filter(|t| !t.is_empty())
        {
            format!("{cap} {t}")
        } else {
            cap
        }
    });
    Some(ClaudeCreds {
        access_token,
        expires_ms,
        plan_label,
    })
}

struct SharedOAuth {
    access: String,
    account_id: Option<String>,
    expires_ms: Option<i64>,
}

fn read_pi_oauth(path: &Path, keys: &[&str]) -> Option<SharedOAuth> {
    let json = read_json_file(path)?;
    let obj = json.as_object()?;
    for key in keys {
        let Some(entry) = obj.get(*key) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("oauth") {
            continue;
        }
        let Some(access) = entry
            .get("access")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let expires_ms = entry.get("expires").and_then(Value::as_i64);
        let account_id = entry
            .get("accountId")
            .or_else(|| entry.get("account_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Some(SharedOAuth {
            access: access.to_string(),
            account_id,
            expires_ms,
        });
    }
    None
}

struct CodexAuth {
    access_token: String,
    account_id: Option<String>,
    id_token: Option<String>,
    expires_ms: Option<i64>,
}

fn read_codex_auth_file(path: &Path) -> Option<CodexAuth> {
    let json = read_json_file(path)?;
    let tokens = json.get("tokens")?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(CodexAuth {
        access_token,
        account_id: tokens
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        id_token: tokens
            .get("id_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        expires_ms: None,
    })
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn codex_account_identity(auth: &CodexAuth, path: &Path) -> (String, Option<String>) {
    let access_claims = decode_jwt_claims(&auth.access_token);
    let id_claims = auth.id_token.as_deref().and_then(decode_jwt_claims);

    let claim_str = |claims: &Option<Value>, nested: Option<&str>, field: &str| -> Option<String> {
        let obj = claims.as_ref()?;
        let target = match nested {
            Some(ns) => obj.get(ns)?,
            None => obj,
        };
        target
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let raw_key = auth
        .account_id
        .clone()
        .or_else(|| {
            claim_str(
                &access_claims,
                Some("https://api.openai.com/auth"),
                "chatgpt_account_id",
            )
        })
        .or_else(|| claim_str(&access_claims, None, "chatgpt_account_id"))
        .or_else(|| {
            claim_str(
                &id_claims,
                Some("https://api.openai.com/auth"),
                "chatgpt_account_id",
            )
        });
    let key = raw_key
        .map(|k| hash_account_key(&k))
        .unwrap_or_else(|| hash_account_key(&format!("codex:{}", path.display())));

    let label = claim_str(
        &access_claims,
        Some("https://api.openai.com/profile"),
        "email",
    )
    .or_else(|| claim_str(&access_claims, None, "email"))
    .or_else(|| claim_str(&id_claims, Some("https://api.openai.com/profile"), "email"))
    .or_else(|| claim_str(&id_claims, None, "email"));

    (key, label)
}

fn read_gh_hosts_token(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("oauth_token:") {
            let token = rest.trim().trim_matches('"').trim_matches('\'');
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn cursor_account_key(token: &str, fallback: &str) -> String {
    if let Some(claims) = decode_jwt_claims(token) {
        if let Some(sub) = claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return format!("account-{}", hash_account_key(sub));
        }
    }
    format!("store-{}", hash_account_key(fallback))
}

fn read_cursor_token_from_sqlite(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    for key in ["cursorAuth/accessToken", "cursorAuthStatus"] {
        let val: Option<String> = conn
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .ok();
        let Some(raw) = val else {
            continue;
        };
        let trimmed = raw.trim();
        if key == "cursorAuth/accessToken" && !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            if let Some(token) = parsed
                .get("accessToken")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn read_cursor_token_from_auth_json(path: &Path) -> Option<String> {
    let json = read_json_file(path)?;
    json.get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

struct KimiCreds {
    access_token: String,
    expires_at_secs: Option<i64>,
}

fn read_kimi_credentials_file(path: &Path) -> Option<KimiCreds> {
    let json = read_json_file(path)?;
    let access_token = json
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let expires_at_secs = json.get("expires_at").and_then(Value::as_i64);
    Some(KimiCreds {
        access_token,
        expires_at_secs,
    })
}

pub fn extract_grok_token(auth: &Value) -> Option<String> {
    let obj = auth.as_object()?;
    if let Some(top) = obj
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(top.to_string());
    }
    // Prefer https://auth.x.ai::* entries first.
    for prefer_xai in [true, false] {
        for (k, v) in obj {
            if prefer_xai != k.starts_with("https://auth.x.ai::") {
                continue;
            }
            if let Some(key) = v
                .get("key")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn read_opencode_go_key(path: &Path) -> Option<String> {
    let json = read_json_file(path)?;
    for field in ["opencode-go", "opencode"] {
        if let Some(entry) = json.get(field) {
            if let Some(key) = entry
                .get("key")
                .or_else(|| entry.get("access"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn read_glm_config_key(path: &Path) -> Option<String> {
    let json = read_json_file(path)?;
    for field in ["apiKey", "api_key", "token"] {
        if let Some(key) = json
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(key.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fetching dispatcher
// ---------------------------------------------------------------------------

fn base_report(account: &DiscoveredAccount, now: DateTime<Utc>) -> UsageAccountReport {
    UsageAccountReport {
        id: account.id.clone(),
        provider: account.provider.clone(),
        provider_label: account.provider_label.clone(),
        harness: account.harness.clone(),
        account_label: account.account_label.clone(),
        status: UsageReportStatus::Available,
        plan_label: None,
        observed_at: None,
        fetched_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        windows: Vec::new(),
        balances: Vec::new(),
        details: Vec::new(),
        problem: None,
        error: None,
        last_successful: None,
    }
}

fn unavailable_outcome(
    account: &DiscoveredAccount,
    now: DateTime<Utc>,
    problem: UsageProblem,
    retry_at: Option<DateTime<Utc>>,
) -> FetchOutcome {
    let mut report = base_report(account, now);
    report.status = UsageReportStatus::Unavailable;
    report.problem = Some(problem);
    FetchOutcome { report, retry_at }
}

fn error_outcome(
    account: &DiscoveredAccount,
    now: DateTime<Utc>,
    error: impl Into<String>,
) -> FetchOutcome {
    let mut report = base_report(account, now);
    report.status = UsageReportStatus::Error;
    report.error = Some(error.into());
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub async fn fetch_account_usage(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    now: DateTime<Utc>,
) -> FetchOutcome {
    match &account.source {
        AccountSource::ClaudeFile { path, store } => {
            fetch_claude_file(client, account, path, store, now).await
        }
        #[cfg(target_os = "macos")]
        AccountSource::ClaudeKeychain {
            service,
            account: kc_account,
        } => fetch_claude_keychain(client, account, service, kc_account, now).await,
        AccountSource::CodexFile { path, store } => {
            fetch_codex_file(client, account, path, store, now).await
        }
        AccountSource::CopilotEnv { var } => {
            let Some(token) = non_empty_env(var) else {
                return error_outcome(account, now, "Copilot token is no longer set");
            };
            fetch_copilot_http(client, account, &token, now).await
        }
        AccountSource::CopilotHostsFile { path } => {
            let Some(token) = read_gh_hosts_token(path) else {
                return error_outcome(account, now, "GitHub CLI hosts file no longer has a token");
            };
            fetch_copilot_http(client, account, &token, now).await
        }
        AccountSource::CursorEnv { var } => {
            let Some(token) = non_empty_env(var) else {
                return error_outcome(account, now, "Cursor token is no longer set");
            };
            fetch_cursor_http(client, account, &token, now).await
        }
        AccountSource::CursorAuthJson { path } => {
            let Some(token) = read_cursor_token_from_auth_json(path) else {
                return error_outcome(account, now, "Cursor auth file no longer has a token");
            };
            fetch_cursor_http(client, account, &token, now).await
        }
        AccountSource::CursorSqlite { path } => {
            let path = path.clone();
            let token = tokio::task::spawn_blocking(move || read_cursor_token_from_sqlite(&path))
                .await
                .ok()
                .flatten();
            let Some(token) = token else {
                return error_outcome(account, now, "Cursor desktop database has no access token");
            };
            fetch_cursor_http(client, account, &token, now).await
        }
        AccountSource::KimiEnv { var } => {
            let Some(token) = non_empty_env(var) else {
                return error_outcome(account, now, "Kimi API key is no longer set");
            };
            fetch_kimi_http(client, account, &token, KIMI_DEFAULT_USAGE_URL, now).await
        }
        AccountSource::KimiCredentialsFile { path, usage_url } => {
            let Some(creds) = read_kimi_credentials_file(path) else {
                return error_outcome(account, now, "Kimi credentials file could not be read");
            };
            if let Some(exp) = creds.expires_at_secs {
                if exp <= now.timestamp() + 5 {
                    let expires_at = Utc
                        .timestamp_opt(exp, 0)
                        .single()
                        .unwrap_or(now)
                        .to_rfc3339_opts(SecondsFormat::Secs, true);
                    return unavailable_outcome(
                        account,
                        now,
                        UsageProblem::Expired {
                            expires_at,
                            refreshed_by: Some("kimi".into()),
                        },
                        None,
                    );
                }
            }
            fetch_kimi_http(client, account, &creds.access_token, usage_url, now).await
        }
        AccountSource::GrokEnv { var } => {
            let Some(token) = non_empty_env(var) else {
                return error_outcome(account, now, "Grok token is no longer set");
            };
            fetch_grok_http(client, account, &token, now).await
        }
        AccountSource::GrokAuthFile { path } => {
            let Some(token) = read_json_file(path).and_then(|v| extract_grok_token(&v)) else {
                return error_outcome(account, now, "Grok auth file no longer has a token");
            };
            fetch_grok_http(client, account, &token, now).await
        }
        AccountSource::OpenCodeAuthFile { path } => {
            let Some(token) = read_opencode_go_key(path) else {
                return error_outcome(account, now, "OpenCode Go key no longer exists");
            };
            fetch_opencode_go_http(client, account, &token, now).await
        }
        AccountSource::GlmEnv { var } => {
            let Some(token) = non_empty_env(var) else {
                return error_outcome(account, now, "GLM/Z.ai API key is no longer set");
            };
            fetch_glm_http(client, account, &token, now).await
        }
        AccountSource::GlmConfigFile { path } => {
            let Some(token) = read_glm_config_key(path) else {
                return error_outcome(account, now, "GLM config no longer has an API key");
            };
            fetch_glm_http(client, account, &token, now).await
        }
        AccountSource::AntigravityCli { binary } => {
            fetch_antigravity_cli(account, binary, now).await
        }
    }
}

// ---------------------------------------------------------------------------
// Provider HTTP / CLI fetchers + pure response parsers
// ---------------------------------------------------------------------------

async fn fetch_claude_file(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    path: &Path,
    store: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let creds = if store == "claude" {
        read_claude_file_credentials(path)
    } else {
        read_pi_oauth(path, &["anthropic", "claude"]).map(|o| ClaudeCreds {
            access_token: o.access,
            expires_ms: o.expires_ms,
            plan_label: None,
        })
    };
    let Some(creds) = creds else {
        return error_outcome(account, now, "Claude credential store no longer exists");
    };
    fetch_claude_http(client, account, &creds, store, now).await
}

#[cfg(target_os = "macos")]
async fn fetch_claude_keychain(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    service: &str,
    kc_account: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let service = service.to_string();
    let kc_account = kc_account.to_string();
    let creds = tokio::task::spawn_blocking(move || {
        read_claude_keychain_json(&service, &kc_account)
            .and_then(|v| parse_claude_credentials_json(&v))
    })
    .await
    .ok()
    .flatten();
    let Some(creds) = creds else {
        return error_outcome(
            account,
            now,
            "Claude Keychain credentials could not be read",
        );
    };
    fetch_claude_http(client, account, &creds, "claude", now).await
}

async fn fetch_claude_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    creds: &ClaudeCreds,
    refreshed_by: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    if let Some(exp_ms) = creds.expires_ms {
        if exp_ms <= now.timestamp_millis() {
            let expires_at = Utc
                .timestamp_millis_opt(exp_ms)
                .single()
                .unwrap_or(now)
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            return unavailable_outcome(
                account,
                now,
                UsageProblem::Expired {
                    expires_at,
                    refreshed_by: Some(refreshed_by.to_string()),
                },
                None,
            );
        }
    }

    let response = match client
        .get(CLAUDE_USAGE_URL)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("Accept", "application/json")
        .header("anthropic-beta", CLAUDE_OAUTH_BETA)
        .header("User-Agent", "claude-code/2.1.0")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Claude usage request failed: {err}"))
        }
    };

    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: Some(refreshed_by.to_string()),
            },
            None,
        );
    }
    if status == 429 {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok());
        let retry_at = parse_retry_after(retry_after, now)
            .or_else(|| Some(now + chrono::Duration::minutes(5)));
        return unavailable_outcome(
            account,
            now,
            UsageProblem::RateLimited {
                retry_at: retry_at.map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true)),
                detail: "Rate limited by Claude usage API".into(),
            },
            retry_at,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Claude usage API returned HTTP {status}"),
        );
    }

    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => {
            return error_outcome(account, now, format!("Invalid Claude usage JSON: {err}"))
        }
    };

    let mut report = base_report(account, now);
    report.plan_label = creds.plan_label.clone();
    let (windows, details) = parse_claude_usage(&body);
    report.windows = windows;
    report.details = details;
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_claude_usage(body: &Value) -> (Vec<UsageWindow>, Vec<UsageDetail>) {
    let mut windows = Vec::new();
    let mut details = Vec::new();

    for (field, id, label, short_label, duration_secs) in [
        ("five_hour", "five_hour", "Session", "5h", 5 * 3600_u64),
        ("seven_day", "weekly", "Weekly", "wk", 7 * 86_400_u64),
    ] {
        if let Some(w) = body.get(field).filter(|v| v.is_object()) {
            let util = as_f64(w.get("utilization").or_else(|| w.get("used_percentage")));
            let resets_at = as_string(w.get("resets_at"));
            windows.push(window_from_used_pct(
                id,
                label,
                Some(short_label.into()),
                true,
                util,
                Some(duration_secs),
                resets_at,
                None,
            ));
        }
    }

    // Reconcile legacy `seven_day_<model>` keys and newer `limits[]` entries.
    let mut scoped_names: HashMap<String, usize> = HashMap::new();
    for (field, name) in [
        ("seven_day_opus", "Opus"),
        ("seven_day_omelette", "Omelette"),
        ("seven_day_sonnet", "Sonnet"),
    ] {
        if let Some(w) = body.get(field).filter(|v| v.is_object()) {
            let norm = normalize_id(name);
            let idx = windows.len();
            scoped_names.insert(format!("model:{norm}"), idx);
            windows.push(window_from_used_pct(
                format!("weekly_model_{norm}"),
                format!("Weekly · {name}"),
                Some(format!("wk {name}")),
                false,
                as_f64(w.get("utilization")),
                Some(7 * 86_400),
                as_string(w.get("resets_at")),
                None,
            ));
        }
    }

    if let Some(limits) = body.get("limits").and_then(Value::as_array) {
        for entry in limits {
            if entry.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
                continue;
            }
            let scope = entry.get("scope");
            let (dim, scope_obj) =
                if let Some(m) = scope.and_then(|s| s.get("model")).filter(|v| v.is_object()) {
                    ("model", m)
                } else if let Some(s) = scope
                    .and_then(|s| s.get("surface"))
                    .filter(|v| v.is_object())
                {
                    ("surface", s)
                } else {
                    continue;
                };
            let id_raw = as_string(scope_obj.get("id"));
            let name = as_string(scope_obj.get("display_name")).or_else(|| id_raw.clone());
            let Some(name) = name else {
                continue;
            };
            let norm = normalize_id(&name);
            let util = as_f64(entry.get("percent"));
            let resets_at = as_string(entry.get("resets_at"));
            let key = format!("{dim}:{norm}");
            if let Some(&idx) = scoped_names.get(&key) {
                if util.is_some() {
                    windows[idx].used_pct = util.map(|v| v.clamp(0.0, 100.0));
                    windows[idx].remaining_pct =
                        windows[idx].used_pct.map(|v| (100.0 - v).clamp(0.0, 100.0));
                    windows[idx].tone = tone_from_used_pct(windows[idx].used_pct);
                }
                if resets_at.is_some() {
                    windows[idx].resets_at = resets_at;
                }
            } else {
                let win_id = format!("weekly_{dim}_{}", id_raw.unwrap_or(norm));
                let idx = windows.len();
                scoped_names.insert(key, idx);
                windows.push(window_from_used_pct(
                    win_id,
                    format!("Weekly · {name}"),
                    Some(format!("wk {name}")),
                    false,
                    util,
                    Some(7 * 86_400),
                    resets_at,
                    None,
                ));
            }
        }
    }

    if let Some(enabled) = body
        .get("extra_usage")
        .and_then(|e| e.get("is_enabled"))
        .and_then(Value::as_bool)
    {
        details.push(UsageDetail {
            id: "extra_usage".into(),
            label: "Extra usage".into(),
            value: if enabled { "Enabled" } else { "Disabled" }.into(),
            tone: None,
        });
    }

    (windows, details)
}

async fn fetch_codex_file(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    path: &Path,
    store: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let auth = if store == "codex" {
        read_codex_auth_file(path)
    } else {
        read_pi_oauth(path, &["openai", "openai-codex"]).map(|o| CodexAuth {
            access_token: o.access,
            account_id: o.account_id,
            id_token: None,
            expires_ms: o.expires_ms,
        })
    };
    let Some(auth) = auth else {
        return error_outcome(account, now, "Codex login store no longer exists");
    };
    if let Some(exp_ms) = auth.expires_ms {
        if exp_ms <= now.timestamp_millis() {
            let expires_at = Utc
                .timestamp_millis_opt(exp_ms)
                .single()
                .unwrap_or(now)
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            return unavailable_outcome(
                account,
                now,
                UsageProblem::Expired {
                    expires_at,
                    refreshed_by: Some(store.to_string()),
                },
                None,
            );
        }
    }

    let mut req = client
        .get(CODEX_USAGE_URL)
        .header("Authorization", format!("Bearer {}", auth.access_token))
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
        );
    if let Some(account_id) = &auth.account_id {
        req = req.header("ChatGPT-Account-Id", account_id);
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Codex usage request failed: {err}"))
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: Some(store.to_string()),
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Codex usage API returned HTTP {status}"),
        );
    }

    let text = match response.text().await {
        Ok(t) => t,
        Err(err) => {
            return error_outcome(account, now, format!("Codex response read failed: {err}"))
        }
    };
    if text.trim_start().starts_with('<') {
        return error_outcome(account, now, "Codex usage API returned HTML");
    }
    let body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(err) => return error_outcome(account, now, format!("Invalid Codex usage JSON: {err}")),
    };

    let mut report = base_report(account, now);
    let (plan_label, windows, balances) = parse_codex_usage(&body);
    report.plan_label = plan_label;
    report.windows = windows;
    report.balances = balances;
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_codex_usage(body: &Value) -> (Option<String>, Vec<UsageWindow>, Vec<UsageBalance>) {
    let plan_label = as_string(body.get("plan_type"));
    let mut windows = Vec::new();

    let mut push_rate_limit = |rl: Option<&Value>, scope: Option<(&str, &str)>, summary: bool| {
        let Some(rl) = rl.filter(|v| v.is_object()) else {
            return;
        };
        for (slot, primary) in [("primary_window", true), ("secondary_window", false)] {
            let Some(win) = rl.get(slot).filter(|v| v.is_object()) else {
                continue;
            };
            let used_pct = as_f64(win.get("used_percent"));
            let duration = as_u64(win.get("limit_window_seconds"));
            let resets_at = as_i64(win.get("reset_at")).and_then(|secs| {
                Utc.timestamp_opt(secs, 0)
                    .single()
                    .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
            });
            windows.push(window_from_reported_duration(
                duration,
                scope,
                if primary {
                    "unknown_primary"
                } else {
                    "unknown_secondary"
                },
                if primary {
                    "Primary limit"
                } else {
                    "Secondary limit"
                },
                "",
                used_pct,
                resets_at,
                summary,
                None,
            ));
        }
    };

    push_rate_limit(body.get("rate_limit"), None, true);
    if let Some(additional) = body.get("additional_rate_limits").and_then(Value::as_array) {
        for item in additional {
            let identity = as_string(item.get("metered_feature"))
                .or_else(|| as_string(item.get("limit_name")));
            let Some(identity) = identity else {
                continue;
            };
            let label = as_string(item.get("limit_name")).unwrap_or_else(|| identity.clone());
            let scope_id = format!("limit:{}", normalize_id(&identity));
            push_rate_limit(item.get("rate_limit"), Some((&scope_id, &label)), false);
        }
    }
    push_rate_limit(
        body.get("code_review_rate_limit"),
        Some(("code_review", "Code review")),
        false,
    );

    let mut balances = Vec::new();
    if let Some(bal) = body.get("credits").and_then(|c| as_f64(c.get("balance"))) {
        balances.push(UsageBalance {
            id: "credits".into(),
            label: "Credits".into(),
            used: None,
            remaining: Some(bal),
            limit: None,
            unit: UsageBalanceUnit::Credits,
            resets_at: None,
            tone: balance_tone_from_remaining(Some(bal)),
        });
    }

    (plan_label, windows, balances)
}

async fn fetch_copilot_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let response = match client
        .get(COPILOT_USAGE_URL)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("X-Github-Api-Version", "2025-04-01")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Copilot usage request failed: {err}"))
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: None,
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Copilot usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => {
            return error_outcome(account, now, format!("Invalid Copilot usage JSON: {err}"))
        }
    };
    let mut report = base_report(account, now);
    let (plan_label, windows, details) = parse_copilot_usage(&body);
    report.plan_label = plan_label;
    report.windows = windows;
    report.details = details;
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_copilot_usage(body: &Value) -> (Option<String>, Vec<UsageWindow>, Vec<UsageDetail>) {
    let plan_label = as_string(body.get("copilot_plan"));
    let reset_utc = as_string(body.get("quota_reset_date_utc"));
    let reset_date = as_string(body.get("quota_reset_date"));
    let mut details = Vec::new();
    if let Some(ref rd) = reset_date {
        details.push(UsageDetail {
            id: "reset".into(),
            label: "Quota reset".into(),
            value: rd.clone(),
            tone: None,
        });
    }
    let mut windows = Vec::new();
    if let Some(premium) = body
        .get("quota_snapshots")
        .and_then(|s| s.get("premium_interactions"))
        .filter(|v| v.is_object())
    {
        let unlimited = premium
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !unlimited {
            if let Some(pct_rem) = as_f64(premium.get("percent_remaining")) {
                windows.push(window_from_used_pct(
                    "premium_interactions",
                    "Premium requests",
                    Some("mo".into()),
                    true,
                    Some((100.0 - pct_rem).clamp(0.0, 100.0)),
                    Some(30 * 86_400),
                    reset_utc.or(reset_date),
                    None,
                ));
            }
            if let (Some(rem), Some(ent)) = (
                as_u64(premium.get("remaining")),
                as_u64(premium.get("entitlement")),
            ) {
                details.push(UsageDetail {
                    id: "premium_remaining".into(),
                    label: "Premium requests remaining".into(),
                    value: format!("{rem} / {ent}"),
                    tone: None,
                });
            }
        }
    }
    (plan_label, windows, details)
}

async fn fetch_cursor_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let response = match client
        .post(CURSOR_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .body("{}")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Cursor usage request failed: {err}"))
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: None,
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Cursor usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => {
            return error_outcome(account, now, format!("Invalid Cursor usage JSON: {err}"))
        }
    };
    let mut report = base_report(account, now);
    let (windows, balances) = parse_cursor_usage(&body);
    report.windows = windows;
    report.balances = balances;
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_cursor_usage(body: &Value) -> (Vec<UsageWindow>, Vec<UsageBalance>) {
    let resets_at = parse_flexible_timestamp(body.get("billingCycleEnd"));
    let mut windows = Vec::new();
    let mut balances = Vec::new();
    if let Some(plan) = body.get("planUsage").filter(|v| v.is_object()) {
        for (id, label, field, summary) in [
            ("plan_usage", "Total", "totalPercentUsed", true),
            ("cursor_usage", "Cursor", "autoPercentUsed", false),
            ("third_party_usage", "Third Party", "apiPercentUsed", false),
        ] {
            if let Some(pct) = as_f64(plan.get(field)) {
                windows.push(window_from_used_pct(
                    id,
                    label,
                    Some("mo".into()),
                    summary,
                    Some(pct),
                    Some(30 * 86_400),
                    resets_at.clone(),
                    None,
                ));
            }
        }
        if windows.is_empty() {
            let total_spend = as_f64(plan.get("totalSpend")).map(|c| c / 100.0);
            let remaining = as_f64(plan.get("remaining")).map(|c| c / 100.0);
            let limit = as_f64(plan.get("limit")).map(|c| c / 100.0);
            if total_spend.is_some() || remaining.is_some() || limit.is_some() {
                balances.push(UsageBalance {
                    id: "plan_usage".into(),
                    label: "Plan usage".into(),
                    used: total_spend,
                    remaining,
                    limit,
                    unit: UsageBalanceUnit::Usd,
                    resets_at,
                    tone: tone_from_used_pct(used_pct_of(total_spend, limit)),
                });
            }
        }
    }
    (windows, balances)
}

async fn fetch_kimi_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    usage_url: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let response = match client
        .get(usage_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Kimi usage request failed: {err}"))
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: Some("kimi".into()),
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Kimi usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => return error_outcome(account, now, format!("Invalid Kimi usage JSON: {err}")),
    };
    let mut report = base_report(account, now);
    report.windows = parse_kimi_usage(&body);
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_kimi_usage(body: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    if let Some(usage) = body.get("usage").filter(|v| v.is_object()) {
        if let Some(pct) = kimi_used_pct(usage) {
            let label = as_string(usage.get("name"))
                .or_else(|| as_string(usage.get("title")))
                .unwrap_or_else(|| "Usage limit".into());
            windows.push(window_from_used_pct(
                "coding_usage",
                label,
                None,
                true,
                Some(pct),
                None,
                kimi_reset_time(usage),
                None,
            ));
        }
    }
    if let Some(limits) = body.get("limits").and_then(Value::as_array) {
        for (idx, item) in limits.iter().enumerate() {
            let fields = item.get("detail").filter(|v| v.is_object()).unwrap_or(item);
            let Some(pct) = kimi_used_pct(fields) else {
                continue;
            };
            let win_meta = item.get("window").filter(|v| v.is_object());
            let duration = win_meta
                .and_then(|w| as_u64(w.get("duration")))
                .or_else(|| as_u64(item.get("duration")));
            let time_unit = win_meta
                .and_then(|w| as_string(w.get("timeUnit")))
                .or_else(|| as_string(item.get("timeUnit")));
            let duration_secs = kimi_duration_seconds(duration, time_unit.as_deref());
            let label = as_string(item.get("name"))
                .or_else(|| as_string(fields.get("name")))
                .or_else(|| as_string(fields.get("title")))
                .or_else(|| duration_secs.map(|s| super::duration_window_name(s).1))
                .unwrap_or_else(|| format!("Limit {}", idx + 1));
            let short = duration_secs.map(|s| super::duration_window_name(s).2);
            windows.push(window_from_used_pct(
                format!("coding_limit_{}", idx + 1),
                label,
                short,
                windows.is_empty(),
                Some(pct),
                duration_secs,
                kimi_reset_time(fields),
                None,
            ));
        }
    }
    windows
}

fn kimi_used_pct(obj: &Value) -> Option<f64> {
    let limit = as_f64(obj.get("limit"))?;
    if limit <= 0.0 {
        return None;
    }
    let used =
        as_f64(obj.get("used")).or_else(|| as_f64(obj.get("remaining")).map(|rem| limit - rem))?;
    Some((used / limit * 100.0).clamp(0.0, 100.0))
}

fn kimi_reset_time(obj: &Value) -> Option<String> {
    for key in ["resetTime", "resetAt", "reset_time", "reset_at"] {
        if let Some(ts) = as_string(obj.get(key)) {
            return Some(ts);
        }
    }
    None
}

fn kimi_duration_seconds(duration: Option<u64>, unit: Option<&str>) -> Option<u64> {
    let d = duration.filter(|&v| v > 0)?;
    let norm = unit?.trim().to_ascii_uppercase();
    let norm = norm.strip_prefix("TIME_UNIT_").unwrap_or(&norm);
    let mult = match norm {
        "SECOND" => 1,
        "MINUTE" => 60,
        "HOUR" => 3_600,
        "DAY" => 86_400,
        "WEEK" => 604_800,
        _ => return None,
    };
    d.checked_mul(mult)
}

async fn fetch_grok_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let response = match client
        .get(GROK_BILLING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(account, now, format!("Grok usage request failed: {err}"))
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: None,
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("Grok usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => return error_outcome(account, now, format!("Invalid Grok usage JSON: {err}")),
    };
    let mut report = base_report(account, now);
    let (windows, balances) = parse_grok_usage(&body);
    report.windows = windows;
    report.balances = balances;
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_grok_usage(body: &Value) -> (Vec<UsageWindow>, Vec<UsageBalance>) {
    let config = body.get("config").filter(|v| v.is_object()).unwrap_or(body);
    let mut windows = Vec::new();
    let mut balances = Vec::new();

    if let Some(pct) = as_f64(config.get("creditUsagePercent")) {
        let period = config.get("currentPeriod");
        let ptype = period.and_then(|p| as_string(p.get("type")));
        let resets_at = period.and_then(|p| as_string(p.get("end")));
        let (id, label, short, dur) = match ptype.as_deref() {
            Some("USAGE_PERIOD_TYPE_WEEKLY") => ("weekly", "Weekly", "wk", Some(7 * 86_400)),
            Some("USAGE_PERIOD_TYPE_MONTHLY") => ("monthly", "Monthly", "mo", Some(30 * 86_400)),
            _ => ("current_period", "Current period", "", None),
        };
        windows.push(window_from_used_pct(
            id,
            label,
            Some(short.into()),
            true,
            Some(pct),
            dur,
            resets_at,
            None,
        ));
    }

    let limit = config
        .get("monthlyLimit")
        .and_then(|m| as_f64(m.get("val")));
    let used = config
        .get("used")
        .and_then(|u| as_f64(u.get("val")))
        .or_else(|| body.get("usage").and_then(|u| as_f64(u.get("creditUsage"))));
    if limit.is_some() || used.is_some() {
        let remaining = match (limit, used) {
            (Some(l), Some(u)) => Some((l - u).max(0.0)),
            _ => None,
        };
        balances.push(UsageBalance {
            id: if limit.is_some() {
                "monthly_credits".into()
            } else {
                "credits".into()
            },
            label: if limit.is_some() {
                "Monthly credits".into()
            } else {
                "Credits".into()
            },
            used,
            remaining,
            limit,
            unit: UsageBalanceUnit::Credits,
            resets_at: None,
            tone: tone_from_used_pct(used_pct_of(used, limit)),
        });
    }

    (windows, balances)
}

async fn fetch_opencode_go_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let response = match client
        .get(OPENCODE_GO_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(
                account,
                now,
                format!("OpenCode Go usage request failed: {err}"),
            )
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: Some("opencode".into()),
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("OpenCode Go usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => {
            return error_outcome(
                account,
                now,
                format!("Invalid OpenCode Go usage JSON: {err}"),
            )
        }
    };
    let mut report = base_report(account, now);
    report.plan_label = Some("Go".into());
    report.windows = parse_opencode_go_usage(&body);
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_opencode_go_usage(body: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    let Some(usage) = body.get("usage").filter(|v| v.is_object()) else {
        return windows;
    };
    for (field, label, short, duration_secs, summary) in [
        ("rolling", "Rolling", "", None, true),
        ("weekly", "Weekly", "wk", Some(7 * 86_400_u64), true),
        ("monthly", "Monthly", "mo", Some(30 * 86_400_u64), false),
    ] {
        if let Some(w) = usage.get(field).filter(|v| v.is_object()) {
            let pct = as_f64(w.get("percent"));
            let resets_at = as_string(w.get("resetsAt"));
            windows.push(window_from_used_pct(
                field,
                label,
                Some(short.into()),
                summary,
                pct,
                duration_secs,
                resets_at,
                None,
            ));
        }
    }
    windows
}

async fn fetch_glm_http(
    client: &reqwest::Client,
    account: &DiscoveredAccount,
    token: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    // Try Z.ai quota limit endpoint first (5h/weekly/monthly windows).
    if let Ok(response) = client
        .get(ZAI_QUOTA_URL)
        .header("Authorization", token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return unavailable_outcome(
                account,
                now,
                UsageProblem::Rejected {
                    status,
                    refreshed_by: None,
                },
                None,
            );
        }
        if response.status().is_success() {
            if let Ok(body) = response.json::<Value>().await {
                let (plan_label, windows) = parse_glm_quota(&body);
                if !windows.is_empty() {
                    let mut report = base_report(account, now);
                    report.plan_label = plan_label;
                    report.windows = windows;
                    return FetchOutcome {
                        report,
                        retry_at: None,
                    };
                }
            }
        }
    }

    // Fallback to subscription list metadata.
    let response = match client
        .get(ZAI_SUBSCRIPTION_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return error_outcome(
                account,
                now,
                format!("GLM/Z.ai usage request failed: {err}"),
            )
        }
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::Rejected {
                status,
                refreshed_by: None,
            },
            None,
        );
    }
    if !response.status().is_success() {
        return error_outcome(
            account,
            now,
            format!("GLM/Z.ai usage API returned HTTP {status}"),
        );
    }
    let body: Value = match response.json().await {
        Ok(v) => v,
        Err(err) => return error_outcome(account, now, format!("Invalid GLM/Z.ai JSON: {err}")),
    };
    let Some(sub) = body
        .get("data")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
    else {
        return unavailable_outcome(
            account,
            now,
            UsageProblem::NoQuota {
                detail: "No active coding plan".into(),
            },
            None,
        );
    };
    let mut report = base_report(account, now);
    report.plan_label = as_string(sub.get("productName"));
    for (field, id, label) in [
        ("status", "status", "Status"),
        ("valid", "valid", "Valid"),
        ("purchaseTime", "purchase_time", "Purchased"),
    ] {
        if let Some(val) = as_string(sub.get(field)) {
            report.details.push(UsageDetail {
                id: id.into(),
                label: label.into(),
                value: val,
                tone: None,
            });
        }
    }
    FetchOutcome {
        report,
        retry_at: None,
    }
}

pub fn parse_glm_quota(body: &Value) -> (Option<String>, Vec<UsageWindow>) {
    let mut windows = Vec::new();
    let Some(data) = body.get("data").filter(|v| v.is_object()) else {
        return (None, windows);
    };
    let plan_label = as_string(data.get("level"));
    let Some(limits) = data.get("limits").and_then(Value::as_array) else {
        return (plan_label, windows);
    };
    for limit in limits {
        let ltype = limit.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(ltype, "TOKENS_LIMIT" | "CREDIT_LIMIT" | "TIME_LIMIT") {
            continue;
        }
        let used_pct = if let Some(total) = as_f64(limit.get("usage")).filter(|&t| t > 0.0) {
            as_f64(limit.get("currentValue"))
                .or_else(|| as_f64(limit.get("remaining")).map(|rem| total - rem))
                .map(|used| (used / total * 100.0).clamp(0.0, 100.0))
                .or_else(|| as_f64(limit.get("percentage")))
        } else {
            as_f64(limit.get("percentage"))
        };
        let unit = as_u64(limit.get("unit"));
        let count = as_u64(limit.get("number"));
        let minutes = if ltype == "TIME_LIMIT" && unit == Some(5) && count == Some(1) {
            Some(30 * 24 * 60_u64)
        } else {
            match (unit, count) {
                (Some(1), Some(n)) => Some(n * 1_440),
                (Some(3), Some(n)) => Some(n * 60),
                (Some(5), Some(n)) => Some(n),
                (Some(6), Some(n)) => Some(n * 10_080),
                _ => None,
            }
        };
        let Some(mins) = minutes.filter(|&m| m > 0) else {
            continue;
        };
        let resets_at = as_i64(limit.get("nextResetTime")).and_then(|ms| {
            let secs = if ms > 10_000_000_000 { ms / 1000 } else { ms };
            Utc.timestamp_opt(secs, 0)
                .single()
                .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        });
        let secs = mins * 60;
        let summary = mins == 300 || mins == 10_080;
        windows.push(window_from_reported_duration(
            Some(secs),
            None,
            "limit",
            "Limit",
            "",
            used_pct,
            resets_at,
            summary,
            None,
        ));
    }
    (plan_label, windows)
}

async fn fetch_antigravity_cli(
    account: &DiscoveredAccount,
    binary: &str,
    now: DateTime<Utc>,
) -> FetchOutcome {
    let bin = binary.to_string();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(25),
        tokio::process::Command::new(&bin)
            .args([
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "20s",
            ])
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .output(),
    )
    .await;

    let output = match output {
        Ok(Ok(out)) => out,
        Ok(Err(err)) => {
            return error_outcome(
                account,
                now,
                format!("Antigravity usage command failed: {err}"),
            )
        }
        Err(_) => return error_outcome(account, now, "Antigravity usage command timed out"),
    };

    if !output.status.success() {
        return error_outcome(
            account,
            now,
            "Antigravity usage command failed; check that agy is signed in",
        );
    }

    let body: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(err) => {
            return error_outcome(
                account,
                now,
                format!("Invalid Antigravity usage JSON: {err}"),
            )
        }
    };

    match parse_antigravity_usage(&body) {
        Ok(windows) if windows.is_empty() => unavailable_outcome(
            account,
            now,
            UsageProblem::NoQuota {
                detail: "No usage quota reported by agy".into(),
            },
            None,
        ),
        Ok(windows) => {
            let mut report = base_report(account, now);
            report.windows = windows;
            FetchOutcome {
                report,
                retry_at: None,
            }
        }
        Err(err) => error_outcome(account, now, err),
    }
}

pub fn parse_antigravity_usage(body: &Value) -> Result<Vec<UsageWindow>, String> {
    let status = body.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "SUCCESS" {
        let msg = as_string(body.get("error"))
            .or_else(|| as_string(body.get("response")))
            .unwrap_or_else(|| format!("agy usage returned {status}"));
        return Err(msg);
    }
    let groups = body
        .get("command")
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("groups"))
        .and_then(Value::as_array)
        .ok_or_else(|| "agy did not return usage groups".to_string())?;

    let include_group = groups.len() > 1;
    let mut windows = Vec::new();
    for group in groups {
        let raw_group = as_string(group.get("name")).unwrap_or_default();
        let group_label = raw_group
            .trim()
            .strip_suffix("models")
            .or_else(|| raw_group.trim().strip_suffix("Models"))
            .unwrap_or(raw_group.trim())
            .trim()
            .to_string();
        let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            let disabled = bucket
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let rem_frac = as_f64(bucket.get("remaining_fraction"));
            if !disabled && rem_frac.is_none() {
                continue;
            }
            let Some(id) = as_string(bucket.get("id")) else {
                continue;
            };
            let bname = as_string(bucket.get("name")).unwrap_or_else(|| id.clone());
            let bwin = as_string(bucket.get("window")).unwrap_or_default();
            let norm_win = bwin.trim().to_ascii_lowercase();
            let duration_secs = match norm_win.as_str() {
                "5h" => Some(5 * 3600_u64),
                "weekly" => Some(7 * 86_400_u64),
                _ => None,
            };
            let short_period = match norm_win.as_str() {
                "weekly" => "wk",
                "monthly" => "mo",
                other => other,
            };
            let short_label = if include_group && !group_label.is_empty() {
                format!("{group_label} {short_period}").trim().to_string()
            } else {
                short_period.to_string()
            };
            let clean_bname = bname
                .trim()
                .trim_end_matches("Limit Remaining")
                .trim_end_matches("Remaining")
                .trim();
            let win_label = if norm_win == "5h" { "5h" } else { clean_bname };
            let mut label_parts = Vec::new();
            if !group_label.is_empty() {
                label_parts.push(group_label.as_str());
            }
            if !win_label.is_empty() {
                label_parts.push(win_label);
            }
            if disabled {
                label_parts.push("Disabled");
            }
            let used_pct = if disabled {
                None
            } else {
                rem_frac.map(|f| ((1.0 - f) * 100.0).clamp(0.0, 100.0))
            };
            let resets_at = if disabled {
                None
            } else {
                as_string(bucket.get("reset_time"))
            };
            windows.push(window_from_used_pct(
                id,
                label_parts.join(" · "),
                Some(short_label),
                !disabled && norm_win == "weekly",
                used_pct,
                duration_secs,
                resets_at,
                if disabled {
                    Some(UsageTone::Default)
                } else {
                    None
                },
            ));
        }
    }
    Ok(windows)
}

// ---------------------------------------------------------------------------
// JSON coercion helpers
// ---------------------------------------------------------------------------

fn as_f64(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        return n.is_finite().then_some(n);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    }
    None
}

fn as_i64(value: Option<&Value>) -> Option<i64> {
    let v = value?;
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    as_f64(Some(v)).map(|n| n as i64)
}

fn as_u64(value: Option<&Value>) -> Option<u64> {
    let v = value?;
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    as_f64(Some(v)).filter(|&n| n >= 0.0).map(|n| n as u64)
}

fn as_string(value: Option<&Value>) -> Option<String> {
    let v = value?;
    match v {
        Value::String(s) => {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn normalize_id(name: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_string()
}

fn parse_flexible_timestamp(value: Option<&Value>) -> Option<String> {
    let v = value?;
    if let Some(num) = as_i64(Some(v)) {
        let secs = if num.abs() >= 10_000_000_000 {
            num / 1000
        } else {
            num
        };
        return Utc
            .timestamp_opt(secs, 0)
            .single()
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    let s = as_string(Some(v))?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Some(
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_claude_usage_with_legacy_and_scoped_limits_reconciled() {
        let payload = json!({
            "five_hour": { "utilization": 42.5, "resets_at": "2026-11-26T16:00:00Z" },
            "seven_day": { "utilization": 18.0, "resets_at": "2026-12-01T00:00:00Z" },
            "seven_day_opus": { "utilization": 12.0, "resets_at": "2026-12-01T00:00:00Z" },
            "limits": [
                {
                    "kind": "weekly_scoped",
                    "percent": 15.0,
                    "resets_at": "2026-12-01T00:00:00Z",
                    "scope": { "model": { "display_name": "Opus" } }
                },
                {
                    "kind": "weekly_scoped",
                    "percent": 30.0,
                    "resets_at": "2026-12-01T00:00:00Z",
                    "scope": { "surface": { "id": "web", "display_name": "Web" } }
                }
            ],
            "extra_usage": { "is_enabled": true }
        });
        let (windows, details) = parse_claude_usage(&payload);
        assert_eq!(windows.len(), 4);
        assert_eq!(windows[0].id, "five_hour");
        assert_eq!(windows[0].used_pct, Some(42.5));
        assert_eq!(windows[0].remaining_pct, Some(57.5));
        assert_eq!(windows[1].id, "weekly");
        // Reconciled Opus window takes the newer limits[] percent (15.0) without duplicating.
        assert_eq!(windows[2].id, "weekly_model_opus");
        assert_eq!(windows[2].used_pct, Some(15.0));
        assert_eq!(windows[3].id, "weekly_surface_web");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].value, "Enabled");
    }

    #[test]
    fn parses_codex_usage_windows_and_credits() {
        let payload = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": { "used_percent": 75.0, "limit_window_seconds": 18000, "reset_at": 1795694400 },
                "secondary_window": { "used_percent": 20.0, "limit_window_seconds": 604800, "reset_at": 1796000000 }
            },
            "credits": { "balance": 12.5 }
        });
        let (plan, windows, balances) = parse_codex_usage(&payload);
        assert_eq!(plan.as_deref(), Some("plus"));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, "five_hour");
        assert_eq!(windows[0].tone, UsageTone::Warning);
        assert_eq!(windows[1].id, "weekly");
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].remaining, Some(12.5));
    }

    #[test]
    fn parses_copilot_cursor_kimi_grok_opencode_glm_and_antigravity() {
        let (plan, w, d) = parse_copilot_usage(&json!({
            "copilot_plan": "individual",
            "quota_reset_date_utc": "2026-12-01T00:00:00Z",
            "quota_snapshots": {
                "premium_interactions": { "entitlement": 300, "remaining": 210, "percent_remaining": 70.0 }
            }
        }));
        assert_eq!(plan.as_deref(), Some("individual"));
        assert_eq!(w[0].used_pct, Some(30.0));
        assert_eq!(d[0].value, "210 / 300");

        let (cw, cb) = parse_cursor_usage(&json!({
            "planUsage": { "totalPercentUsed": 55.0, "autoPercentUsed": 40.0 },
            "billingCycleEnd": 1796083200000_i64
        }));
        assert_eq!(cw.len(), 2);
        assert!(cb.is_empty());

        let kw = parse_kimi_usage(&json!({
            "limits": [
                { "window": { "duration": 5, "timeUnit": "TIME_UNIT_HOUR" }, "detail": { "limit": "100", "used": "25", "resetTime": "2026-11-26T17:00:00Z" } }
            ]
        }));
        assert_eq!(kw.len(), 1);
        assert_eq!(kw[0].used_pct, Some(25.0));
        assert_eq!(kw[0].duration_seconds, Some(18_000));

        let (gw, gb) = parse_grok_usage(&json!({
            "config": {
                "creditUsagePercent": 80.0,
                "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-12-01T00:00:00Z" },
                "monthlyLimit": { "val": 100 },
                "used": { "val": 80 }
            }
        }));
        assert_eq!(gw[0].id, "weekly");
        assert_eq!(gb[0].remaining, Some(20.0));

        let ow = parse_opencode_go_usage(&json!({
            "usage": {
                "rolling": { "status": "ok", "percent": 15, "resetsAt": "2026-11-26T15:00:00Z" },
                "weekly": { "status": "ok", "percent": 45, "resetsAt": "2026-12-01T00:00:00Z" },
                "monthly": { "status": "ok", "percent": 20, "resetsAt": "2026-12-26T00:00:00Z" }
            }
        }));
        assert_eq!(ow.len(), 3);

        let (gplan, glmw) = parse_glm_quota(&json!({
            "success": true,
            "data": {
                "level": "pro",
                "limits": [
                    { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "usage": 1000, "currentValue": 400, "nextResetTime": 1795694400000_i64 }
                ]
            }
        }));
        assert_eq!(gplan.as_deref(), Some("pro"));
        assert_eq!(glmw[0].used_pct, Some(40.0));

        let agw = parse_antigravity_usage(&json!({
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [{
                        "name": "Claude models",
                        "buckets": [{
                            "id": "claude-5h",
                            "name": "5h Limit Remaining",
                            "window": "5h",
                            "remaining_fraction": 0.6,
                            "reset_time": "2026-11-26T17:00:00Z"
                        }]
                    }]
                }
            }
        }))
        .unwrap();
        assert_eq!(agw.len(), 1);
        assert!((agw[0].used_pct.unwrap() - 40.0).abs() < 1e-6);

        assert!(is_antigravity_usage_supported("agy 1.2.11"));
        assert!(is_antigravity_usage_supported("1.1.11"));
        assert!(!is_antigravity_usage_supported("agy 1.1.10"));
    }

    #[test]
    fn extracts_grok_token_from_legacy_and_nested_shapes() {
        assert_eq!(
            extract_grok_token(&json!({ "access_token": "tok-1" })).as_deref(),
            Some("tok-1")
        );
        assert_eq!(
            extract_grok_token(&json!({ "https://auth.x.ai::default": { "key": "tok-2" } }))
                .as_deref(),
            Some("tok-2")
        );
    }
}
