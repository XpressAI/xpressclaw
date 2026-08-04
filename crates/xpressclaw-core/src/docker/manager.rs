use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::container::{
    AttachContainerOptions, Config as ContainerConfig, CreateContainerOptions,
    ListContainersOptions, LogOutput, LogsOptions, RemoveContainerOptions, StopContainerOptions,
    WaitContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::pin::Pin;
use tokio::io::AsyncWrite;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

const LIFECYCLE_LABEL: &str = "io.xpressclaw.lifecycle";
const PROJECT_LIFECYCLE: &str = "project";
const SPEC_FINGERPRINT_LABEL: &str = "io.xpressclaw.spec-fingerprint";

/// Specification for an agent container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,
    pub memory_limit: Option<i64>,
    pub cpu_limit: Option<i64>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    pub network_mode: Option<String>,
    /// Port to expose from the container (harness HTTP port).
    pub expose_port: Option<u16>,
    /// Command to run (overrides image CMD).
    pub cmd: Option<Vec<String>>,
    /// Working directory inside the container.
    pub working_dir: Option<String>,
    /// Run with the host user's effective filesystem identity. Rootless
    /// runtimes use container root because it maps back to the invoking user.
    #[serde(default)]
    pub run_as_host_user: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

impl Default for ContainerSpec {
    fn default() -> Self {
        Self {
            image: "ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest".to_string(),
            memory_limit: Some(2 * 1024 * 1024 * 1024), // 2GB
            cpu_limit: None,
            environment: Vec::new(),
            volumes: Vec::new(),
            network_mode: Some("bridge".to_string()),
            expose_port: Some(8080),
            cmd: None,
            working_dir: None,
            run_as_host_user: false,
        }
    }
}

/// Info about a running container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub container_id: String,
    pub agent_id: String,
    pub status: String,
    pub host_port: Option<u16>,
}

/// A running container attached to its stdio streams. ACP agents use this
/// bidirectional channel for newline-delimited JSON-RPC rather than exposing a
/// terminal or an HTTP control port.
pub struct AttachedContainer {
    pub info: ContainerInfo,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
    pub output: Pin<Box<dyn Stream<Item = std::result::Result<LogOutput, BollardError>> + Send>>,
}

/// Captured result of a short-lived worker container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerOutput {
    pub status_code: i64,
    pub output: String,
}

/// Manages Docker/Podman containers for agent isolation.
pub struct DockerManager {
    docker: Docker,
    rootless: bool,
    socket_path: Option<PathBuf>,
    runtime: &'static str,
    runtime_version: Option<String>,
}

impl DockerManager {
    /// Connect to the Docker/Podman daemon.
    ///
    /// Tries multiple socket paths to handle rootless Podman and Docker
    /// Desktop installations. Order:
    /// 1. DOCKER_HOST env var (if set)
    /// 2. User-level Docker Desktop and rootless Podman sockets
    /// 3. bollard defaults (/var/run/docker.sock, npipe, etc.)
    ///
    /// User sockets come before the conventional system socket because GUI
    /// applications often do not inherit the DOCKER_HOST exported by a login
    /// shell. If rootless Podman is active, its image store must be used
    /// consistently for image inspection, worker launch, and socket mounts.
    pub async fn connect() -> Result<Self> {
        // If DOCKER_HOST is set, trust it
        if std::env::var("DOCKER_HOST").is_ok() {
            return Self::connect_default().await;
        }

        // Prefer a running user-level runtime. This makes desktop launches
        // agree with a user's rootless Podman environment even when a dormant
        // or unrelated system Docker daemon is also installed.
        #[cfg(unix)]
        {
            for user_socket in fallback_unix_sockets() {
                if user_socket.exists() {
                    if let Ok(mgr) = Self::connect_to_socket(&user_socket).await {
                        return Ok(mgr);
                    }
                }
            }
        }

        // Fall back to the platform default only when no user runtime answers.
        if let Ok(mgr) = Self::connect_default().await {
            return Ok(mgr);
        }

        Err(Error::DockerNotAvailable(
            "Cannot reach Docker/Podman daemon. \
             Ensure Docker Desktop or Podman is running."
                .to_string(),
        ))
    }

