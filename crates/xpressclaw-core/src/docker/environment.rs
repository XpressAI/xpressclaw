//! Harness-independent access to retained containers. All processes go through
//! DockerManager's installation/Agent ownership checks, never a host shell.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{StreamExt, TryStreamExt};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::{
    codec::{FramedRead, LinesCodec, LinesCodecError},
    io::StreamReader,
    sync::CancellationToken,
};

use super::manager::{AttachedContainer, DockerManager};
use crate::{
    db::Database,
    error::{Error, Result},
};

pub const MAX_CONTAINER_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
// Base64 expands each three bytes to four. Allow a small, separate envelope
// for the JSON fields; this is a transport limit, not the decoded file cap.
fn file_response_limit(request: &Value) -> Result<usize> {
    let bytes = if request["operation"] == "download" {
        match request.get("max_bytes") {
            Some(value) => value
                .as_u64()
                .filter(|size| *size <= MAX_CONTAINER_DOWNLOAD_BYTES as u64)
                .ok_or_else(|| {
                    Error::Container("Download limit must be between 0 and 100 MiB".into())
                })? as usize,
            None => MAX_CONTAINER_DOWNLOAD_BYTES,
        }
    } else {
        MAX_CONTAINER_DOWNLOAD_BYTES
    };
    Ok(bytes.div_ceil(3) * 4 + 64 * 1024)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForwardDirection {
    HostToContainer,
    ContainerToHost,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortForward {
    pub id: String,
    pub direction: ForwardDirection,
    pub host_port: u16,
    pub container_port: u16,
}

impl PortForward {
    pub fn validate(&self) -> Result<()> {
        if self.host_port == 0 || self.container_port == 0 {
            return Err(Error::Container("Ports must be between 1 and 65535".into()));
        }
        Ok(())
    }
}

// Local runtime preferences belong to this installation, not portable Project sync.
pub fn saved_forwards(db: &Database, agent_id: &str) -> Result<Vec<PortForward>> {
    db.with_conn(|conn| {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM config WHERE key = ?1",
                [format!("port_forwards:{agent_id}")],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| Error::Container(error.to_string()))
            })
            .unwrap_or_else(|| Ok(vec![]))
    })
}

