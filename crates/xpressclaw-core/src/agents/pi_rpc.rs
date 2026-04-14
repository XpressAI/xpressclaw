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

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agents::pi_terminal::PiTerminalBus;
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

/// A tool invocation pi made during the turn.
#[derive(Debug, Clone)]
pub struct PiToolCall {
    pub name: String,
    pub params: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub is_error: bool,
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
    /// Tool calls pi made and their results.
    pub tool_calls: Vec<PiToolCall>,
}

/// Outgoing RPC command sent on stdin.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PiCommand<'a> {
    Prompt { message: &'a str },
}

/// A running pi subprocess with a mutex around its stdin.
pub struct PiProcess {
    child: Mutex<Option<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    /// Parsed stdout events funnel through this channel.
    rx: Arc<Mutex<tokio::sync::mpsc::Receiver<PiEvent>>>,
    agent_id: String,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl PiProcess {
    /// Spawn the pi WASM container without broadcasting terminal output.
    pub async fn spawn(cfg: &PiLaunchConfig) -> Result<Self> {
        Self::spawn_with_terminal(cfg, None).await
    }

    /// Spawn the pi WASM container and wire up stdin/stdout.
    /// If `terminal` is Some, every stdout/stderr line is also published
    /// to that bus keyed by agent_id for the Logs tab.
    pub async fn spawn_with_terminal(
        cfg: &PiLaunchConfig,
        terminal: Option<Arc<PiTerminalBus>>,
    ) -> Result<Self> {
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
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        // Stdout reader task — parses JSONL and mirrors to the terminal bus.
        let tx_out = tx.clone();
        let agent_id_log = agent_id.clone();
        let alive_out = alive.clone();
        let term_stdout = terminal.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(bus) = term_stdout.as_ref() {
                    bus.publish(&agent_id_log, "stdout", line.clone()).await;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.starts_with('{') {
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
            alive_out.store(false, std::sync::atomic::Ordering::SeqCst);
            debug!(agent_id = %agent_id_log, "pi stdout reader exit");
        });

        // Stderr reader task — log + mirror to terminal bus.
        if let Some(stderr) = stderr {
            let agent_id_log = agent_id.clone();
            let term_stderr = terminal.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(bus) = term_stderr.as_ref() {
                        bus.publish(&agent_id_log, "stderr", line.clone()).await;
                    }
                    debug!(agent_id = %agent_id_log, "pi stderr: {line}");
                }
            });
        }

        Ok(Self {
            child: Mutex::new(Some(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            rx: Arc::new(Mutex::new(rx)),
            agent_id,
            alive,
        })
    }

    /// Whether the underlying subprocess is still running (best-effort).
    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::SeqCst)
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
                PiEvent::ToolExecutionStart { tool, params } => {
                    result.tool_calls.push(PiToolCall {
                        name: tool.clone().unwrap_or_default(),
                        params: params.clone().unwrap_or(serde_json::Value::Null),
                        result: None,
                        is_error: false,
                    });
                }
                PiEvent::ToolExecutionEnd { tool, result: res } => {
                    let is_err = res
                        .as_ref()
                        .and_then(|v| v.get("isError"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // Match by name to the last in-flight call with no result.
                    let target = tool.as_deref();
                    if let Some(tc) = result
                        .tool_calls
                        .iter_mut()
                        .rev()
                        .find(|tc| tc.result.is_none() && target.map(|t| t == tc.name).unwrap_or(true))
                    {
                        tc.result = res.clone();
                        tc.is_error = is_err;
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

    /// Force-kill the container. Safe to call multiple times.
    pub async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.alive.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Pool of long-lived pi subprocesses keyed by agent_id.
///
/// On each request, re-uses the cached process if alive, otherwise spawns
/// a fresh one. Amortizes the ~30s Bochs boot over many prompts.
#[derive(Clone, Default)]
pub struct PiPool {
    inner: Arc<RwLock<HashMap<String, Arc<PiProcess>>>>,
    terminal: Option<Arc<PiTerminalBus>>,
}

impl PiPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a terminal bus so spawned processes mirror stdout/stderr there.
    pub fn with_terminal(mut self, bus: Arc<PiTerminalBus>) -> Self {
        self.terminal = Some(bus);
        self
    }

    /// Return the cached pi process for this agent, spawning one if needed
    /// or the existing one has died.
    pub async fn get_or_spawn(&self, cfg: &PiLaunchConfig) -> Result<Arc<PiProcess>> {
        // Fast path: cached + alive.
        {
            let guard = self.inner.read().await;
            if let Some(proc) = guard.get(&cfg.agent_id) {
                if proc.is_alive() {
                    return Ok(proc.clone());
                }
            }
        }

        // Spawn + cache.
        let mut guard = self.inner.write().await;
        if let Some(proc) = guard.get(&cfg.agent_id) {
            if proc.is_alive() {
                return Ok(proc.clone());
            }
            // Previous died — drop it so the kill-on-drop fires.
            guard.remove(&cfg.agent_id);
        }
        let fresh = Arc::new(PiProcess::spawn_with_terminal(cfg, self.terminal.clone()).await?);
        guard.insert(cfg.agent_id.clone(), fresh.clone());
        Ok(fresh)
    }

    /// Kill and forget the process for this agent (if any).
    pub async fn evict(&self, agent_id: &str) {
        let mut guard = self.inner.write().await;
        if let Some(proc) = guard.remove(agent_id) {
            proc.shutdown().await;
        }
    }

    /// Kill every cached process.
    pub async fn shutdown_all(&self) {
        let mut guard = self.inner.write().await;
        for (_, proc) in guard.drain() {
            proc.shutdown().await;
        }
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