    async fn connect_default() -> Result<Self> {
        let docker = Docker::connect_with_defaults()
            .map_err(|e| Error::DockerNotAvailable(e.to_string()))?;
        Self::connected(docker, configured_unix_socket()).await
    }

    async fn connected(docker: Docker, socket_path: Option<PathBuf>) -> Result<Self> {
        docker
            .ping()
            .await
            .map_err(|e| Error::DockerNotAvailable(format!("Docker ping failed: {e}")))?;
        let version = docker.version().await.ok();
        let runtime = runtime_from_version(version.as_ref());
        let runtime_version = version.and_then(|version| version.version);
        let rootless = docker
            .info()
            .await
            .ok()
            .and_then(|info| info.security_options)
            .is_some_and(|options| options.iter().any(|option| option.contains("rootless")));
        info!(
            socket = socket_path.as_ref().map(|path| path.display().to_string()),
            runtime,
            version = runtime_version.as_deref(),
            rootless,
            "connected to container runtime"
        );
        Ok(Self {
            docker,
            rootless,
            socket_path,
            runtime,
            runtime_version,
        })
    }

    #[cfg(unix)]
    async fn connect_to_socket(path: &Path) -> Result<Self> {
        let path_string = path.to_string_lossy();
        let docker = Docker::connect_with_unix(&path_string, 120, bollard::API_DEFAULT_VERSION)
            .map_err(|e| Error::DockerNotAvailable(e.to_string()))?;
        Self::connected(docker, Some(path.to_path_buf()))
            .await
            .map_err(|error| {
                Error::DockerNotAvailable(format!(
                    "Docker ping failed on {}: {error}",
                    path.display()
                ))
            })
    }

    /// Unix socket that can be mounted into a trusted worker so its Docker
    /// CLI talks to the same engine as the control plane.
    pub fn host_engine_socket(&self) -> Option<&Path> {
        self.socket_path.as_deref()
    }

