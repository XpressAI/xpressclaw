//! xclaw shell-bridge protocol (ADR-023 §7, task 5).
//!
//! The `xclaw` CLI is mounted inside every non-MCP harness workspace.
//! Agents invoke it like any other shell tool — `xclaw memory add "note"`
//! — and it proxies the request to the xpressclaw server over a Unix
//! socket. This module defines the wire format shared by the CLI
//! (client) and the server-side bridge listener.
//!
//! Design goals:
//!
//! - **Trivial on the wire.** One line of JSON per message, newline-
//!   terminated, no multiplexing. Connect → request → response → close.
//!   Debuggable with `socat` and `nc`.
//! - **Shell-verb native.** Verbs read like sentences
//!   (`memory.add`, `task.update.status`). Arguments are a flat JSON
//!   object. Structured results are JSON objects; simple results are
//!   strings.
//! - **Not MCP.** Pi and future shell-native harnesses don't speak MCP
//!   (that's why we need this bridge at all). Under the hood the server
//!   can route to MCP tools where appropriate, but the wire format
//!   stays shell-friendly.

use serde::{Deserialize, Serialize};

/// One request over the xclaw socket.
///
/// The CLI builds this from its argv. Verbs use dot-separated naming
/// (`memory.add`, `task.update.status`) so the server can route by
/// prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XclawRequest {
    /// Dot-separated verb path (e.g. `"memory.add"`, `"log"`).
    pub verb: String,
    /// Flat argument object. Verbs define their own schema.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Agent invoking this verb. Populated by PiHarness (via env var)
    /// on each CLI run so the server can scope state per-agent.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// One response over the xclaw socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XclawResponse {
    /// Success flag. On `false`, inspect `error`.
    pub ok: bool,
    /// Verb-specific result payload on success. `null` is valid (e.g.
    /// for verbs that return "command accepted" with no data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Human-readable error message on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl XclawResponse {
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(msg.into()),
        }
    }
}

/// Environment variable the CLI reads to locate its socket. PiHarness
/// sets this on every launch; developers running the CLI manually can
/// set it too.
pub const XCLAW_SOCKET_ENV: &str = "XCLAW_SOCKET";

/// Environment variable the CLI reads for `agent_id` attribution.
/// PiHarness sets this; manual invocations may leave it unset, in which
/// case the server applies a sensible default ("cli" or the caller
/// provides `--agent-id`).
pub const XCLAW_AGENT_ENV: &str = "XCLAW_AGENT_ID";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request() {
        let req = XclawRequest {
            verb: "memory.add".to_string(),
            args: serde_json::json!({ "content": "hi", "tags": ["x"] }),
            agent_id: Some("atlas".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: XclawRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.verb, "memory.add");
        assert_eq!(back.agent_id.as_deref(), Some("atlas"));
    }

    #[test]
    fn response_success_and_failure_shapes() {
        let ok = XclawResponse::success(serde_json::json!({ "id": "mem-1" }));
        assert!(ok.ok);
        assert!(ok.error.is_none());
        assert!(ok.result.is_some());

        let err = XclawResponse::failure("nope");
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("nope"));
        assert!(err.result.is_none());
    }
}
