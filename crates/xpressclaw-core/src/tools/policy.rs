use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// What action to take when a tool matches a policy rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Deny,
    RequireApproval,
}

/// How approval should be obtained.
///
/// **Security note:** approval handlers intentionally receive only metadata
/// (tool name, agent ID) — never the tool arguments or conversation context.
/// This prevents prompt injection attacks where a malicious prompt tries to
/// manipulate the approval process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ApprovalMode {
    /// Human must approve via the web UI or API.
    Manual,
    /// Run a shell script. Exit 0 = approve, non-0 = deny.
    /// Env vars: TOOL_NAME, AGENT_ID (no arguments — prevents prompt injection).
    Script { command: String },
    /// Ask another agent to approve/deny (future).
    Agent { agent_id: String },
}

/// A single policy rule with a glob pattern.
///
/// Rules are evaluated in order — first match wins. If no rule matches,
/// the tool call is allowed by default.
///
/// Example YAML:
/// ```yaml
/// tool_policies:
///   - pattern: "dangerous_*"
///     action: deny
///   - pattern: "github__*"
///     action: allow
///   - pattern: "*"
///     action: require_approval
///     approval:
///       type: script
///       command: /usr/local/bin/approve-tool
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicyRule {
    /// Glob pattern matching tool names (e.g., `foo*`, `github__*`, `*`).
    pub pattern: String,
    /// What to do when matched.
    pub action: PolicyAction,
    /// How to approve (only used when action is `require_approval`).
    #[serde(default)]
    pub approval: Option<ApprovalMode>,
}

/// Result of evaluating a tool call against policies.
#[derive(Debug)]
pub enum PolicyDecision {
    /// Tool call is allowed — proceed with execution.
    Allow,
    /// Tool call is denied outright.
    Deny { reason: String },
    /// Tool call needs approval before it can execute.
    NeedsApproval {
        mode: ApprovalMode,
        matched_pattern: String,
    },
}

/// Evaluates tool calls against pattern-based policy rules.
///
/// Rules are evaluated in order — first match wins. If no rule matches,
/// the tool call is allowed by default (same behavior as the existing
/// ToolRegistry permissions).
///
/// Policy evaluation happens *before* ToolRegistry permission checks,
/// giving the admin a coarse-grained control layer on top of the
/// per-agent fine-grained permissions.
pub struct ToolPolicyEngine {
    rules: Vec<ToolPolicyRule>,
}

impl ToolPolicyEngine {
    pub fn new(rules: Vec<ToolPolicyRule>) -> Self {
        if !rules.is_empty() {
            info!(count = rules.len(), "loaded tool policy rules");
        }
        Self { rules }
    }

    /// Evaluate a tool call against the policy rules.
    /// First matching rule wins. No match = allow.
    pub fn evaluate(&self, tool_name: &str, agent_id: &str) -> PolicyDecision {
        for rule in &self.rules {
            if glob_match(&rule.pattern, tool_name) {
                debug!(
                    tool = tool_name,
                    agent = agent_id,
                    pattern = rule.pattern,
                    action = ?rule.action,
                    "policy rule matched"
                );
                return match &rule.action {
                    PolicyAction::Allow => PolicyDecision::Allow,
                    PolicyAction::Deny => PolicyDecision::Deny {
                        reason: format!(
                            "denied by policy: pattern '{}' blocks tool '{}'",
                            rule.pattern, tool_name
                        ),
                    },
                    PolicyAction::RequireApproval => {
                        let mode = rule.approval.clone().unwrap_or(ApprovalMode::Manual);
                        PolicyDecision::NeedsApproval {
                            mode,
                            matched_pattern: rule.pattern.clone(),
                        }
                    }
                };
            }
        }

        // No rule matched — allow by default
        PolicyDecision::Allow
    }