    /// Docker-compatible runtime selected by automatic detection.
    pub fn runtime(&self) -> &'static str {
        self.runtime
    }

    /// Runtime version reported by its compatibility API.
    pub fn runtime_version(&self) -> Option<&str> {
        self.runtime_version.as_deref()
    }

    pub fn is_rootless(&self) -> bool {
        self.rootless
    }

    /// Check if Docker Desktop is installed (macOS/Windows).
    pub fn is_docker_desktop_installed() -> bool {
        #[cfg(target_os = "macos")]
        {
            std::path::Path::new("/Applications/Docker.app").exists()
        }
        #[cfg(target_os = "windows")]
        {
            std::path::Path::new("C:\\Program Files\\Docker\\Docker\\Docker Desktop.exe").exists()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        false
    }

    /// Try to start Docker Desktop (macOS/Windows). Returns Ok if launched.
    pub fn start_docker_desktop() -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("open")
                .args(["-a", "Docker"])
                .status()
                .map_err(|e| Error::DockerNotAvailable(format!("Failed to start Docker: {e}")))?;
            if status.success() {
                Ok(())
            } else {
                Err(Error::DockerNotAvailable(
                    "Failed to start Docker Desktop".to_string(),
                ))
            }
        }
        #[cfg(target_os = "windows")]
        {
            let status = std::process::Command::new("cmd")
                .args([
                    "/c",
                    "start",
                    "",
                    "C:\\Program Files\\Docker\\Docker\\Docker Desktop.exe",
                ])
                .status()
                .map_err(|e| Error::DockerNotAvailable(format!("Failed to start Docker: {e}")))?;
            if status.success() {
                Ok(())
            } else {
                Err(Error::DockerNotAvailable(
                    "Failed to start Docker Desktop".to_string(),
                ))
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Err(Error::DockerNotAvailable(
            "Auto-start not supported on this platform".to_string(),
        ))
    }

    /// Launch an agent container.
    pub async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo> {
        let container_name = format!("xpressclaw-{agent_id}");

        // Remove existing container if present
        let _ = self.remove(&container_name).await;

        self.create_and_start(agent_id, spec, None).await
    }

    /// Start a project-owned ACP container, retaining and reusing its writable
    /// layer between attempts. A changed runner specification or local image
    /// replaces the environment so stale mounts, credentials, and commands
    /// are never silently reused.
    pub async fn launch_project_attached(
        &self,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<AttachedContainer> {
        let container_name = format!("xpressclaw-{agent_id}");
        let fingerprint = container_spec_fingerprint(spec)?;
        let existing = self.inspect_by_name(&container_name).await;
        let info = if let Some(existing) = existing {
            let labels_match = existing
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref())
                .is_some_and(|labels| project_labels_match(labels, &fingerprint));
            let image_matches = self
                .container_image_matches(&container_name, &spec.image)
                .await;
            if labels_match && image_matches {
                self.stop_preserving(agent_id).await?;
                let container_id = existing.id.ok_or_else(|| {
                    Error::Container(format!(
                        "project container {container_name} has no container ID"
                    ))
                })?;
                self.docker
                    .start_container::<String>(&container_id, None)
                    .await
                    .map_err(|error| {
                        Error::Container(format!("failed to restart project container: {error}"))
                    })?;
                let host_port = self.get_host_port(&container_id, spec.expose_port).await;
                info!(
                    agent_id,
                    container_id = &container_id[..12.min(container_id.len())],
                    "restarted retained project container"
                );
                ContainerInfo {
                    container_id,
                    agent_id: agent_id.to_string(),
                    status: "running".to_string(),
                    host_port,
                }
            } else {
                info!(
                    agent_id,
                    labels_match,
                    image_matches,
                    "recreating project container after runner configuration changed"
                );
                self.remove(&container_name).await?;
                self.create_and_start(agent_id, spec, Some(project_container_labels(&fingerprint)))
                    .await?
            }
        } else {
            self.create_and_start(agent_id, spec, Some(project_container_labels(&fingerprint)))
                .await?
        };

        match self.attach(info).await {
            Ok(attached) => Ok(attached),
            Err(error) => {
                let _ = self.stop_preserving(agent_id).await;
                Err(error)
            }
        }
    }

    /// Whether a retained project container still represents the requested
    /// runner specification and the image currently available under its tag.
    pub async fn project_container_matches(&self, agent_id: &str, spec: &ContainerSpec) -> bool {
        let Ok(fingerprint) = container_spec_fingerprint(spec) else {
            return false;
        };
        let container_name = format!("xpressclaw-{agent_id}");
        let labels_match = self
            .inspect_by_name(&container_name)
            .await
            .and_then(|container| container.config)
            .and_then(|config| config.labels)
            .is_some_and(|labels| project_labels_match(&labels, &fingerprint));
        labels_match
            && self
                .container_image_matches(&container_name, &spec.image)
                .await
    }

    async fn create_and_start(
        &self,
        agent_id: &str,
        spec: &ContainerSpec,
        labels: Option<HashMap<String, String>>,
    ) -> Result<ContainerInfo> {
        let container_name = format!("xpressclaw-{agent_id}");

        // Build mounts — detect named volumes vs bind mounts
        let mounts: Vec<Mount> = spec
            .volumes
            .iter()
            .map(|v| {
                // Docker volume names allow only [a-zA-Z0-9][a-zA-Z0-9_.-], so a separator,
                // drive-letter colon (Windows C:\...), or leading ~ marks a host path → bind.
                let is_named_volume = !v.source.contains('/')
                    && !v.source.contains('\\')
                    && !v.source.contains(':')
                    && !v.source.starts_with('~');
                Mount {
                    target: Some(v.target.clone()),
                    source: Some(v.source.clone()),
                    typ: Some(if is_named_volume {
                        MountTypeEnum::VOLUME
                    } else {
                        MountTypeEnum::BIND
                    }),
                    read_only: Some(v.read_only),
                    ..Default::default()
                }
            })
            .collect();

        // Build environment
        let mut env = spec.environment.clone();
        env.push(format!("XPRESSCLAW_AGENT_ID={agent_id}"));

        // Build port bindings
        let mut port_bindings = HashMap::new();
        let mut exposed_ports = HashMap::new();
        if let Some(port) = spec.expose_port {
            let container_port = format!("{port}/tcp");
            exposed_ports.insert(container_port.clone(), HashMap::new());
            port_bindings.insert(
                container_port,
                Some(vec![bollard::models::PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some("0".to_string()), // Let Docker assign a port
                }]),
            );
        }

        let host_config = HostConfig {
            memory: spec.memory_limit,
            nano_cpus: spec.cpu_limit,
            group_add: socket_mount_groups(spec),
            extra_hosts: (self.runtime == "docker")
                .then(|| vec!["host.docker.internal:host-gateway".to_string()]),
            mounts: if mounts.is_empty() {
                None
            } else {
                Some(mounts)
            },
            network_mode: spec.network_mode.clone(),
            port_bindings: if port_bindings.is_empty() {
                None
            } else {
                Some(port_bindings)
            },
            ..Default::default()
        };

        let config = ContainerConfig {
            image: Some(spec.image.clone()),
            user: container_user(spec.run_as_host_user, self.rootless),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            open_stdin: Some(true),
            stdin_once: Some(false),
            env: Some(env),
            host_config: Some(host_config),
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            cmd: spec.cmd.clone(),
            working_dir: spec.working_dir.clone(),
            labels,
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: &container_name,
            platform: None,
        };

        let response = self
            .docker
            .create_container(Some(opts), config)
            .await
            .map_err(|e| Error::Container(format!("failed to create container: {e}")))?;

        self.docker
            .start_container::<String>(&response.id, None)
            .await
            .map_err(|e| Error::Container(format!("failed to start container: {e}")))?;

        // Get the assigned host port
        let host_port = self.get_host_port(&response.id, spec.expose_port).await;

        info!(
            agent_id,
            container_id = &response.id[..12],
            ?host_port,
            "launched container"
        );

        Ok(ContainerInfo {
            container_id: response.id,
            agent_id: agent_id.to_string(),
            status: "running".to_string(),
            host_port,
        })
    }

    /// Launch a container and attach its stdin/stdout/stderr. The process is
    /// expected to remain idle until the caller sends its first protocol
    /// request, which makes attaching immediately after start race-free for
    /// ACP agents.
    pub async fn launch_attached(
        &self,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<AttachedContainer> {
        let info = self.launch(agent_id, spec).await?;
        self.attach(info).await
    }

    async fn attach(&self, info: ContainerInfo) -> Result<AttachedContainer> {
        let attached = self
            .docker
            .attach_container(
                &info.container_id,
                Some(AttachContainerOptions::<String> {
                    stdin: Some(true),
                    stdout: Some(true),
                    stderr: Some(true),
                    stream: Some(true),
                    logs: Some(false),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|error| Error::Container(format!("failed to attach container: {error}")))?;

        Ok(AttachedContainer {
            info,
            input: attached.input,
            output: attached.output,
        })
    }

    /// Stop a project container without deleting its writable layer.
    pub async fn stop_preserving(&self, agent_id: &str) -> Result<()> {
        if !self.is_running(agent_id).await {
            return Ok(());
        }
        let container_name = format!("xpressclaw-{agent_id}");
        let stop_opts = StopContainerOptions { t: 2 };
        if let Err(error) = self
            .docker
            .stop_container(&container_name, Some(stop_opts))
            .await
        {
            warn!(agent_id, %error, "error stopping retained project container");
            if self.is_running(agent_id).await {
                return Err(Error::Container(format!(
                    "failed to stop retained project container: {error}"
                )));
            }
        }
        info!(agent_id, "stopped and retained project container");
        Ok(())
    }

    /// Whether this named container is a retained project environment.
    pub async fn is_project_container(&self, agent_id: &str) -> bool {
        let container_name = format!("xpressclaw-{agent_id}");
        self.inspect_by_name(&container_name)
            .await
            .and_then(|container| container.config)
            .and_then(|config| config.labels)
            .is_some_and(|labels| {
                labels.get(LIFECYCLE_LABEL).map(String::as_str) == Some(PROJECT_LIFECYCLE)
            })
    }

    /// Stop and remove an agent container.
    pub async fn stop(&self, agent_id: &str) -> Result<()> {
        let container_name = format!("xpressclaw-{agent_id}");

        // Short timeout — containers will be restarted on next boot.
        // Node.js/Python processes that don't handle SIGTERM get SIGKILL after this.
        let stop_opts = StopContainerOptions { t: 2 };
        if let Err(e) = self
            .docker
            .stop_container(&container_name, Some(stop_opts))
            .await
        {
            warn!(agent_id, error = %e, "error stopping container");
        }

        self.remove(&container_name).await?;
        info!(agent_id, "stopped container");
        Ok(())
    }

    /// Stop all xpressclaw containers.
    pub async fn stop_all(&self) -> Result<()> {
        let containers = self.list().await?;
        for info in containers {
            if let Err(e) = self.stop(&info.agent_id).await {
                warn!(agent_id = info.agent_id, error = %e, "error stopping container");
            }
        }
        Ok(())
    }

    /// Stop all XpressClaw containers during control-plane shutdown, retaining
    /// project environments while removing transient workloads.
    pub async fn stop_all_for_shutdown(&self) -> Result<()> {
        let containers = self.list().await?;
        for info in containers {
            let result = if self.is_project_container(&info.agent_id).await {
                self.stop_preserving(&info.agent_id).await
            } else {
                self.stop(&info.agent_id).await
            };
            if let Err(error) = result {
                warn!(agent_id = info.agent_id, %error, "error stopping container");
            }
        }
        Ok(())
    }

    /// List running xpressclaw containers.
    pub async fn list(&self) -> Result<Vec<ContainerInfo>> {
        let mut filters = HashMap::new();
        filters.insert("name", vec!["xpressclaw-"]);

        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        let containers = self
            .docker
            .list_containers(Some(opts))
            .await
            .map_err(|e| Error::Docker(e.to_string()))?;

        let mut infos = Vec::new();
        for c in containers {
            let names = match c.names {
                Some(ref n) => n.clone(),
                None => continue,
            };
            let name = match names.first() {
                Some(n) => n.trim_start_matches('/').to_string(),
                None => continue,
            };
            let agent_id = match name.strip_prefix("xpressclaw-") {
                Some(id) => id.to_string(),
                None => continue,
            };
            let container_id = c.id.unwrap_or_default();
            let status = c.state.unwrap_or_default();

            // Retrieve the host port via inspect (needed to route to the harness)
            let host_port = if status == "running" {
                self.get_host_port(&container_id, Some(8080)).await
            } else {
                None
            };

            infos.push(ContainerInfo {
                container_id,
                agent_id,
                status,
                host_port,
            });
        }

        Ok(infos)
    }

    /// Get container logs.
    pub async fn logs(&self, agent_id: &str, tail: usize) -> Result<String> {
        let container_name = format!("xpressclaw-{agent_id}");

        let opts = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            ..Default::default()
        };

        let mut stream = self.docker.logs(&container_name, Some(opts));
        let mut output = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(log) => output.push_str(&log.to_string()),
                Err(e) => {
                    debug!(error = %e, "error reading log chunk");
                    break;
                }
            }
        }

        Ok(output)
    }

    /// Wait for a short-lived workload container, capture its output, then
    /// remove it. The output is consumed by semantic adapters and never shown
    /// as an interactive terminal.
    pub async fn wait_for_exit(&self, workload_id: &str) -> Result<ContainerOutput> {
        self.wait_for_exit_streaming(workload_id, |_| {}).await
    }

    /// Follow a short-lived workload's output while it runs. Callers receive
    /// chunks for semantic parsing; the complete output is still returned for
    /// the final result adapter and artifact.
    pub async fn wait_for_exit_streaming<F>(
        &self,
        workload_id: &str,
        mut on_output: F,
    ) -> Result<ContainerOutput>
    where
        F: FnMut(&str),
    {
        let container_name = format!("xpressclaw-{workload_id}");
        let options = WaitContainerOptions {
            condition: "not-running",
        };
        let mut wait_stream = self.docker.wait_container(&container_name, Some(options));
        let log_options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: true,
            ..Default::default()
        };
        let mut log_stream = self.docker.logs(&container_name, Some(log_options));
        let mut streamed_output = String::new();
        let mut logs_done = false;

        let response = loop {
            tokio::select! {
                response = wait_stream.next() => {
                    break response
                        .ok_or_else(|| Error::Container("container wait ended without a result".to_string()))?
                        .map_err(|e| Error::Container(format!("container wait failed: {e}")))?;
                }
                chunk = log_stream.next(), if !logs_done => {
                    match chunk {
                        Some(Ok(log)) => {
                            let text = log.to_string();
                            streamed_output.push_str(&text);
                            on_output(&text);
                        }
                        Some(Err(error)) => {
                            debug!(%error, "error following workload output");
                            logs_done = true;
                        }
                        None => logs_done = true,
                    }
                }
            }
        };

        // Docker may report the exit before the final log frames are delivered.
        // Give the follow stream a short opportunity to drain those frames.
        if !logs_done {
            loop {
                match tokio::time::timeout(Duration::from_secs(1), log_stream.next()).await {
                    Ok(Some(Ok(log))) => {
                        let text = log.to_string();
                        streamed_output.push_str(&text);
                        on_output(&text);
                    }
                    Ok(Some(Err(error))) => {
                        debug!(%error, "error draining workload output");
                        break;
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }

        // Re-read the exited container for an authoritative final result. The
        // live stream above exists for activity, not as the durable artifact.
        let output = self
            .logs(workload_id, 100_000)
            .await
            .unwrap_or(streamed_output);
        let _ = self.remove(&container_name).await;
        Ok(ContainerOutput {
            status_code: response.status_code,
            output,
        })
    }

    /// Check if a container is running.
    pub async fn is_running(&self, agent_id: &str) -> bool {
        let container_name = format!("xpressclaw-{agent_id}");
        match self.docker.inspect_container(&container_name, None).await {
            Ok(info) => info.state.and_then(|s| s.running).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Pull a Docker image.
    pub async fn pull_image(&self, image: &str) -> Result<()> {
        info!(image, "pulling image");

        let opts = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(opts), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!(status, "pull progress");
                    }
                }
                Err(e) => {
                    return Err(Error::Docker(format!("failed to pull {image}: {e}")));
                }
            }
        }

        info!(image, "pull complete");
        Ok(())
    }

    /// Check if an image exists locally.
    pub async fn has_image(&self, image: &str) -> bool {
        self.docker.inspect_image(image).await.is_ok()
    }

    /// Check an image's declared protocol without starting it. Built-in ACP
    /// images carry this label so stale pre-ACP local tags are not reported as
    /// ready and then fail immediately with a missing server executable.
    pub async fn image_has_label(&self, image: &str, key: &str, value: &str) -> bool {
        self.docker
            .inspect_image(image)
            .await
            .ok()
            .and_then(|image| image.config)
            .and_then(|config| config.labels)
            .and_then(|labels| labels.get(key).cloned())
            .is_some_and(|label| label == value)
    }

    /// Check if a container's image matches the latest local image.
    /// Returns false if the container is running an outdated image.
    pub async fn container_image_matches(
        &self,
        container_name: &str,
        expected_image: &str,
    ) -> bool {
        let container_image = match self.docker.inspect_container(container_name, None).await {
            Ok(info) => info.image,
            Err(_) => return true, // Can't check, assume ok
        };
        let latest_image = match self.docker.inspect_image(expected_image).await {
            Ok(info) => info.id,
            Err(_) => return true, // Image not found locally, assume ok
        };
        match (container_image, latest_image) {
            (Some(container_sha), Some(latest_sha)) => container_sha == latest_sha,
            _ => true, // Can't compare, assume ok
        }
    }

    /// Check if a named container is running.
    pub async fn is_container_running(&self, container_name: &str) -> bool {
        match self.docker.inspect_container(container_name, None).await {
            Ok(info) => info.state.and_then(|s| s.running).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Get container uptime in seconds (0 if not running or not found).
    pub async fn container_uptime_secs(&self, container_name: &str) -> u64 {
        match self.docker.inspect_container(container_name, None).await {
            Ok(info) => {
                let started = info
                    .state
                    .and_then(|s| s.started_at)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok());
                match started {
                    Some(t) => chrono::Utc::now()
                        .signed_duration_since(t)
                        .num_seconds()
                        .max(0) as u64,
                    None => 0,
                }
            }
            Err(_) => 0,
        }
    }

    /// Inspect a container by name, returning None if not found.
    pub async fn inspect_by_name(
        &self,
        container_name: &str,
    ) -> Option<bollard::models::ContainerInspectResponse> {
        self.docker
            .inspect_container(container_name, None)
            .await
            .ok()
    }

    /// Get the host port for a container (public API for conversation routing).
    pub async fn get_container_port(&self, container_id: &str) -> Option<u16> {
        self.get_host_port(container_id, Some(8080)).await
    }

    /// Get the container ID by name.
    pub async fn get_container_id(&self, container_name: &str) -> Option<String> {
        self.inspect_by_name(container_name)
            .await
            .and_then(|info| info.id)
    }

    /// Get the host port for a container with a specific internal port.
    pub async fn get_container_port_for(
        &self,
        container_id: &str,
        internal_port: u16,
    ) -> Option<u16> {
        self.get_host_port(container_id, Some(internal_port)).await
    }

    /// Inspect a container and return its host port for any exposed port.
    pub async fn inspect(&self, container_id: &str) -> Result<Option<u16>> {
        let info = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| Error::Container(format!("inspect failed: {e}")))?;
        let port = info
            .network_settings
            .and_then(|ns| ns.ports)
            .and_then(|ports| {
                // Return the first mapped port
                for (_key, bindings) in ports.iter() {
                    if let Some(bindings) = bindings {
                        if let Some(binding) = bindings.first() {
                            if let Some(hp) = &binding.host_port {
                                return hp.parse().ok();
                            }
                        }
                    }
                }
                None
            });
        Ok(port)
    }

    async fn get_host_port(&self, container_id: &str, expose_port: Option<u16>) -> Option<u16> {
        let port = expose_port?;
        let info = self
            .docker
            .inspect_container(container_id, None)
            .await
            .ok()?;
        let network = info.network_settings?;
        let ports = network.ports?;
        let bindings = ports.get(&format!("{port}/tcp"))?.as_ref()?;
        let binding = bindings.first()?;
        binding.host_port.as_ref()?.parse().ok()
    }

    async fn remove(&self, container_name: &str) -> Result<()> {
        let opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        self.docker
            .remove_container(container_name, Some(opts))
            .await
            .map_err(|e| Error::Container(format!("failed to remove container: {e}")))?;
        Ok(())
    }

    /// Get the underlying bollard Docker client.
    pub fn client(&self) -> &Docker {
        &self.docker
    }
}

