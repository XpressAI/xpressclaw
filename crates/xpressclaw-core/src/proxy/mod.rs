//! Instance-owned HTTP(S) egress policy for native runners.
pub mod service;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProxyMode {
    #[default]
    Inherit,
    Direct,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainPolicy {
    #[default]
    AllowExceptBlocked,
    AllowListedOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderMatch {
    #[default]
    Any,
    Exact,
    Contains,
    Regex,
    Email,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderAction {
    #[default]
    Remove,
    Replace,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderRule {
    pub header: String,
    pub matcher: HeaderMatch,
    pub value: String,
    pub action: HeaderAction,
    pub replacement: String,
    pub domains: Vec<String>,
}

impl Default for HeaderRule {
    fn default() -> Self {
        Self {
            header: "User-Agent".into(),
            matcher: HeaderMatch::Email,
            value: String::new(),
            action: HeaderAction::Replace,
            replacement: "XpressCLAW/1.0".into(),
            domains: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub domain_policy: DomainPolicy,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub inspection_bypass: Vec<String>,
    pub header_rules: Vec<HeaderRule>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domain_policy: DomainPolicy::default(),
            allowed_domains: vec![],
            blocked_domains: vec![],
            inspection_bypass: vec![],
            header_rules: vec![
                HeaderRule::default(),
                HeaderRule {
                    header: "From".into(),
                    matcher: HeaderMatch::Any,
                    action: HeaderAction::Remove,
                    replacement: String::new(),
                    ..HeaderRule::default()
                },
            ],
        }
    }
}

impl ProxyConfig {
    /// Existing installations activate the new service explicitly in Settings.
    pub fn legacy_default() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn applies_to(&self, mode: AgentProxyMode) -> bool {
        self.enabled && mode == AgentProxyMode::Inherit
    }

    pub fn validate(&self) -> Result<(), String> {
        for domains in [
            &self.allowed_domains,
            &self.blocked_domains,
            &self.inspection_bypass,
        ] {
            validate_domains(domains)?;
        }
        if self.header_rules.len() > 100 {
            return Err("At most 100 header rules are supported".into());
        }
        for (index, rule) in self.header_rules.iter().enumerate() {
            let prefix = format!("Header rule {}", index + 1);
            if rule.header.is_empty()
                || !rule
                    .header
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            {
                return Err(format!("{prefix}: enter a valid HTTP header name"));
            }
            // Routing and framing are owned by the HTTP implementation.
            if [
                "host",
                "connection",
                "content-length",
                "transfer-encoding",
                "upgrade",
                "trailer",
                "te",
                "proxy-authorization",
                "proxy-authenticate",
            ]
            .contains(&rule.header.to_ascii_lowercase().as_str())
            {
                return Err(format!(
                    "{prefix}: routing, framing and proxy authentication headers cannot be edited"
                ));
            }
            if rule.value.len() > 1024
                || rule.replacement.len() > 4096
                || rule
                    .replacement
                    .chars()
                    .any(|c| c.is_control() && c != '\t')
            {
                return Err(format!("{prefix}: invalid or oversized header value"));
            }
            if rule.matcher == HeaderMatch::Regex {
                regex::Regex::new(&rule.value)
                    .map_err(|_| format!("{prefix}: invalid regular expression"))?;
                // Keep the accepted syntax compatible with Python's bounded matcher.
                if ["\\p", "\\P", "\\z", "(?<", "(?U", "(?-u", "&&", "--", "~~"]
                    .iter()
                    .any(|token| rule.value.contains(token))
                {
                    return Err(format!(
                        "{prefix}: use a regular expression compatible with Python re"
                    ));
                }
            }
            validate_domains(&rule.domains)?;
        }
        Ok(())
    }
}

fn validate_domains(domains: &[String]) -> Result<(), String> {
    if domains.len() > 1000 {
        return Err("At most 1000 domains are supported per list".into());
    }
    for pattern in domains {
        let host = pattern.strip_prefix("*.").unwrap_or(pattern);
        if host.is_empty()
            || host.len() > 253
            || host != host.trim()
            || host.ends_with('.')
            || host.contains(['/', ':', '@', '?', '#', '*'])
        {
            return Err(format!(
                "Invalid domain {pattern:?}; use a hostname or *.example.com"
            ));
        }
        if !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) {
            return Err(format!("Invalid domain {pattern:?}; use ASCII hostnames (punycode for international names)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_defaults_and_exceptions() {
        let config = ProxyConfig::default();
        assert!(config.applies_to(AgentProxyMode::Inherit));
        assert!(!config.applies_to(AgentProxyMode::Direct));
        assert!(!ProxyConfig::legacy_default().applies_to(AgentProxyMode::Inherit));
        config.validate().unwrap();
    }
    #[test]
    fn rejects_ambiguous_domains_and_http_framing_changes() {
        for domain in [
            "*",
            "https://example.com",
            "example.com:443",
            "bad..host",
            "foo*bar.com",
            "example.com/",
        ] {
            let config = ProxyConfig {
                blocked_domains: vec![domain.into()],
                ..Default::default()
            };
            assert!(config.validate().is_err(), "{domain}");
        }
        let mut config = ProxyConfig {
            allowed_domains: vec!["example.com".into(), "*.example.com".into()],
            ..Default::default()
        };
        config.validate().unwrap();
        config.header_rules[0].header = "Content-Length".into();
        assert!(config.validate().is_err());
        config.header_rules[0].header = "User-Agent".into();
        config.header_rules[0].replacement = "safe\r\nInjected: bad".into();
        assert!(config.validate().is_err());
    }
    #[test]
    fn configuration_roundtrip_and_legacy_activation() {
        use crate::config::{AgentConfig, Config, NativeRunnerConfig};
        let legacy: Config = serde_yaml::from_str("agents: []").unwrap();
        assert!(!legacy.proxy.enabled);
        let mut config = Config::default();
        assert!(config.proxy.enabled);
        config.agents.push(AgentConfig {
            runner: NativeRunnerConfig {
                proxy: AgentProxyMode::Direct,
                ..Default::default()
            },
            ..Default::default()
        });
        let loaded: Config =
            serde_yaml::from_str(&serde_yaml::to_string(&config).unwrap()).unwrap();
        assert!(loaded.proxy.enabled);
        assert_eq!(loaded.agents[0].runner.proxy, AgentProxyMode::Direct);
    }
}