    /// Run a script-based approval check.
    ///
    /// The script receives only metadata as env vars — **never** the tool
    /// arguments or conversation context. This prevents prompt injection:
    /// a malicious prompt cannot influence the approval decision.
    ///
    /// Env vars provided to the script:
    /// - `TOOL_NAME` — the tool being called
    /// - `AGENT_ID` — the agent making the call
    ///
    /// Returns `Ok(true)` if approved (exit 0), `Ok(false)` if denied (non-zero),
    /// or `Err` if the script failed to execute.
    pub async fn run_approval_script(
        command: &str,
        tool_name: &str,
        agent_id: &str,
    ) -> std::result::Result<bool, String> {
        use tokio::process::Command;

        info!(
            command,
            tool = tool_name,
            agent = agent_id,
            "running approval script"
        );

        #[cfg(windows)]
        let output_future = Command::new("cmd")
            .args(["/C", command])
            .env("TOOL_NAME", tool_name)
            .env("AGENT_ID", agent_id)
            .stdin(std::process::Stdio::null())
            .output();

        #[cfg(not(windows))]
        let output_future = Command::new("sh")
            .args(["-c", command])
            .env("TOOL_NAME", tool_name)
            .env("AGENT_ID", agent_id)
            .stdin(std::process::Stdio::null())
            .output();

        let output = output_future
            .await
            .map_err(|e| format!("failed to run approval script '{command}': {e}"))?;

        let approved = output.status.success();
        if !approved {
            let stderr = String::from_utf8_lossy(&output.stderr);
            info!(
                tool = tool_name,
                agent = agent_id,
                exit_code = output.status.code(),
                stderr = %stderr,
                "approval script denied tool call"
            );
        }

        Ok(approved)
    }

    pub fn rules(&self) -> &[ToolPolicyRule] {
        &self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl Default for ToolPolicyEngine {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Simple glob matching: `*` matches zero or more characters, `?` matches exactly one.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = pattern.as_bytes();
    let txt = text.as_bytes();

    let mut pi = 0; // pattern index
    let mut ti = 0; // text index
    let mut star_pi = None; // position after last * in pattern
    let mut star_ti = 0; // text position to retry from after * match

    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    // Consume trailing *'s in pattern
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }

    pi == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── glob matching ──

    #[test]
    fn test_glob_exact() {
        assert!(glob_match("hello", "hello"));
        assert!(!glob_match("hello", "world"));
        assert!(!glob_match("hello", "hell"));
        assert!(!glob_match("hell", "hello"));
    }

    #[test]
    fn test_glob_star() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
        assert!(glob_match("foo*", "foo"));
        assert!(glob_match("foo*", "foobar"));
        assert!(glob_match("foo*", "foo_bar_baz"));
        assert!(!glob_match("foo*", "bar"));
        assert!(glob_match("*bar", "foobar"));
        assert!(glob_match("*bar", "bar"));
        assert!(!glob_match("*bar", "barz"));
    }

    #[test]
    fn test_glob_star_middle() {
        assert!(glob_match("foo*baz", "foobaz"));
        assert!(glob_match("foo*baz", "foobarbaz"));
        assert!(!glob_match("foo*baz", "foobar"));
    }

    #[test]
    fn test_glob_question_mark() {
        assert!(glob_match("fo?", "foo"));
        assert!(glob_match("fo?", "fob"));
        assert!(!glob_match("fo?", "fo"));
        assert!(!glob_match("fo?", "fooo"));
    }

    #[test]
    fn test_glob_double_underscore_pattern() {
        assert!(glob_match("github__*", "github__create_issue"));
        assert!(glob_match("github__*", "github__list_repos"));
        assert!(!glob_match("github__*", "slack__send_message"));
    }

    #[test]
    fn test_glob_multiple_stars() {
        assert!(glob_match("*__*", "github__create_issue"));
        assert!(glob_match("*__*", "anything__anything"));
        assert!(!glob_match("*__*", "no_double_underscore"));
    }

    // ── policy evaluation ──