fn configured_unix_socket() -> Option<PathBuf> {
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        return unix_socket_from_host(&host.to_string_lossy());
    }
    #[cfg(unix)]
    {
        let conventional = PathBuf::from("/var/run/docker.sock");
        if conventional.exists() {
            return Some(conventional);
        }
    }
    None
}

fn unix_socket_from_host(host: &str) -> Option<PathBuf> {
    host.strip_prefix("unix://")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn runtime_from_version(version: Option<&bollard::system::Version>) -> &'static str {
    let is_podman = version
        .and_then(|version| version.components.as_deref())
        .is_some_and(|components| {
            components
                .iter()
                .any(|component| component.name.to_ascii_lowercase().contains("podman"))
        });
    if is_podman {
        "podman"
    } else {
        "docker"
    }
}

#[cfg(unix)]
fn fallback_unix_sockets() -> Vec<PathBuf> {
    let mut sockets = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        sockets.push(PathBuf::from(home).join(".docker/run/docker.sock"));
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        sockets.push(PathBuf::from(runtime).join("podman/podman.sock"));
    } else {
        // SAFETY: getuid has no preconditions and only reads process credentials.
        let uid = unsafe { libc::getuid() };
        sockets.push(PathBuf::from(format!("/run/user/{uid}/podman/podman.sock")));
    }
    sockets
}

