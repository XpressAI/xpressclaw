use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::{Config, RateLimitConfig};

/// Per-agent sliding window rate limiter.
///
/// Tracks requests and tokens per minute per agent. Each agent uses its own
/// rate limit config if set, otherwise falls back to the system default.
pub struct RateLimiter {
    config: std::sync::Arc<Config>,
    state: Mutex<HashMap<String, AgentRateState>>,
}

struct AgentRateState {
    /// Timestamps of recent requests (sliding window)
    request_times: Vec<Instant>,
    /// (timestamp, token_count) of recent requests
    token_counts: Vec<(Instant, u32)>,
}

impl AgentRateState {
    fn new() -> Self {
        Self {
            request_times: Vec::new(),
            token_counts: Vec::new(),
        }
    }

    fn prune(&mut self, window: std::time::Duration) {
        let cutoff = Instant::now() - window;
        self.request_times.retain(|t| *t > cutoff);
        self.token_counts.retain(|(t, _)| *t > cutoff);
    }

    fn request_count(&self) -> u32 {
        self.request_times.len() as u32
    }

    fn token_count(&self) -> u32 {
        self.token_counts.iter().map(|(_, c)| c).sum()
    }
}

#[derive(Debug)]
pub enum RateLimitResult {
    Allowed,
    RequestsExceeded { limit: u32, current: u32 },
    TokensExceeded { limit: u32, current: u32 },
}

impl RateLimiter {
    pub fn new(config: std::sync::Arc<Config>) -> Self {
        Self {
            config,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve the effective rate limit config for an agent.
    fn effective_config(&self, agent_id: &str) -> &RateLimitConfig {
        self.config
            .agents
            .iter()
            .find(|a| a.name == agent_id)
            .and_then(|a| a.rate_limit.as_ref())
            .unwrap_or(&self.config.system.rate_limit)
    }

    /// Check if a request is allowed for the given agent.
    /// Does NOT record the request — call `record_request` after the LLM call completes.
    pub fn check(&self, agent_id: &str) -> RateLimitResult {
        let rl = self.effective_config(agent_id);
        let window = std::time::Duration::from_secs(60);

        let mut state = self.state.lock().unwrap();
        let agent_state = state
            .entry(agent_id.to_string())
            .or_insert_with(AgentRateState::new);
        agent_state.prune(window);

        let req_count = agent_state.request_count();
        if req_count >= rl.requests_per_minute {
            return RateLimitResult::RequestsExceeded {
                limit: rl.requests_per_minute,
                current: req_count,
            };
        }

        let tok_count = agent_state.token_count();
        if tok_count >= rl.tokens_per_minute {
            return RateLimitResult::TokensExceeded {
                limit: rl.tokens_per_minute,
                current: tok_count,
            };
        }

        RateLimitResult::Allowed
    }

    /// Record a completed request for rate limiting.
    pub fn record_request(&self, agent_id: &str, tokens: u32) {
        let mut state = self.state.lock().unwrap();
        let agent_state = state
            .entry(agent_id.to_string())
            .or_insert_with(AgentRateState::new);
        let now = Instant::now();
        agent_state.request_times.push(now);
        agent_state.token_counts.push((now, tokens));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, Config};
    use std::sync::Arc;

    #[test]
    fn test_allows_within_limits() {
        let config = Arc::new(Config::default());
        let limiter = RateLimiter::new(config);

        assert!(matches!(limiter.check("atlas"), RateLimitResult::Allowed));
        limiter.record_request("atlas", 100);
        assert!(matches!(limiter.check("atlas"), RateLimitResult::Allowed));
    }

    #[test]
    fn test_blocks_when_requests_exceeded() {
        let mut config = Config::default();
        config.system.rate_limit.requests_per_minute = 2;
        let limiter = RateLimiter::new(Arc::new(config));

        limiter.record_request("atlas", 10);
        limiter.record_request("atlas", 10);

        assert!(matches!(
            limiter.check("atlas"),
            RateLimitResult::RequestsExceeded { .. }
        ));
    }

    #[test]
    fn test_blocks_when_tokens_exceeded() {
        let mut config = Config::default();
        config.system.rate_limit.tokens_per_minute = 100;
        let limiter = RateLimiter::new(Arc::new(config));

        limiter.record_request("atlas", 101);

        assert!(matches!(
            limiter.check("atlas"),
            RateLimitResult::TokensExceeded { .. }
        ));
    }

    #[test]
    fn test_per_agent_override() {
        let mut config = Config::default();
        config.system.rate_limit.requests_per_minute = 100;
        config.agents = vec![AgentConfig {
            name: "atlas".to_string(),
            rate_limit: Some(RateLimitConfig {
                requests_per_minute: 2,
                tokens_per_minute: 1000,
                concurrent_requests: 1,
            }),
            ..Default::default()
        }];
        let limiter = RateLimiter::new(Arc::new(config));

        // atlas has a 2 req/min limit
        limiter.record_request("atlas", 10);
        limiter.record_request("atlas", 10);
        assert!(matches!(
            limiter.check("atlas"),
            RateLimitResult::RequestsExceeded { .. }
        ));

        // hermes uses system default (100 req/min), still allowed
        limiter.record_request("hermes", 10);
        limiter.record_request("hermes", 10);
        assert!(matches!(limiter.check("hermes"), RateLimitResult::Allowed));
    }

    #[test]
    fn test_agents_are_independent() {
        let config = Arc::new(Config::default());
        let limiter = RateLimiter::new(config);

        limiter.record_request("atlas", 100);
        // hermes is unaffected by atlas's usage
        assert!(matches!(limiter.check("hermes"), RateLimitResult::Allowed));
    }
}