pub fn save_forwards(db: &Database, agent_id: &str, forwards: &[PortForward]) -> Result<()> {
    for spec in forwards {
        spec.validate()?;
    }
    let value =
        serde_json::to_string(forwards).map_err(|error| Error::Container(error.to_string()))?;
    db.with_conn(|conn| {
        // The database connection lock makes reservation and persistence one
        // atomic operation across all Agents, including stopped containers.
        check_host_port_reservations(conn, agent_id, forwards)?;
        conn.execute("INSERT INTO config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [format!("port_forwards:{agent_id}"), value])?;
        Ok(())
    })
}

pub fn check_forward_reservations(
    db: &Database,
    agent_id: &str,
    forwards: &[PortForward],
) -> Result<()> {
    db.with_conn(|conn| check_host_port_reservations(conn, agent_id, forwards))
}

fn check_host_port_reservations(
    conn: &rusqlite::Connection,
    agent_id: &str,
    forwards: &[PortForward],
) -> Result<()> {
    let mut statement = conn
        .prepare("SELECT key, value FROM config WHERE key LIKE 'port_forwards:%' AND key != ?1")?;
    let saved = statement.query_map([format!("port_forwards:{agent_id}")], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for saved in saved {
        let (key, value) = saved?;
        let others: Vec<PortForward> =
            serde_json::from_str(&value).map_err(|error| Error::Container(error.to_string()))?;
        for spec in forwards {
            if others.iter().any(|other| host_ports_conflict(spec, other)) {
                return Err(Error::Container(format!("Host port {} is reserved by Agent {}; remove its mapping or choose another port", spec.host_port, key.trim_start_matches("port_forwards:"))));
            }
        }
    }
    Ok(())
}

fn host_ports_conflict(left: &PortForward, right: &PortForward) -> bool {
    left.host_port == right.host_port
        && (left.direction == ForwardDirection::ContainerToHost
            || right.direction == ForwardDirection::ContainerToHost)
}

#[derive(Default)]
pub(crate) struct ForwardingLifecycle(Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>);

impl ForwardingLifecycle {
    async fn lock(&self, agent_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut agents = self.0.lock().unwrap();
            agents.retain(|_, lock| lock.strong_count() > 0);
            let entry = agents.entry(agent_id.into()).or_default();
            match entry.upgrade() {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    *entry = Arc::downgrade(&lock);
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

pub(crate) type AgentForwards = tokio::sync::Mutex<HashMap<String, LiveForward>>;

pub(crate) struct LiveForward {
    spec: PortForward,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for LiveForward {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

fn io_error(error: std::io::Error) -> Error {
    Error::Container(error.to_string())
}

fn stream_reader(attached: &mut AttachedContainer) -> impl tokio::io::AsyncRead + Unpin + '_ {
    StreamReader::new(
        attached
            .output
            .by_ref()
            .map_ok(|output| match output {
                bollard::container::LogOutput::StdOut { message }
                | bollard::container::LogOutput::StdErr { message }
                | bollard::container::LogOutput::StdIn { message }
                | bollard::container::LogOutput::Console { message } => message,
            })
            .map_err(std::io::Error::other),
    )
}

impl DockerManager {
    pub async fn lock_agent_forwards(&self, agent_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.forwarding_lifecycle.lock(agent_id).await
    }

    async fn agent_forwards(&self, agent_id: &str) -> Arc<AgentForwards> {
        self.forwards
            .lock()
            .await
            .entry(agent_id.into())
            .or_default()
            .clone()
    }

    pub async fn container_files(&self, agent_id: &str, request: Value) -> Result<Value> {
        let response_limit = file_response_limit(&request)?;
        self.start_project_environment(agent_id).await?;
        let mut attached = self
            .open_project_process(
                agent_id,
                &["node".into(), "-e".into(), include_str!("files.cjs").into()],
                None,
                &[],
            )
            .await?;
        let operation = async {
            attached
                .input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(io_error)?;
            let mut data = Vec::new();
            stream_reader(&mut attached)
                .take(response_limit as u64 + 1)
                .read_to_end(&mut data)
                .await
                .map_err(io_error)?;
            if data.len() > response_limit {
                return Err(Error::Container(format!(
                    "Container file response exceeds its {response_limit} byte transport limit"
                )));
            }
            let value: Value = serde_json::from_slice(&data).map_err(|error| {
                Error::Container(format!("Container file operation failed: {error}"))
            })?;
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(Error::Container(error.into()));
            }
            Ok(value)
        };
        let result = tokio::time::timeout(Duration::from_secs(65), operation)
            .await
            .unwrap_or_else(|_| {
                Err(Error::Container(
                    "Container file operation timed out".into(),
                ))
            });
        let _ = attached.input.shutdown().await;
        result
    }

    pub async fn forward_active(&self, agent_id: &str, id: &str) -> bool {
        let forwards = self.agent_forwards(agent_id).await;
        let mut forwards = forwards.lock().await;
        forwards.retain(|_, forward| !forward.task.is_finished());
        forwards.contains_key(id)
    }

    pub async fn stop_forward(&self, agent_id: &str, id: &str) {
        let forwards = self.agent_forwards(agent_id).await;
        let forward = forwards.lock().await.remove(id);
        if let Some(forward) = forward {
            shutdown_forward(forward).await;
        }
    }

    pub(crate) async fn stop_agent_forwards(&self, agent_id: &str) {
        let _lock = self.lock_agent_forwards(agent_id).await;
        let forwards = self.agent_forwards(agent_id).await;
        let forwards: Vec<_> = forwards
            .lock()
            .await
            .drain()
            .map(|(_, forward)| forward)
            .collect();
        futures_util::future::join_all(forwards.into_iter().map(shutdown_forward)).await;
    }

    /// Saved forwards are optional runtime services; a conflict must not block
    /// an Agent turn or prevent other mappings from being restored.
    pub async fn restore_forwards(&self, db: &Database, agent_id: &str) {
        let _lock = self.lock_agent_forwards(agent_id).await;
        let forwards = match saved_forwards(db, agent_id) {
            Ok(forwards) => forwards,
            Err(error) => {
                tracing::warn!(%agent_id, %error, "could not load saved port forwards");
                return;
            }
        };
        for forward in forwards {
            if let Err(error) =
                check_forward_reservations(db, agent_id, std::slice::from_ref(&forward))
            {
                tracing::warn!(%agent_id, forward_id = %forward.id, %error, "saved port forward conflicts with another Agent; leaving it inactive");
                continue;
            }
            if let Err(error) = self.start_forward(agent_id, &forward).await {
                tracing::warn!(%agent_id, forward_id = %forward.id, host_port = forward.host_port, container_port = forward.container_port, %error, "saved port forward is inactive; continuing with the environment");
            }
        }
    }

    pub async fn start_forward(&self, agent_id: &str, spec: &PortForward) -> Result<()> {
        spec.validate()?;
        // Only this Agent waits for exec/readiness. The installation registry
        // is released before any container I/O.
        let forwards = self.agent_forwards(agent_id).await;
        let mut registry = forwards.lock().await;
        registry.retain(|_, forward| !forward.task.is_finished());
        let key = spec.id.clone();
        if registry
            .get(&key)
            .is_some_and(|live| live.spec == *spec && !live.task.is_finished())
        {
            return Ok(());
        }
        registry.remove(&key);
        let direction = match spec.direction {
            ForwardDirection::HostToContainer => "host_to_container",
            ForwardDirection::ContainerToHost => "container_to_host",
        };
        let listener = if spec.direction == ForwardDirection::ContainerToHost {
            Some(
                TcpListener::bind(("127.0.0.1", spec.host_port))
                    .await
                    .map_err(|error| {
                        Error::Container(format!(
                            "Cannot bind host port {}: {error}",
                            spec.host_port
                        ))
                    })?,
            )
        } else {
            None
        };
        let attached = self
            .open_project_process(
                agent_id,
                &[
                    "node".into(),
                    "-e".into(),
                    include_str!("bridge.cjs").into(),
                    direction.into(),
                    spec.container_port.to_string(),
                ],
                None,
                &[],
            )
            .await?;
        let mut input = attached.input;
        let mut reader = FramedRead::new(
            StreamReader::new(
                attached
                    .output
                    .map_ok(|output| match output {
                        bollard::container::LogOutput::StdOut { message }
                        | bollard::container::LogOutput::StdErr { message }
                        | bollard::container::LogOutput::StdIn { message }
                        | bollard::container::LogOutput::Console { message } => message,
                    })
                    .map_err(std::io::Error::other),
            ),
            LinesCodec::new_with_max_length(100000),
        );
        // Readiness belongs to the actual listener, so a busy container port
        // is reported before the mapping is saved or a URL is advertised.
        let ready = async {
            let line = reader
                .next()
                .await
                .ok_or_else(|| Error::Container("TCP bridge exited before becoming ready".into()))?
                .map_err(|error| Error::Container(error.to_string()))?;
            let value: Value = serde_json::from_str(&line)
                .map_err(|error| Error::Container(format!("Cannot start TCP bridge: {error}")))?;
            if value["type"] != "ready" {
                return Err(Error::Container(
                    value["message"]
                        .as_str()
                        .unwrap_or("TCP bridge failed to start")
                        .into(),
                ));
            }
            Ok(())
        };
        match tokio::time::timeout(Duration::from_secs(10), ready).await {
            Ok(Ok(())) => {}
            result => {
                let _ = input.shutdown().await;
                return Err(match result {
                    Ok(Err(error)) => error,
                    _ => Error::Container("TCP bridge startup timed out".into()),
                });
            }
        };
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let host_port = spec.host_port;
        let task = tokio::spawn(async move {
            if let Err(error) = bridge_loop(&mut input, reader, listener, host_port, &cancel).await
            {
                tracing::warn!(%error, "port forward stopped");
            }
            let _ = input.shutdown().await;
        });
        registry.insert(
            key,
            LiveForward {
                spec: spec.clone(),
                cancellation,
                task,
            },
        );
        Ok(())
    }
}

async fn shutdown_forward(mut forward: LiveForward) {
    forward.cancellation.cancel();
    if tokio::time::timeout(Duration::from_secs(3), &mut forward.task)
        .await
        .is_err()
    {
        forward.task.abort();
    }
}

async fn bridge_loop(
    input: &mut (dyn tokio::io::AsyncWrite + Send + Unpin),
    reader: impl futures_util::Stream<Item = std::result::Result<String, LinesCodecError>> + Unpin,
    listener: Option<TcpListener>,
    host_port: u16,
    cancel: &CancellationToken,
) -> Result<()> {
    let (writer, mut frames) = mpsc::channel::<Value>(32);
    let writing = async {
        while let Some(frame) = frames.recv().await {
            write_frame(input, &frame).await?;
        }
        Ok(())
    };
    tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        result = writing => result,
        result = bridge_router(reader, listener, host_port, writer) => result,
    }
}

async fn bridge_router(
    reader: impl futures_util::Stream<Item = std::result::Result<String, LinesCodecError>> + Unpin,
    listener: Option<TcpListener>,
    host_port: u16,
    writer: mpsc::Sender<Value>,
) -> Result<()> {
    let exposes_host = listener.is_some();
    let accepts = futures_util::stream::unfold(listener, |listener| async move {
        let accepted = match &listener {
            Some(listener) => listener.accept().await.map(|(socket, _)| socket),
            None => std::future::pending().await,
        };
        Some((accepted, listener))
    });
    bridge_router_with_accepts(reader, accepts, exposes_host, host_port, writer).await
}

async fn bridge_router_with_accepts(
    reader: impl futures_util::Stream<Item = std::result::Result<String, LinesCodecError>> + Unpin,
    accepts: impl futures_util::Stream<Item = std::io::Result<TcpStream>>,
    exposes_host: bool,
    host_port: u16,
    writer: mpsc::Sender<Value>,
) -> Result<()> {
    tokio::pin!(accepts);
    let mut accept_after = tokio::time::Instant::now();
    let mut pending = VecDeque::new();
    let mut lines = reader;
    let (events, mut receiver) = mpsc::channel::<Value>(32);
    let mut sockets = HashMap::<u64, mpsc::Sender<Value>>::new();
    let mut tasks = JoinSet::new();
    let mut next_id = 0u64;
    loop {
        tokio::select! {
            permit = writer.reserve(), if !pending.is_empty() => {
                let Ok(permit) = permit else { break; };
                permit.send(pending.pop_front().unwrap());
            },
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = async { tokio::time::sleep_until(accept_after).await; accepts.next().await }, if sockets.len() < 64 && pending.len() < 64 => {
                let socket = match accepted {
                    Some(Ok(socket)) => socket,
                    Some(Err(error)) => {
                        tracing::warn!(%error, "TCP bridge accept failed; retrying");
                        accept_after = tokio::time::Instant::now() + Duration::from_millis(100);
                        continue;
                    }
                    None => break,
                };
                next_id += 1;
                let id = next_id;
                let (sender, incoming) = mpsc::channel(16);
                sockets.insert(id, sender);
                pending.push_back(json!({"type":"open", "id":id}));
                tasks.spawn(socket_loop(id, Ok(socket), incoming, events.clone()));
            },
            event = receiver.recv(), if pending.len() < 64 => {
                if let Some(event) = event {
                    if event["type"] == "close" { sockets.remove(&event["id"].as_u64().unwrap_or(0)); }
                    pending.push_back(event);
                }
            },
            read = lines.next() => {
                let Some(line) = read else { break; };
                let line = line.map_err(|error| Error::Container(error.to_string()))?;
                if line.len() > 100000 { return Err(Error::Container("Oversized TCP bridge frame".into())); }
                let frame: Value = serde_json::from_str(&line).map_err(|error| Error::Container(error.to_string()))?;
                let id = frame["id"].as_u64().unwrap_or(0);
                if frame["type"] == "open" && !exposes_host && sockets.len() < 64 && !sockets.contains_key(&id) {
                    let (sender, incoming) = mpsc::channel(16);
                    sockets.insert(id, sender);
                    let events = events.clone();
                    tasks.spawn(async move {
                        let socket = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(("127.0.0.1", host_port))).await.unwrap_or_else(|_| Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out")));
                        socket_loop(id, socket, incoming, events).await;
                    });
                } else if let Some(sender) = sockets.get(&id) {
                    // Bound the time spent waiting for a stalled consumer.
                    if !matches!(tokio::time::timeout(Duration::from_secs(30), sender.send(frame)).await, Ok(Ok(()))) { sockets.remove(&id); pending.push_back(json!({"type":"close", "id":id})); }
                }
            },
        }
    }
    Ok(())
}

async fn write_frame(
    input: &mut (dyn tokio::io::AsyncWrite + Send + Unpin),
    frame: &Value,
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(30),
        input.write_all(format!("{frame}\n").as_bytes()),
    )
    .await
    .map_err(|_| Error::Container("TCP bridge write timed out".into()))?
    .map_err(io_error)?;
    Ok(())
}

async fn socket_loop(
    id: u64,
    socket: std::io::Result<TcpStream>,
    mut incoming: mpsc::Receiver<Value>,
    events: mpsc::Sender<Value>,
) {
    if let Ok(socket) = socket {
        let (mut read, mut write) = socket.into_split();
        let read_events = events.clone();
        let reader = async move {
            let mut buffer = vec![0; 32 * 1024];
            loop {
                let length = read.read(&mut buffer).await.map_err(|_| ())?;
                let frame = if length == 0 {
                    json!({"type":"end", "id":id})
                } else {
                    json!({"type":"data", "id":id, "data":STANDARD.encode(&buffer[..length])})
                };
                read_events.send(frame).await.map_err(|_| ())?;
                if length == 0 {
                    return Ok::<(), ()>(());
                }
            }
        };
        let writer = async move {
            while let Some(frame) = incoming.recv().await {
                match frame["type"].as_str() {
                    Some("data") => {
                        let data = STANDARD
                            .decode(frame["data"].as_str().unwrap_or(""))
                            .map_err(|_| ())?;
                        tokio::time::timeout(Duration::from_secs(30), write.write_all(&data))
                            .await
                            .map_err(|_| ())?
                            .map_err(|_| ())?;
                    }
                    Some("end") => {
                        write.shutdown().await.map_err(|_| ())?;
                        return Ok::<(), ()>(());
                    }
                    Some("close") => return Err(()),
                    _ => {}
                }
            }
            Err(())
        };
        let _ = tokio::try_join!(reader, writer);
    }
    let _ = events.send(json!({"type":"close", "id":id})).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::manager::ContainerSpec;
    use futures_util::FutureExt;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn port_preferences_are_scoped_to_an_agent() {
        let db = Database::open_memory().unwrap();
        let forward = PortForward {
            id: "local-llm".into(),
            direction: ForwardDirection::HostToContainer,
            host_port: 8080,
            container_port: 8081,
        };
        save_forwards(&db, "atlas", std::slice::from_ref(&forward)).unwrap();
        assert_eq!(saved_forwards(&db, "atlas").unwrap(), vec![forward]);
        assert!(saved_forwards(&db, "other").unwrap().is_empty());
        save_forwards(&db, "atlas", &[]).unwrap();
        assert!(saved_forwards(&db, "atlas").unwrap().is_empty());
    }

    #[test]
    fn host_port_reservations_reject_cross_agent_exposures_in_either_order() {
        let db = Database::open_memory().unwrap();
        let import = PortForward {
            id: "llm".into(),
            direction: ForwardDirection::HostToContainer,
            host_port: 8080,
            container_port: 8081,
        };
        let expose = PortForward {
            direction: ForwardDirection::ContainerToHost,
            ..import.clone()
        };
        save_forwards(&db, "atlas", std::slice::from_ref(&import)).unwrap();
        // A host LLM may be deliberately imported by multiple Agents.
        save_forwards(&db, "helper", std::slice::from_ref(&import)).unwrap();
        let error = save_forwards(&db, "helper", std::slice::from_ref(&expose))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Host port 8080 is reserved by Agent atlas"));
        assert_eq!(saved_forwards(&db, "helper").unwrap(), vec![import.clone()]);
        save_forwards(&db, "helper", &[]).unwrap();
        save_forwards(&db, "atlas", std::slice::from_ref(&expose)).unwrap();
        assert!(save_forwards(&db, "helper", &[import]).is_err());
        assert!(save_forwards(&db, "helper", &[expose]).is_err());
        assert!(saved_forwards(&db, "helper").unwrap().is_empty());
    }

    #[test]
    fn competing_agents_cannot_race_host_port_reservations() {
        let db = Arc::new(Database::open_memory().unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [
            ForwardDirection::HostToContainer,
            ForwardDirection::ContainerToHost,
        ]
        .into_iter()
        .enumerate()
        .map(|(id, direction)| {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                save_forwards(
                    &db,
                    &id.to_string(),
                    &[PortForward {
                        id: "forward".into(),
                        direction,
                        host_port: 3000,
                        container_port: 3001,
                    }],
                )
                .is_ok()
            })
        })
        .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|thread| thread.join().unwrap().then_some(()))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn forwarding_lifecycle_does_not_block_other_agents() {
        let locks = ForwardingLifecycle::default();
        let first = locks.lock("atlas").await;
        let second = tokio::time::timeout(Duration::from_millis(100), locks.lock("helper"))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), locks.lock("atlas"))
                .await
                .is_err()
        );
        drop(first);
        tokio::time::timeout(Duration::from_millis(100), locks.lock("atlas"))
            .await
            .unwrap();
        drop(second);
    }

    #[test]
    fn file_transport_limit_accounts_for_base64_and_the_requested_budget() {
        assert_eq!(
            file_response_limit(&json!({"operation":"download"})).unwrap(),
            MAX_CONTAINER_DOWNLOAD_BYTES.div_ceil(3) * 4 + 64 * 1024
        );
        assert_eq!(
            file_response_limit(&json!({"operation":"download", "max_bytes":20 * 1024 * 1024}))
                .unwrap(),
            (20usize * 1024 * 1024).div_ceil(3) * 4 + 64 * 1024
        );
        for size in [
            json!(-1),
            json!(MAX_CONTAINER_DOWNLOAD_BYTES + 1),
            json!("unbounded"),
        ] {
            assert!(
                file_response_limit(&json!({"operation":"download", "max_bytes":size})).is_err()
            );
        }
    }

    #[tokio::test]
    async fn bridge_accept_errors_are_retried_without_losing_the_router() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let accepts = futures_util::stream::iter(vec![
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
            Err(std::io::Error::from(std::io::ErrorKind::Other)),
            Ok(socket),
        ])
        .chain(futures_util::stream::pending());
        let (writer, mut frames) = mpsc::channel(32);
        let router = tokio::spawn(bridge_router_with_accepts(
            futures_util::stream::pending(),
            accepts,
            true,
            0,
            writer,
        ));
        let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame["type"], "open");
        assert!(!router.is_finished());
        drop(client);
        router.abort();
    }

    async fn capture(docker: &DockerManager, script: &str) -> String {
        let mut process = docker
            .open_project_process(
                "loopback",
                &["node".into(), "-e".into(), script.into()],
                None,
                &[],
            )
            .await
            .unwrap();
        let mut data = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(20),
            stream_reader(&mut process).read_to_end(&mut data),
        )
        .await
        .unwrap()
        .unwrap();
        String::from_utf8(data).unwrap()
    }

    /// Run with XPRESSCLAW_ENVIRONMENT_TEST_IMAGE pointing to a local image
    /// containing Node, tar, and tmux. The test owns an isolated installation
    /// and never mounts host directories into the test Agent.
    #[tokio::test]
    #[ignore = "requires Docker/Podman and XPRESSCLAW_ENVIRONMENT_TEST_IMAGE"]
    async fn retained_environment_roundtrip() {
        let Ok(image) = std::env::var("XPRESSCLAW_ENVIRONMENT_TEST_IMAGE") else {
            eprintln!("skipping container environment integration test; set XPRESSCLAW_ENVIRONMENT_TEST_IMAGE to the built fixture image to run it");
            return;
        };
        let installation = format!("environment-test-{}", uuid::Uuid::new_v4());
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let spec = ContainerSpec { image, expose_port: None, working_dir: Some("/tmp".into()), cmd: Some(vec!["node".into(), "-e".into(), "const fs=require('fs'); fs.mkdirSync('/tmp/artifacts',{recursive:true}); if(!fs.existsSync('/tmp/artifacts/report.txt')) fs.writeFileSync('/tmp/artifacts/report.txt','initial'); require('net').createServer({allowHalfOpen:true},s=>s.pipe(s)).listen(18080,'127.0.0.1',()=>console.log('ready'));".into()]), ..Default::default() };
        let mut main = docker
            .launch_project_attached("loopback", &spec)
            .await
            .unwrap();
        let result = std::panic::AssertUnwindSafe(async {
            let mut ready = String::new();
            tokio::time::timeout(Duration::from_secs(10), BufReader::new(stream_reader(&mut main)).read_line(&mut ready)).await.unwrap().unwrap();
            assert!(ready.contains("ready"));
            eprintln!("container ready");
            let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let inbound = PortForward { id: "llm".into(), direction: ForwardDirection::HostToContainer, host_port: echo.local_addr().unwrap().port(), container_port: 18081 };
            let echo_task = tokio::spawn(async move {
                while let Ok((socket, _)) = echo.accept().await {
                    tokio::spawn(async move { let (mut read, mut write) = socket.into_split(); let _ = tokio::io::copy(&mut read, &mut write).await; let _ = write.shutdown().await; });
                }
            });
            docker.start_forward("loopback", &inbound).await.unwrap();
            // A blocked/slow Agent registry cannot hold up another Agent.
            let other_forwards = docker.agent_forwards("other").await;
            let other_guard = other_forwards.lock().await;
            assert!(tokio::time::timeout(Duration::from_millis(100), docker.forward_active("loopback", &inbound.id)).await.unwrap());
            drop(other_guard);
            let mut finished = tokio::spawn(async {});
            (&mut finished).await.unwrap();
            other_forwards.lock().await.insert("dead".into(), LiveForward { spec: inbound.clone(), cancellation: CancellationToken::new(), task: finished });
            assert!(!docker.forward_active("other", "dead").await);
            assert!(other_forwards.lock().await.is_empty());
            eprintln!("host-to-container bridge ready");
            let echoed = capture(&docker, "const s=require('net').connect(18081,'127.0.0.1'); const data=Buffer.alloc(2*1024*1024,87); const chunks=[]; s.on('connect',()=>s.end(data)); s.on('data',d=>chunks.push(d)); s.on('end',()=>console.log(Buffer.concat(chunks).equals(data)?'echo ok':'corrupted')); s.on('error',e=>{console.error(e);process.exit(1)});").await;
            assert_eq!(echoed.trim(), "echo ok");
            eprintln!("host-to-container transfer passed");
            echo_task.abort();
            let conflict = PortForward { id: "conflict".into(), ..inbound.clone() };
            assert!(docker.start_forward("loopback", &conflict).await.is_err());
            docker.stop_forward("loopback", "llm").await;
            docker.start_forward("loopback", &inbound).await.unwrap();

            let reservation = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let host_port = reservation.local_addr().unwrap().port(); drop(reservation);
            let outbound = PortForward { id: "preview".into(), direction: ForwardDirection::ContainerToHost, host_port, container_port: 18080 };
            docker.start_forward("loopback", &outbound).await.unwrap();
            eprintln!("container-to-host bridge ready");
            let socket = TcpStream::connect(("127.0.0.1", host_port)).await.unwrap();
            let (mut read, mut write) = socket.into_split();
            let sent = vec![93u8; 2 * 1024 * 1024];
            let expected = sent.clone();
            let writing = tokio::spawn(async move { write.write_all(&sent).await.unwrap(); write.shutdown().await.unwrap(); });
            let mut received = Vec::new();
            tokio::time::timeout(Duration::from_secs(20), read.read_to_end(&mut received)).await.unwrap().unwrap();
            writing.await.unwrap();
            assert_eq!(received, expected);
            eprintln!("container-to-host transfer passed");
            docker.stop_forward("loopback", "preview").await;
            assert!(TcpStream::connect(("127.0.0.1", host_port)).await.is_err());

            let opened = docker.container_files("loopback", json!({"operation":"read", "path":"/tmp/artifacts/report.txt"})).await.unwrap();
            docker.container_files("loopback", json!({"operation":"write", "path":"/tmp/artifacts/report.txt", "content":"saved outside workspace", "expected_revision":opened["revision"]})).await.unwrap();
            assert!(docker.container_files("loopback", json!({"operation":"write", "path":"/tmp/artifacts/report.txt", "content":"stale", "expected_revision":opened["revision"]})).await.is_err());
            let archive = docker.container_files("loopback", json!({"operation":"download", "path":"/tmp/artifacts"})).await.unwrap();
            assert_eq!(archive["name"], "artifacts.tar.gz");
            assert!(!STANDARD.decode(archive["data"].as_str().unwrap()).unwrap().is_empty());

            let staged = docker.container_files("loopback", json!({"operation":"stage", "workspace":"/tmp", "name":"input.bin", "data":STANDARD.encode([0, 255, 1])})).await.unwrap();
            let staged_path = staged["path"].as_str().unwrap();
            assert!(staged_path.starts_with("/var/tmp/xpressclaw-uploads-"));
            let upload = docker.container_files("loopback", json!({"operation":"download", "path":staged_path})).await.unwrap();
            assert_eq!(STANDARD.decode(upload["data"].as_str().unwrap()).unwrap(), [0, 255, 1]);

            let mut first = docker.open_project_terminal("loopback", 100, 30, "shared").await.unwrap();
            first.input.write_all(b"export XC_SHARED_LOGIN=ready; printf started > /tmp/shared-started\r").await.unwrap();
            for _ in 0..50 {
                if docker.container_files("loopback", json!({"operation":"read", "path":"/tmp/shared-started"})).await.is_ok() { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            first.input.shutdown().await.unwrap();
            drop(first);
            let mut second = docker.open_project_terminal("loopback", 100, 30, "shared").await.unwrap();
            second.input.write_all(b"printf \"$XC_SHARED_LOGIN\" > /tmp/shared-result\r").await.unwrap();
            let mut content = Value::Null;
            for _ in 0..50 {
                if let Ok(result) = docker.container_files("loopback", json!({"operation":"read", "path":"/tmp/shared-result"})).await { content = result["content"].clone(); break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(content, "ready");
            let sessions = docker.container_files("loopback", json!({"operation":"sessions"})).await.unwrap();
            assert!(sessions["sessions"].as_array().unwrap().contains(&json!("shared")));
            second.input.shutdown().await.unwrap();

            let db = Database::open_memory().unwrap();
            save_forwards(&db, "loopback", std::slice::from_ref(&outbound)).unwrap();
            docker.stop_preserving("loopback").await.unwrap();
            assert!(!docker.forward_active("loopback", "llm").await);
            let retained = docker.container_files("loopback", json!({"operation":"read", "path":"/tmp/artifacts/report.txt"})).await.unwrap();
            assert_eq!(retained["content"], "saved outside workspace");
            let available = TcpListener::bind(("127.0.0.1", outbound.host_port)).await.unwrap();
            let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            drop(available);
            let unavailable = PortForward { id: "unavailable".into(), host_port: occupied.local_addr().unwrap().port(), ..outbound.clone() };
            save_forwards(&db, "loopback", &[unavailable.clone(), outbound.clone()]).unwrap();
            docker.restore_forwards(&db, "loopback").await;
            assert!(!docker.forward_active("loopback", "unavailable").await);
            assert!(docker.forward_active("loopback", "preview").await);
            assert_eq!(capture(&docker, "console.log('turn can run')").await.trim(), "turn can run");
            drop(occupied);
            docker.restore_forwards(&db, "loopback").await;
            assert!(docker.forward_active("loopback", "unavailable").await);
        }).catch_unwind().await;
        docker.stop("loopback").await.unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
