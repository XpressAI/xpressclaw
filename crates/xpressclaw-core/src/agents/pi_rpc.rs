//! Pi agent RPC client (ADR-003 rewrite).
//!
//! Spawns `pi-coding-agent` running inside a container2wasm WASM image
//! (via `c2w-net -invoke`) and talks to it over JSONL on stdin/stdout.
//!
//! The subprocess chain is:
//!
//!     xpressclaw  ──stdin──▶  c2w-net -invoke pi-agent.wasm --net=socket
//!                              │
//!                              └─▶ wasmtime (patched Bochs) boots Linux
//!                                  ├─ entrypoint.sh mounts mcpfs
//!                                  └─ pi --mode rpc (reads stdin, writes stdout)
//!
//! We send prompts as:
//!     {"type":"prompt","message":"..."}
//!
//! Pi streams back event lines like:
//!     {"type":"agent_start"}
//!     {"type":"message_update","assistantMessageEvent":{"type":"thinking_delta", ...}}
//!     {"type":"message_update","assistantMessageEvent":{"type":"text_delta", ...}}
//!     {"type":"tool_execution_start","tool":"...","params":{...}}
//!     {"type":"tool_execution_end","tool":"...","result":...}
//!     {"type":"agent_end", ...}
//!
//! Network: host services are reachable at `192.168.127.254` from inside
//! the container (c2w-net's NAT gateway).

use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// Configuration for launching a pi-agent WASM container.
#[derive(Debug, Clone)]
pub struct PiLaunchConfig {
    /// Path to c2w-net binary.
    pub c2w_net: String,
    /// Path to the pi-agent.wasm image.
    pub wasm_path: String,
    /// Path to the wasmtime-shim that injects --env flags.
    /// Its directory is prepended to PATH.
    pub wasmtime_shim: String,
    /// xpressclaw MCP server URL the container will mount via mcpfs.
    /// Host's own URL as seen from the container (e.g. http://192.168.127.254:9101).
    pub xpressclaw_url: String,
    /// Local LLM base URL as seen from the container
    /// (e.g. http://192.168.127.254:8081/v1).
    pub llm_url: String,
    /// API key the custom provider extension sends (defaults to "opensesame").
    pub llm_key: String,
    /// Model id the custom provider exposes.
    pub llm_model: String,
    /// Agent id, passed through to entrypoint as AGENT_ID.
    pub agent_id: String,
}

impl PiLaunchConfig {
    /// Defaults resolved relative to the repo root.
    ///
    /// Caller may override after load.
    pub fn defaults_for(agent_id: &str) -> Self {
        Self {
            c2w_net: "c2w-net".into(),
            wasm_path: "wasm-agents/pi-agent.wasm".into(),
            wasmtime_shim: "wasm-agents/wasmtime-shim".into(),
            xpressclaw_url: "http://192.168.127.254:8935".into(),
            llm_url: "http://192.168.127.254:8081/v1".into(),
            llm_key: "opensesame".into(),
            llm_model: "local".into(),
            agent_id: agent_id.to_string(),
        }
    }
}

/// A single pi event emitted over stdout (parsed leniently —
/// unknown fields are ignored).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum PiEvent {
    #[serde(rename = "response")]
    Response {
        command: Option<String>,
        success: bool,
        #[serde(default)]
        error: Option<String>,
    },
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "agent_end")]
    AgentEnd,
    #[serde(rename = "turn_start")]
    TurnStart,
    #[serde(rename = "turn_end")]
    TurnEnd {
        #[serde(default)]
        message: Option<serde_json::Value>,
    },
    #[serde(rename = "message_start")]
    MessageStart {
        #[serde(default)]
        message: Option<serde_json::Value>,
    },
    #[serde(rename = "message_end")]
    MessageEnd {
        #[serde(default)]
        message: Option<serde_json::Value>,
    },
    #[serde(rename = "message_update")]
    MessageUpdate {
        #[serde(default, rename = "assistantMessageEvent")]
        inner: Option<serde_json::Value>,
    },
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        params: Option<serde_json::Value>,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        result: Option<serde_json::Value>,
    },
    #[serde(rename = "auto_retry_start")]
    AutoRetryStart {
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default, rename = "errorMessage")]
        error: Option<String>,
    },
    #[serde(rename = "auto_retry_end")]
    AutoRetryEnd {
        #[serde(default)]
        success: bool,
        #[serde(default, rename = "finalError")]
        final_error: Option<String>,
    },
    /// Fallback for events we don't enumerate — still carries the raw value.
    #[serde(other)]
    Other,
}