fn socket_mount_groups(spec: &ContainerSpec) -> Option<Vec<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let mut groups: Vec<String> = spec
            .volumes
            .iter()
            .filter(|volume| volume.target == "/var/run/docker.sock")
            .filter_map(|volume| std::fs::metadata(&volume.source).ok())
            .map(|metadata| metadata.gid().to_string())
            .collect();
        groups.sort();
        groups.dedup();
        if !groups.is_empty() {
            return Some(groups);
        }
    }
    None
}

fn container_user(run_as_host_user: bool, rootless: bool) -> Option<String> {
    if !run_as_host_user {
        return None;
    }
    if rootless {
        return Some("0:0".to_string());
    }
    #[cfg(unix)]
    {
        // SAFETY: getuid/getgid have no preconditions and only read process
        // credentials.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Some(format!("{uid}:{gid}"))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

pub(crate) fn container_spec_fingerprint(spec: &ContainerSpec) -> Result<String> {
    let encoded = serde_json::to_vec(spec).map_err(|error| {
        Error::Container(format!(
            "failed to fingerprint project container specification: {error}"
        ))
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn project_container_labels(fingerprint: &str) -> HashMap<String, String> {
    HashMap::from([
        (LIFECYCLE_LABEL.to_string(), PROJECT_LIFECYCLE.to_string()),
        (SPEC_FINGERPRINT_LABEL.to_string(), fingerprint.to_string()),
    ])
}

fn project_labels_match(labels: &HashMap<String, String>, fingerprint: &str) -> bool {
    labels.get(LIFECYCLE_LABEL).map(String::as_str) == Some(PROJECT_LIFECYCLE)
        && labels.get(SPEC_FINGERPRINT_LABEL).map(String::as_str) == Some(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connect() {
        // This test requires Docker/Podman to be running
        let result = DockerManager::connect().await;
        // Don't fail if Docker isn't available in CI
        if let Ok(mgr) = result {
            let containers = mgr.list().await.unwrap();
            // Just verify we can list (may be empty)
            // Just verify the list call succeeded (may be empty)
            let _ = containers.len();
        }
    }

    #[test]
    fn test_container_spec_default() {
        let spec = ContainerSpec::default();
        assert_eq!(spec.expose_port, Some(8080));
        assert!(spec.image.contains("claude-sdk"));
        assert!(!spec.run_as_host_user);
    }

    #[test]
    fn rootless_native_workers_use_container_root_mapping() {
        assert_eq!(container_user(true, true).as_deref(), Some("0:0"));
        assert_eq!(container_user(false, true), None);
    }

    #[test]
    fn docker_host_only_exposes_local_unix_sockets() {
        assert_eq!(
            unix_socket_from_host("unix:///run/user/1000/podman/podman.sock"),
            Some(PathBuf::from("/run/user/1000/podman/podman.sock"))
        );
        assert_eq!(unix_socket_from_host("tcp://docker.example:2376"), None);
        assert_eq!(
            unix_socket_from_host("npipe:////./pipe/docker_engine"),
            None
        );
    }

    #[test]
    fn identifies_podman_from_compatibility_api_components() {
        let mut version = bollard::system::Version::default();
        version.components = Some(vec![bollard::system::VersionComponents {
            name: "Podman Engine".to_string(),
            version: "5.7.0".to_string(),
            details: None,
        }]);
        assert_eq!(runtime_from_version(Some(&version)), "podman");
        assert_eq!(
            runtime_from_version(Some(&bollard::system::Version::default())),
            "docker"
        );
    }

    #[test]
    fn socket_mounts_add_the_host_socket_group() {
        let source = std::env::temp_dir().join(format!(
            "xpressclaw-socket-group-test-{}",
            std::process::id()
        ));
        std::fs::write(&source, []).unwrap();
        let mut spec = ContainerSpec::default();
        spec.volumes.push(VolumeMount {
            source: source.display().to_string(),
            target: "/var/run/docker.sock".to_string(),
            read_only: false,
        });
        #[cfg(unix)]
        assert!(socket_mount_groups(&spec).is_some());
        std::fs::remove_file(source).unwrap();
    }

    #[test]
    fn project_container_fingerprint_changes_with_runner_configuration() {
        let mut spec = ContainerSpec::default();
        spec.environment
            .push("PRIVATE_TOKEN=first-secret".to_string());
        let first = container_spec_fingerprint(&spec).unwrap();
        let same = container_spec_fingerprint(&spec).unwrap();
        assert_eq!(first, same);
        assert!(!first.contains("first-secret"));

        spec.environment[0] = "PRIVATE_TOKEN=rotated-secret".to_string();
        let rotated = container_spec_fingerprint(&spec).unwrap();
        assert_ne!(first, rotated);
    }

    #[test]
    fn only_matching_project_labels_are_reusable() {
        let labels = project_container_labels("expected");
        assert!(project_labels_match(&labels, "expected"));
        assert!(!project_labels_match(&labels, "changed"));

        let transient =
            HashMap::from([(SPEC_FINGERPRINT_LABEL.to_string(), "expected".to_string())]);
        assert!(!project_labels_match(&transient, "expected"));
    }
}
