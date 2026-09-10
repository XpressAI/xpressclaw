//! Harness-independent access to retained containers. All processes go through
//! DockerManager's installation/Agent ownership checks, never a host shell.
use std::collections::{HashMap, VecDeque};
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
    let value =
        serde_json::to_string(forwards).map_err(|error| Error::Container(error.to_string()))?;
    db.with_conn(|conn| {
        conn.execute("INSERT INTO config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [format!("port_forwards:{agent_id}"), value])?;
        Ok(())
    })
}

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
    pub async fn container_files(&self, agent_id: &str, request: Value) -> Result<Value> {
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
                .take(140 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .await
                .map_err(io_error)?;
            if data.len() > 140 * 1024 * 1024 {
                return Err(Error::Container("Download exceeds 100 MiB".into()));
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
        self.forwards
            .lock()
            .await
            .get(&(agent_id.into(), id.into()))
            .is_some_and(|forward| !forward.task.is_finished())
    }

    pub async fn stop_forward(&self, agent_id: &str, id: &str) {
        let forward = self
            .forwards
            .lock()
            .await
            .remove(&(agent_id.into(), id.into()));
        if let Some(mut forward) = forward {
            forward.cancellation.cancel();
            if tokio::time::timeout(Duration::from_secs(3), &mut forward.task)
                .await
                .is_err()
            {
                forward.task.abort();
            }
        }
    }

    pub(crate) async fn stop_agent_forwards(&self, agent_id: &str) {
        let _lock = self.forwarding_lifecycle.lock().await;
        let ids: Vec<_> = self
            .forwards
            .lock()
            .await
            .keys()
            .filter(|(agent, _)| agent == agent_id)
            .map(|(_, id)| id.clone())
            .collect();
        for id in ids {
            self.stop_forward(agent_id, &id).await;
        }
    }

    pub async fn restore_forwards(&self, db: &Database, agent_id: &str) -> Result<()> {
        let _lock = self.forwarding_lifecycle.lock().await;
        for forward in saved_forwards(db, agent_id)? {
            self.start_forward(agent_id, &forward).await?;
        }
        Ok(())
    }

    pub async fn start_forward(&self, agent_id: &str, spec: &PortForward) -> Result<()> {
        spec.validate()?;
        let mut registry = self.forwards.lock().await;
        let key = (agent_id.to_string(), spec.id.clone());
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
            accepted = async { match &listener { Some(listener) => listener.accept().await, None => std::future::pending().await } }, if sockets.len() < 64 && pending.len() < 64 => {
                let (socket, _) = accepted.map_err(io_error)?;
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
                if frame["type"] == "open" && listener.is_none() && sockets.len() < 64 && !sockets.contains_key(&id) {
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
            docker.restore_forwards(&db, "loopback").await.unwrap();
            assert!(docker.forward_active("loopback", "preview").await);
        }).catch_unwind().await;
        docker.stop("loopback").await.unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