    #[test]
    fn test_no_rules_allows_all() {
        let engine = ToolPolicyEngine::new(vec![]);
        assert!(matches!(
            engine.evaluate("any_tool", "any_agent"),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn test_deny_rule() {
        let engine = ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "dangerous_*".into(),
            action: PolicyAction::Deny,
            approval: None,
        }]);

        assert!(matches!(
            engine.evaluate("dangerous_delete_all", "atlas"),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            engine.evaluate("safe_tool", "atlas"),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn test_allow_rule() {
        let engine = ToolPolicyEngine::new(vec![
            ToolPolicyRule {
                pattern: "github__*".into(),
                action: PolicyAction::Allow,
                approval: None,
            },
            ToolPolicyRule {
                pattern: "*".into(),
                action: PolicyAction::Deny,
                approval: None,
            },
        ]);

        // github tools allowed by first rule
        assert!(matches!(
            engine.evaluate("github__create_issue", "atlas"),
            PolicyDecision::Allow
        ));
        // everything else denied by catch-all
        assert!(matches!(
            engine.evaluate("slack__send_message", "atlas"),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn test_first_match_wins() {
        let engine = ToolPolicyEngine::new(vec![
            ToolPolicyRule {
                pattern: "github__delete_*".into(),
                action: PolicyAction::Deny,
                approval: None,
            },
            ToolPolicyRule {
                pattern: "github__*".into(),
                action: PolicyAction::Allow,
                approval: None,
            },
        ]);

        // delete blocked by first rule
        assert!(matches!(
            engine.evaluate("github__delete_repo", "atlas"),
            PolicyDecision::Deny { .. }
        ));
        // other github tools allowed by second rule
        assert!(matches!(
            engine.evaluate("github__create_issue", "atlas"),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn test_require_approval_manual() {
        let engine = ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "*".into(),
            action: PolicyAction::RequireApproval,
            approval: None, // defaults to Manual
        }]);

        match engine.evaluate("any_tool", "atlas") {
            PolicyDecision::NeedsApproval { mode, .. } => {
                assert!(matches!(mode, ApprovalMode::Manual));
            }
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
    }

    #[test]
    fn test_require_approval_script() {
        let engine = ToolPolicyEngine::new(vec![ToolPolicyRule {
            pattern: "*".into(),
            action: PolicyAction::RequireApproval,
            approval: Some(ApprovalMode::Script {
                command: "/usr/local/bin/approve".into(),
            }),
        }]);

        match engine.evaluate("tool", "atlas") {
            PolicyDecision::NeedsApproval { mode, .. } => match mode {
                ApprovalMode::Script { command } => {
                    assert_eq!(command, "/usr/local/bin/approve");
                }
                other => panic!("expected Script, got {other:?}"),
            },
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
    }

    #[test]
    fn test_policy_rule_serde_yaml() {
        let yaml = r#"
- pattern: "dangerous_*"
  action: deny
- pattern: "github__*"
  action: allow
- pattern: "*"
  action: require_approval
  approval:
    type: script
    command: /usr/local/bin/approve-tool
"#;
        let rules: Vec<ToolPolicyRule> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].action, PolicyAction::Deny);
        assert_eq!(rules[1].action, PolicyAction::Allow);
        assert_eq!(rules[2].action, PolicyAction::RequireApproval);
        match &rules[2].approval {
            Some(ApprovalMode::Script { command }) => {
                assert_eq!(command, "/usr/local/bin/approve-tool");
            }
            other => panic!("expected Script, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_approval_script_approve() {
        // Command that exits 0 = approved
        #[cfg(windows)]
        let cmd = "cmd /C exit 0";
        #[cfg(not(windows))]
        let cmd = "true";
        let result = ToolPolicyEngine::run_approval_script(cmd, "test_tool", "atlas").await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_approval_script_deny() {
        // Command that exits 1 = denied
        #[cfg(windows)]
        let cmd = "cmd /C exit 1";
        #[cfg(not(windows))]
        let cmd = "false";
        let result = ToolPolicyEngine::run_approval_script(cmd, "test_tool", "atlas").await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_approval_script_receives_env_vars() {
        #[cfg(windows)]
        let script =
            r#"if not "%TOOL_NAME%"=="my_tool" exit /b 1 & if not "%AGENT_ID%"=="atlas" exit /b 1"#;
        #[cfg(not(windows))]
        let script = r#"test "$TOOL_NAME" = "my_tool" && test "$AGENT_ID" = "atlas""#;

        // Correct values should pass
        let result = ToolPolicyEngine::run_approval_script(script, "my_tool", "atlas").await;
        assert!(result.unwrap());

        // Wrong values should fail
        let result = ToolPolicyEngine::run_approval_script(script, "wrong_tool", "atlas").await;
        assert!(!result.unwrap());
    }
}
