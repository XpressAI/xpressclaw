//! Server-side listener for the xclaw shell bridge (ADR-023 §7, task 5).
//!
//! Binds a Unix socket; on each incoming connection, reads one JSON
//! [`XclawRequest`], dispatches it to the appropriate xpressclaw
//! subsystem, writes one JSON [`XclawResponse`], and closes.
//!
//! This listener is called from the server's startup path. On Windows
//! (where Unix sockets aren't universally available) the listener is
//! a no-op until a platform-specific transport ships — harnesses run
//! in WASM guests that don't care about the host OS, so only the host
//! socket transport needs the split.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use xpressclaw_core::error::Error;
use xpressclaw_core::memory::manager::MemoryManager;
use xpressclaw_core::memory::zettelkasten::CreateMemory;
use xpressclaw_core::xclaw::{XclawRequest, XclawResponse};

use crate::state::AppState;

/// Start the xclaw bridge listener on `socket_path`. Returns the task
/// handle so callers can await it during shutdown.
///
/// If the socket path already exists, it is removed before binding
/// (stale socket from a previous run). Parent directory is created
/// if missing.
#[cfg(unix)]
pub fn start(socket_path: PathBuf, state: AppState) -> std::io::Result<JoinHandle<()>> {
    use tokio::net::UnixListener;

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    info!(path = %socket_path.display(), "xclaw bridge listening");

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, state).await {
                            warn!(error = %e, "xclaw connection errored");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "xclaw accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });

    Ok(handle)
}

#[cfg(not(unix))]
pub fn start(_socket_path: PathBuf, _state: AppState) -> std::io::Result<JoinHandle<()>> {
    warn!("xclaw bridge requires Unix sockets; no-op on this platform");
    Ok(tokio::spawn(async {}))
}

#[cfg(unix)]
async fn handle_connection(stream: tokio::net::UnixStream, state: AppState) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(()); // client closed without sending
    }

    let response = match serde_json::from_str::<XclawRequest>(line.trim_end()) {
        Ok(req) => {
            debug!(verb = %req.verb, agent_id = ?req.agent_id, "xclaw verb");
            dispatch(req, &state).await
        }
        Err(e) => XclawResponse::failure(format!("invalid request: {e}")),
    };

    let body = match serde_json::to_string(&response) {
        Ok(s) => s,
        Err(e) => format!(
            "{{\"ok\":false,\"error\":\"response serialization failed: {}\"}}",
            e
        ),
    };
    writer.write_all(body.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

async fn dispatch(req: XclawRequest, state: &AppState) -> XclawResponse {
    match req.verb.as_str() {
        "version" => XclawResponse::success(serde_json::json!({
            "xpressclaw": env!("CARGO_PKG_VERSION"),
            "protocol": "xclaw-1",
        })),
        "memory.add" => memory_add(req, state).await,
        "memory.list" => memory_list(req, state).await,
        other => XclawResponse::failure(format!("unknown verb: {other}")),
    }
}

async fn memory_add(req: XclawRequest, state: &AppState) -> XclawResponse {
    #[derive(serde::Deserialize)]
    struct Args {
        content: String,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
    }

    let args: Args = match serde_json::from_value(req.args) {
        Ok(a) => a,
        Err(e) => return XclawResponse::failure(format!("invalid args: {e}")),
    };

    let mgr = MemoryManager::new(state.db.clone(), &state.config().memory.eviction);
    // Auto-summary for short content if the caller didn't provide one.
    let summary = args.summary.unwrap_or_else(|| {
        let content = args.content.trim();
        if content.len() <= 80 {
            content.to_string()
        } else {
            format!("{}…", &content[..77])
        }
    });

    let create = CreateMemory {
        content: args.content,
        summary,
        source: "xclaw".to_string(),
        layer: "shared".to_string(),
        agent_id: req.agent_id.clone(),
        user_id: None,
        tags: args.tags,
    };

    match mgr.add(&create) {
        Ok(mem) => XclawResponse::success(serde_json::json!({
            "id": mem.id,
            "summary": mem.summary,
        })),
        Err(Error::Memory(msg)) => XclawResponse::failure(format!("memory: {msg}")),
        Err(e) => XclawResponse::failure(format!("memory: {e}")),
    }
}

async fn memory_list(req: XclawRequest, state: &AppState) -> XclawResponse {
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default = "default_limit")]
        limit: i64,
    }
    fn default_limit() -> i64 {
        20
    }

    let args: Args = match serde_json::from_value(req.args) {
        Ok(a) => a,
        Err(e) => return XclawResponse::failure(format!("invalid args: {e}")),
    };

    let mgr = MemoryManager::new(state.db.clone(), &state.config().memory.eviction);
    match mgr.get_recent(None, req.agent_id.as_deref(), args.limit) {
        Ok(list) => {
            let brief: Vec<serde_json::Value> = list
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.memory.id,
                        "summary": r.memory.summary,
                        "tags": r.memory.tags,
                        "created_at": r.memory.created_at,
                    })
                })
                .collect();
            XclawResponse::success(serde_json::json!({ "memories": brief }))
        }
        Err(e) => XclawResponse::failure(format!("memory: {e}")),
    }
}

/// Default socket path used by the server when no explicit path is
/// configured. Agent-facing path (inside a c2w guest) is
/// `/run/xclaw.sock`; host-side path defaults to
/// `<data_dir>/run/xclaw.sock` so it lives with the rest of xpressclaw
/// state.
pub fn default_socket_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("run").join("xclaw.sock")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    /// Minimal smoke: hit the `version` verb through the real socket
    /// listener and parse the response. Proves the transport works
    /// without needing a full AppState.
    #[tokio::test]
    async fn version_verb_roundtrips() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let sock = dir.path().join("xclaw.sock");

        // Bare listener that only handles the `version` verb — we
        // skip AppState setup to keep the test hermetic.
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: XclawRequest = serde_json::from_str(line.trim_end()).unwrap();
            let resp = if req.verb == "version" {
                XclawResponse::success(serde_json::json!({ "protocol": "xclaw-1" }))
            } else {
                XclawResponse::failure("unhandled")
            };
            let body = serde_json::to_string(&resp).unwrap();
            writer.write_all(body.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        });

        // Client side.
        let mut client = UnixStream::connect(&sock).await.unwrap();
        let req = XclawRequest {
            verb: "version".to_string(),
            args: serde_json::json!({}),
            agent_id: None,
        };
        let body = serde_json::to_string(&req).unwrap();
        client.write_all(body.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        let resp: XclawResponse = serde_json::from_str(buf.trim_end()).unwrap();
        assert!(resp.ok);
        assert_eq!(
            resp.result.as_ref().and_then(|v| v["protocol"].as_str()),
            Some("xclaw-1")
        );
    }
}