/// What a pi turn produced, after stdout events have been drained.
#[derive(Debug, Default)]
pub struct PiTurnResult {
    /// Concatenated text deltas (user-visible response).
    pub text: String,
    /// Concatenated thinking deltas (reasoning).
    pub thinking: String,
    /// Total token count reported by pi (if any).
    pub tokens: i64,
    /// Final error message if the turn failed.
    pub error: Option<String>,
    /// Model id pi reported using.
    pub model: Option<String>,
}

/// Outgoing RPC command sent on stdin.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PiCommand<'a> {
    Prompt { message: &'a str },
}

/// A running pi subprocess with a mutex around its stdin.
pub struct PiProcess {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    /// Parsed stdout events funnel through this channel.
    rx: Arc<Mutex<tokio::sync::mpsc::Receiver<PiEvent>>>,
    agent_id: String,
}

impl PiProcess {
    /// Spawn the pi WASM container and wire up stdin/stdout.
    pub async fn spawn(cfg: &PiLaunchConfig) -> Result<Self> {
        let shim_dir = std::path::Path::new(&cfg.wasmtime_shim)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".into());
        let current_path = std::env::var("PATH").unwrap_or_default();
        let path = format!("{shim_dir}:{current_path}");

        let extra_env = format!(
            "LLM_PROVIDER=xpressclaw LLM_MODEL={} \
             XPRESSCLAW_LLM_URL={} XPRESSCLAW_LLM_KEY={} \
             XPRESSCLAW_URL={} AGENT_ID={}",
            cfg.llm_model, cfg.llm_url, cfg.llm_key, cfg.xpressclaw_url, cfg.agent_id,
        );

        info!(
            agent_id = %cfg.agent_id,
            wasm = %cfg.wasm_path,
            "spawning pi-agent WASM container"
        );

        let mut child = Command::new(&cfg.c2w_net)
            .arg("-invoke")
            .arg(&cfg.wasm_path)
            .arg("--net=socket")
            .env("PATH", &path)
            .env("WASMTIME_EXTRA_ENV", &extra_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Agent(format!("failed to spawn c2w-net: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Agent("no stdin on pi child".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Agent("no stdout on pi child".into()))?;
        let stderr = child.stderr.take();

        let (tx, rx) = tokio::sync::mpsc::channel::<PiEvent>(256);
        let agent_id = cfg.agent_id.clone();

        // Stdout reader task — parses JSONL.
        let tx_out = tx.clone();
        let agent_id_log = agent_id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.starts_with('{') {
                    // mcpfs banner, etc. — log and skip
                    debug!(agent_id = %agent_id_log, "pi stdout: {trimmed}");
                    continue;
                }
                match serde_json::from_str::<PiEvent>(trimmed) {
                    Ok(ev) => {
                        if tx_out.send(ev).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(agent_id = %agent_id_log, error = %e, "malformed pi event: {trimmed}");
                    }
                }
            }
            debug!(agent_id = %agent_id_log, "pi stdout reader exit");
        });

        // Stderr reader task — just logs.
        if let Some(stderr) = stderr {
            let agent_id_log = agent_id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!(agent_id = %agent_id_log, "pi stderr: {line}");
                }
            });
        }

        Ok(Self {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            rx: Arc::new(Mutex::new(rx)),
            agent_id,
        })
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Send a prompt and stream events through the callback until `agent_end`
    /// or `auto_retry_end { success: false }`. Returns the final turn result.
    pub async fn send_prompt<F>(&self, message: &str, mut on_event: F) -> Result<PiTurnResult>
    where
        F: FnMut(&PiEvent) + Send,
    {
        let cmd = PiCommand::Prompt { message };
        let line = format!(
            "{}\n",
            serde_json::to_string(&cmd).map_err(|e| Error::Agent(e.to_string()))?
        );

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| Error::Agent(format!("write prompt failed: {e}")))?;
            stdin
                .flush()
                .await
                .map_err(|e| Error::Agent(format!("flush prompt failed: {e}")))?;
        }

        let mut result = PiTurnResult::default();
        let mut rx = self.rx.lock().await;

        while let Some(ev) = rx.recv().await {
            on_event(&ev);
            match &ev {
                PiEvent::Response {
                    command,
                    success,
                    error,
                } if command.as_deref() == Some("prompt") => {
                    if !success {
                        result.error = error.clone();
                        // Response with success=false on a prompt means pi
                        // rejected the input outright; no agent_end follows.
                        return Ok(result);
                    }
                }
                PiEvent::MessageUpdate { inner: Some(inner) } => {
                    let ev_type = inner.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let partial = inner.get("partial");
                    if ev_type == "text_delta" {
                        if let Some(delta) = inner.get("delta").and_then(|v| v.as_str()) {
                            result.text.push_str(delta);
                        } else if let Some(content) = partial
                            .and_then(|p| p.get("content"))
                            .and_then(|c| c.as_array())
                        {
                            for item in content {
                                if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                                    if let Some(s) = item.get("text").and_then(|v| v.as_str()) {
                                        if s.len() > result.text.len() {
                                            result.text = s.to_string();
                                        }
                                    }
                                }
                            }
                        }
                    } else if ev_type == "thinking_delta" {
                        if let Some(delta) = inner.get("delta").and_then(|v| v.as_str()) {
                            result.thinking.push_str(delta);
                        } else if let Some(content) = partial
                            .and_then(|p| p.get("content"))
                            .and_then(|c| c.as_array())
                        {
                            for item in content {
                                if item.get("type").and_then(|v| v.as_str()) == Some("thinking") {
                                    if let Some(s) = item.get("thinking").and_then(|v| v.as_str())
                                    {
                                        if s.len() > result.thinking.len() {
                                            result.thinking = s.to_string();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                PiEvent::MessageEnd { message: Some(msg) } => {
                    if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                        result.model = Some(m.to_string());
                    }
                    if let Some(tokens) = msg
                        .get("usage")
                        .and_then(|u| u.get("totalTokens"))
                        .and_then(|v| v.as_i64())
                    {
                        result.tokens = tokens;
                    }
                    if let Some(err) = msg.get("errorMessage").and_then(|v| v.as_str()) {
                        result.error = Some(err.to_string());
                    }
                }
                PiEvent::AgentEnd => {
                    break;
                }
                PiEvent::AutoRetryEnd {
                    success: false,
                    final_error,
                } => {
                    result.error = final_error.clone().or_else(|| result.error.clone());
                    break;
                }
                _ => {}
            }
        }

        Ok(result)
    }

    /// Force-kill the container. Consumes self.
    pub async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Map a `PiTurnResult` into a store-ready response string.
pub fn turn_to_stored_content(turn: &PiTurnResult) -> String {
    if let Some(err) = &turn.error {
        return format!("(pi error: {err})");
    }
    if !turn.text.is_empty() {
        turn.text.clone()
    } else if !turn.thinking.is_empty() {
        format!("<think>{}</think>", turn.thinking)
    } else {
        "(No response)".to_string()
    }
}
