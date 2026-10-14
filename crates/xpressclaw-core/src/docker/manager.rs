use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::container::{
    AttachContainerOptions, Config as ContainerConfig, CreateContainerOptions,
    ListContainersOptions, LogOutput, LogsOptions, RemoveContainerOptions, StopContainerOptions,
    WaitContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{ContainerInspectResponse, HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::pin::Pin;
use tokio::io::AsyncWrite;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::external_tools::path_for_external_tool;

#[path = "recovery.rs"]
mod recovery;
pub use recovery::PreservedEnvironment;

const LIFECYCLE_LABEL: &str = "io.xpressclaw.lifecycle";
const PROJECT_LIFECYCLE: &str = "project";
const SPEC_FINGERPRINT_LABEL: &str = "io.xpressclaw.spec-fingerprint";
const IMAGE_REFERENCE_LABEL: &str = "io.xpressclaw.image-reference";
const INSTALLATION_LABEL: &str = "io.xpressclaw.installation";
const AGENT_ID_LABEL: &str = "io.xpressclaw.agent-id";
const BASE_IMAGE_LABEL: &str = "io.xpressclaw.base-image";
const CONFIGURED_MOUNTS_LABEL: &str = "io.xpressclaw.configured-mount-targets";

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
    /// Optional SELinux relabeling for host bind mounts. Named volumes ignore
    /// this setting. Shared labels are appropriate for workspaces and login
    /// directories that may be mounted by more than one retained agent.
    #[serde(default)]
    pub selinux_relabel: SelinuxRelabel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelinuxRelabel {
    #[default]
    None,
    Shared,
    Private,
}

impl VolumeMount {
    /// Parse Docker-style `source:target[:options]` syntax. Supported options
    /// are `ro`, `rw`, `z` (shared SELinux label), and `Z` (private label).
    pub fn parse(raw: &str) -> Option<Self> {
        let (source_or_target, target_or_options) = raw.rsplit_once(':')?;
        let parsed_options = parse_mount_options(target_or_options);
        let (source, target, read_only, selinux_relabel) =
            if let Some((read_only, selinux_relabel)) = parsed_options {
                let (source, target) = source_or_target.rsplit_once(':')?;
                (source, target, read_only, selinux_relabel)
            } else {
                // A container target must be absolute. If the final field is
                // not an absolute target or a supported option list, reject
                // it instead of reinterpreting `source:target:unknown` as a
                // different source and relative target. This still preserves
                // Windows drive-letter sources such as C:\code:/workspace.
                if !target_or_options.starts_with('/') {
                    return None;
                }
                (
                    source_or_target,
                    target_or_options,
                    false,
                    SelinuxRelabel::None,
                )
            };
        if source.trim().is_empty() || target.trim().is_empty() {
            return None;
        }
        Some(Self {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
            selinux_relabel,
        })
    }
}

fn parse_mount_options(options: &str) -> Option<(bool, SelinuxRelabel)> {
    let mut read_only = false;
    let mut selinux_relabel = SelinuxRelabel::None;
    let mut recognized = false;
    for option in options.split(',') {
        match option.trim() {
            "ro" => {
                read_only = true;
                recognized = true;
            }
            "rw" => {
                read_only = false;
                recognized = true;
            }
            "z" => {
                selinux_relabel = SelinuxRelabel::Shared;
                recognized = true;
            }
            "Z" => {
                selinux_relabel = SelinuxRelabel::Private;
                recognized = true;
            }
            _ => return None,
        }
    }
    recognized.then_some((read_only, selinux_relabel))
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

/// An interactive pseudo-terminal started inside a retained project
/// container. The control plane bridges these streams to one browser
/// WebSocket and resizes the Docker exec session as the browser changes size.
pub struct TerminalSession {
    pub exec_id: String,
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
    /// Serializes mapping changes and restoration independently for each Agent.
    pub(crate) forwarding_lifecycle: super::environment::ForwardingLifecycle,
    pub(crate) forwards:
        tokio::sync::Mutex<HashMap<String, std::sync::Arc<super::environment::AgentForwards>>>,
    docker: Docker,
    rootless: bool,
    socket_path: Option<PathBuf>,
    runtime: &'static str,
    runtime_version: Option<String>,
    docker_desktop: bool,
    selinux: bool,
    installation_id: Option<String>,
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

    /// Connect to the engine on behalf of one durable XpressClaw
    /// installation. Owned containers are labelled and isolated from other
    /// control planes that happen to use the same Docker/Podman daemon.
    pub async fn connect_for_installation(installation_id: &str) -> Result<Self> {
        let mut manager = Self::connect().await?;
        manager.installation_id = Some(installation_id.to_string());
        Ok(manager)
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
        let system_info = docker.info().await.ok();
        let docker_desktop = docker_desktop_from_info(
            runtime,
            system_info
                .as_ref()
                .and_then(|info| info.operating_system.as_deref()),
            system_info.as_ref().and_then(|info| info.name.as_deref()),
        );
        let security_options = system_info
            .as_ref()
            .and_then(|info| info.security_options.as_deref())
            .unwrap_or_default();
        let rootless = has_security_option(security_options, "rootless");
        let selinux = has_security_option(security_options, "selinux");
        info!(
            socket = socket_path.as_ref().map(|path| path.display().to_string()),
            runtime,
            version = runtime_version.as_deref(),
            docker_desktop,
            rootless,
            selinux,
            "connected to container runtime"
        );
        Ok(Self {
            forwarding_lifecycle: Default::default(),
            forwards: Default::default(),
            docker,
            rootless,
            socket_path,
            runtime,
            runtime_version,
            docker_desktop,
            selinux,
            installation_id: None,
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

    /// Whether a named Docker network exists and belongs to this control-plane
    /// installation. Optional features must verify this before selecting a
    /// network because Docker rejects container creation for a missing network
    /// and a same-named network owned by another installation is not trusted.
    pub async fn installation_network_present(&self, name: &str) -> bool {
        let Ok(installation_id) = self.installation_id() else {
            return false;
        };
        self.docker
            .inspect_network::<String>(name, None)
            .await
            .ok()
            .and_then(|network| network.labels)
            .is_some_and(|labels| installation_labels_match(&labels, installation_id))
    }

    /// Docker-compatible runtime selected by automatic detection.
    pub fn runtime(&self) -> &'static str {
        self.runtime
    }

    /// Runtime version reported by its compatibility API.
    pub fn runtime_version(&self) -> Option<&str> {
        self.runtime_version.as_deref()
    }

    /// Whether the selected daemon identifies itself as Docker Desktop.
    pub fn is_docker_desktop(&self) -> bool {
        self.docker_desktop
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

        self.create_and_start(&container_name, agent_id, spec, None)
            .await
    }

    fn installation_id(&self) -> Result<&str> {
        self.installation_id.as_deref().ok_or_else(|| {
            Error::Container(
                "retained Agent containers require an XpressClaw installation identity".to_string(),
            )
        })
    }

    fn owned_project_container_name(&self, agent_id: &str) -> Result<String> {
        Ok(project_container_name(self.installation_id()?, agent_id))
    }

    /// Start a project-owned ACP container, retaining and reusing its writable
    /// layer between attempts. Image updates never replace an existing
    /// environment. The dispatcher recovers incompatible settings under its
    /// exclusive runtime boundary before launching; reject an unreconciled
    /// specification here before stopping the old environment.
    pub async fn launch_project_attached(
        &self,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<AttachedContainer> {
        let installation_id = self.installation_id()?;
        let container_name = project_container_name(installation_id, agent_id);
        let fingerprint = container_spec_fingerprint(spec)?;
        let existing = self.inspect_optional(&container_name).await?;
        let info = if let Some(existing) = existing {
            validate_retained_spec(&existing, spec, installation_id, agent_id)?;
            self.stop_preserving(agent_id).await?;
            let container_id = existing.id.ok_or_else(|| {
                Error::Container(format!(
                    "Agent container {container_name} has no container ID"
                ))
            })?;
            self.docker
                .start_container::<String>(&container_id, None)
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to restart Agent container: {error}"))
                })?;
            let host_port = self.get_host_port(&container_id, spec.expose_port).await;
            info!(
                agent_id,
                container_id = &container_id[..12.min(container_id.len())],
                "restarted retained Agent container"
            );
            ContainerInfo {
                container_id,
                agent_id: agent_id.to_string(),
                status: "running".to_string(),
                host_port,
            }
        } else {
            self.create_and_start(
                &container_name,
                agent_id,
                spec,
                Some(project_container_labels(
                    &fingerprint,
                    installation_id,
                    agent_id,
                )),
            )
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

    /// Validate before retiring a live ACP process. An image update alone is
    /// intentionally deferred until the user explicitly rebuilds the environment.
    pub async fn validate_project_configuration(
        &self,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<Option<String>> {
        let installation_id = self.installation_id()?;
        let container_name = project_container_name(installation_id, agent_id);
        let Some(container) = self.inspect_optional(&container_name).await? else {
            return Ok(None);
        };
        validate_retained_spec(&container, spec, installation_id, agent_id)?;
        container.id.map(Some).ok_or_else(|| {
            Error::Container(format!(
                "Agent container {container_name} has no container ID"
            ))
        })
    }

    async fn inspect_optional(&self, reference: &str) -> Result<Option<ContainerInspectResponse>> {
        match self.docker.inspect_container(reference, None).await {
            Ok(container) => Ok(Some(container)),
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(error) => Err(Error::Container(format!(
                "failed to inspect container {reference}: {error}"
            ))),
        }
    }

    /// Check capabilities on the image that actually backs the retained
    /// environment, even if its tag has moved or configuration selects an update.
    pub async fn retained_project_image(&self, agent_id: &str) -> Result<Option<String>> {
        let installation = self.installation_id()?;
        let name = project_container_name(installation, agent_id);
        let container = match self.inspect_optional(&name).await? {
            Some(container) => container,
            None => match self.inspect_optional(&format!("{name}-recovery")).await? {
                Some(container) => container,
                None => return Ok(None),
            },
        };
        let labels = container
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        if !project_ownership_matches(&labels, installation, agent_id) {
            return Err(Error::Container(format!(
                "refusing access to Agent {agent_id}'s environment with mismatched ownership"
            )));
        }
        labels
            .get(BASE_IMAGE_LABEL)
            .cloned()
            .or(container.image)
            .map(Some)
            .ok_or_else(|| Error::Container("retained environment has no image ID".into()))
    }

    async fn create_and_start(
        &self,
        container_name: &str,
        agent_id: &str,
        spec: &ContainerSpec,
        mut labels: Option<HashMap<String, String>>,
    ) -> Result<ContainerInfo> {
        if let Some(labels) = labels.as_mut().filter(|labels| {
            labels.get(LIFECYCLE_LABEL).map(String::as_str) == Some(PROJECT_LIFECYCLE)
        }) {
            // Podman can canonicalize Config.Image (e.g. node:22 becomes
            // docker.io/library/node:22). Retain the exact fingerprint input.
            labels
                .entry(IMAGE_REFERENCE_LABEL.to_string())
                .or_insert_with(|| spec.image.clone());
            labels.entry(CONFIGURED_MOUNTS_LABEL.to_string()).or_insert(
                serde_json::to_string(
                    &spec
                        .volumes
                        .iter()
                        .map(|mount| &mount.target)
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| Error::Container(error.to_string()))?,
            );
        }
        if let Some(installation_id) = self.installation_id.as_deref() {
            labels
                .get_or_insert_with(HashMap::new)
                .insert(INSTALLATION_LABEL.to_string(), installation_id.to_string());
        }

        let runtime_mounts = runtime_mounts(&spec.volumes, self.selinux)?;

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
            binds: if runtime_mounts.binds.is_empty() {
                None
            } else {
                Some(runtime_mounts.binds)
            },
            mounts: if runtime_mounts.mounts.is_empty() {
                None
            } else {
                Some(runtime_mounts.mounts)
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
            name: container_name,
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

    /// Stop an Agent container without deleting its writable layer.
    pub async fn stop_preserving(&self, agent_id: &str) -> Result<()> {
        self.stop_agent_forwards(agent_id).await;
        let container_name = self.owned_project_container_name(agent_id)?;
        if !self.is_container_running(&container_name).await {
            return Ok(());
        }
        let stop_opts = StopContainerOptions { t: 2 };
        if let Err(error) = self
            .docker
            .stop_container(&container_name, Some(stop_opts))
            .await
        {
            warn!(agent_id, %error, "error stopping retained Agent container");
            if self.is_container_running(&container_name).await {
                return Err(Error::Container(format!(
                    "failed to stop retained Agent container: {error}"
                )));
            }
        }
        info!(agent_id, "stopped and retained Agent container");
        Ok(())
    }

    /// Whether this named container is a retained project environment.
    pub async fn is_project_container(&self, agent_id: &str) -> bool {
        let Ok(installation_id) = self.installation_id() else {
            return false;
        };
        let container_name = project_container_name(installation_id, agent_id);
        self.inspect_by_name(&container_name)
            .await
            .and_then(|container| container.config)
            .and_then(|config| config.labels)
            .is_some_and(|labels| project_ownership_matches(&labels, installation_id, agent_id))
    }

    /// Remove every retained or transient container that this installation
    /// owns for one workload ID. Missing containers are idempotent no-ops;
    /// inspect failures are reported instead of being mistaken for absence.
    /// A same-named container without matching ownership labels is preserved.
    pub async fn remove_owned_workload(&self, agent_id: &str) -> Result<()> {
        // Validate the whole set before deleting any writable layer. Upgraded
        // installations can own both a retained environment and a legacy one.
        let candidates = self.removable_owned_workload(agent_id).await?;
        for container_name in candidates {
            self.remove_container_reference(&container_name, agent_id)
                .await?;
            self.stop_agent_forwards(agent_id).await;
        }
        Ok(())
    }

    async fn removable_owned_workload(&self, agent_id: &str) -> Result<Vec<String>> {
        let mut candidates = Vec::with_capacity(3);
        if let Some(installation_id) = self.installation_id.as_deref() {
            let name = project_container_name(installation_id, agent_id);
            candidates.push(format!("{name}-recovery"));
            candidates.push(name);
        }
        candidates.extend(
            self.preserved_project_environments(agent_id)
                .await?
                .into_iter()
                .map(|saved| saved.name),
        );
        candidates.push(format!("xpressclaw-{agent_id}"));
        candidates.sort();
        candidates.dedup();

        let mut owned = Vec::new();
        for container_name in candidates {
            let container = match self.docker.inspect_container(&container_name, None).await {
                Ok(container) => container,
                Err(BollardError::DockerResponseServerError {
                    status_code: 404, ..
                }) => continue,
                Err(error) => {
                    return Err(Error::Docker(format!(
                        "failed to inspect runtime '{container_name}': {error}"
                    )))
                }
            };
            let labels = container
                .config
                .and_then(|config| config.labels)
                .unwrap_or_default();
            if !workload_ownership_matches(
                &container_name,
                &labels,
                self.installation_id.as_deref(),
                agent_id,
            ) {
                continue;
            }
            self.ensure_no_container_dependents(&container_name).await?;
            owned.push(container_name);
        }
        Ok(owned)
    }

    /// Remove an exact container ID read from a Project-owned durable row.
    ///
    /// Older XpressClaw versions did not attach installation ownership labels,
    /// so the durable full ID is the ownership proof for those runtimes. The
    /// inspected container must report that same full ID; names and shortened
    /// IDs are rejected to avoid broadening destructive cleanup.
    pub async fn remove_recorded_workload(&self, container_id: &str) -> Result<()> {
        let container = match self.docker.inspect_container(container_id, None).await {
            Ok(container) => container,
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(()),
            Err(error) => {
                return Err(Error::Docker(format!(
                    "failed to inspect recorded runtime '{container_id}': {error}"
                )))
            }
        };
        if !recorded_container_id_matches(container.id.as_deref(), container_id) {
            return Err(Error::Container(format!(
                "recorded runtime reference '{container_id}' did not resolve to that exact container ID; refusing deletion"
            )));
        }
        self.remove_container_reference(container_id, container_id)
            .await
    }

    async fn remove_container_reference(
        &self,
        container_reference: &str,
        workload_id: &str,
    ) -> Result<()> {
        // Do not stop first: a failed removal must not leave the environment
        // and its dependents offline. Never use Podman's recursive --depend.
        self.ensure_no_container_dependents(container_reference)
            .await?;
        // Capture the image before removing the container. Only recovery
        // snapshots are eligible; ordinary runner images are user-managed.
        let snapshot = self
            .inspect_optional(container_reference)
            .await?
            .and_then(|container| recovery::container_snapshot(&container).map(str::to_owned));
        let opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        match self
            .docker
            .remove_container(container_reference, Some(opts))
            .await
        {
            Ok(())
            | Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                if let Some(snapshot) = snapshot {
                    self.discard_unused_snapshot(&snapshot).await;
                }
                Ok(())
            }
            Err(error) => Err(Error::Container(format!(
                "failed to remove owned runtime '{workload_id}': {error}"
            ))),
        }
    }

    /// Read-only preflight for explicit rebuild/deletion, before retiring any
    /// process. Agent-created containers need not carry XpressClaw labels.
    /// Includes installation-owned legacy containers even if no retained
    /// environment exists yet.
    pub async fn ensure_project_removable(&self, agent_id: &str) -> Result<()> {
        let installation = self.installation_id()?;
        let name = project_container_name(installation, agent_id);
        if let Some(container) = self.inspect_optional(&name).await? {
            let labels = container
                .config
                .and_then(|config| config.labels)
                .unwrap_or_default();
            if !project_ownership_matches(&labels, installation, agent_id) {
                return Err(Error::Container(format!(
                    "refusing to rebuild Agent {agent_id}'s environment with mismatched ownership"
                )));
            }
        }
        self.removable_owned_workload(agent_id).await?;
        Ok(())
    }

    async fn ensure_no_container_dependents(&self, reference: &str) -> Result<()> {
        let dependents = self.container_dependents(reference).await?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(Error::Container(format!(
            "Cannot remove environment {reference}: dependent containers {} still reference it. The environment and dependents have been preserved. Migrate or explicitly remove those containers before deleting this environment. Rebuilding can preserve stopped helpers separately.",
            dependents.iter().map(|container| container.name.as_deref().unwrap_or("unknown").trim_start_matches('/')).collect::<Vec<_>>().join(", ")
        )))
    }

    async fn container_dependents(&self, reference: &str) -> Result<Vec<ContainerInspectResponse>> {
        let Some(parent) = self.inspect_optional(reference).await? else {
            return Ok(Vec::new());
        };
        let parent_id = parent
            .id
            .as_deref()
            .ok_or_else(|| Error::Container("container has no ID; refusing removal".into()))?;
        let parent_name = parent
            .name
            .as_deref()
            .unwrap_or(reference)
            .trim_start_matches('/');
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|error| {
                Error::Container(format!("could not check container dependencies: {error}"))
            })?;
        let mut dependents = Vec::new();
        for candidate in containers {
            let id = candidate.id.ok_or_else(|| {
                Error::Container(
                    "container dependency listing omitted an ID; refusing removal".into(),
                )
            })?;
            if id == parent_id {
                continue;
            }
            // Include stopped containers; they still retain namespace/volume
            // dependencies. A concurrent deletion is harmless, other errors
            // must not turn an incomplete scan into permission to remove.
            let Some(container) = self.inspect_optional(&id).await? else {
                continue;
            };
            let host = container.host_config.as_ref().ok_or_else(|| {
                Error::Container(format!(
                    "dependency inspection for {id} omitted host configuration; refusing removal"
                ))
            })?;
            if container_depends_on(host, parent_id, parent_name) {
                dependents.push(container);
            }
        }
        dependents.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(dependents)
    }

    /// Start an interactive shell inside this installation's retained project
    /// container. A stopped retained container is restarted on demand so a
    /// user can repair credentials after a control-plane restart without
    /// first dispatching another agent turn.
    pub async fn open_project_terminal(
        &self,
        agent_id: &str,
        columns: u16,
        rows: u16,
        session: &str,
    ) -> Result<TerminalSession> {
        if !valid_terminal_session_name(session) {
            return Err(Error::Container(
                "Session names must contain 1–64 letters, digits, underscores or hyphens".into(),
            ));
        }
        let installation_id = self.installation_id()?;
        let container_name = project_container_name(installation_id, agent_id);
        let container = self.inspect_by_name(&container_name).await.ok_or_else(|| {
            Error::ContainerNotFound {
                id: format!(
                    "retained environment for {agent_id}; run a task once to initialize it"
                ),
            }
        })?;
        let labels = container
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .cloned()
            .unwrap_or_default();
        if !project_ownership_matches(&labels, installation_id, agent_id) {
            return Err(Error::Container(format!(
                "refusing terminal access to Agent container {container_name} with mismatched ownership"
            )));
        }
        let running = container
            .state
            .as_ref()
            .and_then(|state| state.running)
            .unwrap_or(false);
        if !running {
            self.docker
                .start_container::<String>(&container_name, None)
                .await
                .map_err(|error| {
                    Error::Container(format!(
                        "failed to restart retained Agent container: {error}"
                    ))
                })?;
        }

        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions::<String> {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    env: Some(vec![
                        "HOME=/home/node".to_string(),
                        "TERM=xterm-256color".to_string(),
                        "COLORTERM=truecolor".to_string(),
                    ]),
                    cmd: Some(vec!["/bin/sh".into(), "-c".into(), "if command -v tmux >/dev/null 2>&1; then exec tmux new-session -A -s \"$1\"; else printf 'tmux is unavailable in this image. Rebuild it with tmux for persistent shared sessions.\\n'; if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi; fi".into(), "xpressclaw-terminal".into(), session.into()]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| Error::Container(format!("failed to create terminal: {error}")))?;
        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: true,
                    output_capacity: Some(1024 * 1024),
                }),
            )
            .await
            .map_err(|error| Error::Container(format!("failed to start terminal: {error}")))?;
        let StartExecResults::Attached { output, input } = started else {
            return Err(Error::Container(
                "interactive terminal unexpectedly started detached".to_string(),
            ));
        };
        let session = TerminalSession {
            exec_id: exec.id,
            input,
            output,
        };
        let _ = self.resize_terminal(&session.exec_id, columns, rows).await;
        Ok(session)
    }

    pub async fn start_project_environment(&self, agent_id: &str) -> Result<()> {
        if !self.is_project_container(agent_id).await {
            return Err(Error::ContainerNotFound {
                id: format!(
                    "retained environment for {agent_id}; run a task once to initialize it"
                ),
            });
        }
        let name = self.owned_project_container_name(agent_id)?;
        if !self.is_container_running(&name).await {
            self.docker
                .start_container::<String>(&name, None)
                .await
                .map_err(|error| {
                    Error::Container(format!("Failed to restart retained environment: {error}"))
                })?;
        }
        Ok(())
    }

    /// Start another non-TTY ACP process inside a retained Agent container.
    /// Conversation turns use this independent stdio channel so an Agent can
    /// keep chatting while its primary ACP process is busy with a long task.
    pub async fn open_project_process(
        &self,
        agent_id: &str,
        command: &[String],
        working_dir: Option<&str>,
        environment: &[String],
    ) -> Result<AttachedContainer> {
        if command.is_empty() {
            return Err(Error::Container(
                "cannot start a project process without a command".into(),
            ));
        }
        let installation_id = self.installation_id()?;
        let container_name = project_container_name(installation_id, agent_id);
        let container = self.inspect_by_name(&container_name).await.ok_or_else(|| {
            Error::ContainerNotFound {
                id: format!("retained environment for {agent_id}"),
            }
        })?;
        let labels = container
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .cloned()
            .unwrap_or_default();
        if !project_ownership_matches(&labels, installation_id, agent_id) {
            return Err(Error::Container(format!(
                "refusing process access to Agent container {container_name} with mismatched ownership"
            )));
        }
        let running = container
            .state
            .as_ref()
            .and_then(|state| state.running)
            .unwrap_or(false);
        if !running {
            return Err(Error::Container(format!(
                "retained Agent container {container_name} is not running"
            )));
        }
        let container_id = container.id.unwrap_or_else(|| container_name.clone());
        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions::<String> {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    cmd: Some(command.to_vec()),
                    env: (!environment.is_empty()).then(|| environment.to_vec()),
                    working_dir: working_dir.map(str::to_string),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                Error::Container(format!("failed to create project ACP process: {error}"))
            })?;
        let started = self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: false,
                    output_capacity: Some(1024 * 1024),
                }),
            )
            .await
            .map_err(|error| {
                Error::Container(format!("failed to start project ACP process: {error}"))
            })?;
        let StartExecResults::Attached { output, input } = started else {
            return Err(Error::Container(
                "project ACP process unexpectedly started detached".into(),
            ));
        };
        Ok(AttachedContainer {
            info: ContainerInfo {
                container_id,
                agent_id: agent_id.to_string(),
                status: "running".into(),
                host_port: None,
            },
            input,
            output,
        })
    }

    /// Resize an interactive terminal created by [`open_project_terminal`].
    pub async fn resize_terminal(&self, exec_id: &str, columns: u16, rows: u16) -> Result<()> {
        self.docker
            .resize_exec(
                exec_id,
                ResizeExecOptions {
                    width: columns.clamp(20, 500),
                    height: rows.clamp(5, 300),
                },
            )
            .await
            .map_err(|error| Error::Container(format!("failed to resize terminal: {error}")))
    }

    /// Stop and remove an agent container.
    pub async fn stop(&self, agent_id: &str) -> Result<()> {
        let retained_name = self.owned_project_container_name(agent_id).ok();
        let container_name = match retained_name {
            Some(name) if self.inspect_by_name(&name).await.is_some() => name,
            _ => format!("xpressclaw-{agent_id}"),
        };

        self.remove(&container_name).await?;
        self.stop_agent_forwards(agent_id).await;
        info!(agent_id, "stopped container");
        Ok(())
    }

    /// Stop all XpressClaw containers visible to this manager. Installation-
    /// scoped managers never enumerate another control plane's containers.
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

    /// List XpressClaw containers owned by this manager's installation.
    /// Unscoped managers retain legacy behavior for unlabelled containers but
    /// never enumerate containers explicitly owned by an installation.
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
            let labels = c.labels.as_ref().cloned().unwrap_or_default();
            let Some(agent_id) =
                listed_container_agent_id(&name, &labels, self.installation_id.as_deref())
            else {
                continue;
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
        let container_name = self
            .owned_project_container_name(agent_id)
            .unwrap_or_else(|_| format!("xpressclaw-{agent_id}"));
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
        self.image_label(image, key)
            .await
            .is_some_and(|label| label == value)
    }

    /// Read one image label from the local runtime cache.
    pub async fn image_label(&self, image: &str, key: &str) -> Option<String> {
        self.docker
            .inspect_image(image)
            .await
            .ok()
            .and_then(|image| image.config)
            .and_then(|config| config.labels)
            .and_then(|labels| labels.get(key).cloned())
    }

    /// Resolve a pulled tag to the immutable repository digest reported by
    /// Docker or Podman. Persisting this reference prevents an explicit user
    /// update from changing again without another update action.
    pub async fn image_repo_digest(&self, image: &str) -> Result<String> {
        let repository = image
            .rsplit_once(':')
            .map(|(name, _)| name)
            .ok_or_else(|| Error::Docker(format!("image reference has no tag: {image}")))?;
        let inspect = self.docker.inspect_image(image).await.map_err(|error| {
            Error::Docker(format!("failed to inspect pulled image {image}: {error}"))
        })?;
        inspect
            .repo_digests
            .unwrap_or_default()
            .into_iter()
            .find(|digest| digest.starts_with(&format!("{repository}@sha256:")))
            .ok_or_else(|| {
                Error::Docker(format!(
                    "container runtime did not report an immutable repository digest for {image}"
                ))
            })
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
        self.remove_container_reference(container_name, container_name)
            .await
    }

    /// Get the underlying bollard Docker client.
    pub fn client(&self) -> &Docker {
        &self.docker
    }
}

pub fn valid_terminal_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
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

fn docker_desktop_from_info(
    runtime: &str,
    operating_system: Option<&str>,
    daemon_name: Option<&str>,
) -> bool {
    runtime == "docker"
        && [operating_system, daemon_name]
            .into_iter()
            .flatten()
            .any(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                normalized.contains("docker desktop") || normalized == "docker-desktop"
            })
}

fn has_security_option(options: &[String], expected: &str) -> bool {
    options.iter().any(|option| {
        option
            .split(['=', ',', ':'])
            .any(|part| part.eq_ignore_ascii_case(expected))
    })
}

#[derive(Debug, Default)]
struct RuntimeMounts {
    mounts: Vec<Mount>,
    binds: Vec<String>,
}

fn runtime_mounts(volumes: &[VolumeMount], selinux_enabled: bool) -> Result<RuntimeMounts> {
    let mut result = RuntimeMounts::default();
    for volume in volumes {
        // Docker volume names allow only [a-zA-Z0-9][a-zA-Z0-9_.-], so a separator,
        // drive-letter colon (Windows C:\...), or leading ~ marks a host path → bind.
        let is_named_volume = !volume.source.contains('/')
            && !volume.source.contains('\\')
            && !volume.source.contains(':')
            && !volume.source.starts_with('~');
        let source = if is_named_volume {
            volume.source.clone()
        } else {
            path_for_external_tool(Path::new(&volume.source))
                .display()
                .to_string()
        };
        let relabel = match volume.selinux_relabel {
            SelinuxRelabel::Shared => Some("z"),
            SelinuxRelabel::Private => Some("Z"),
            SelinuxRelabel::None => None,
        };
        if selinux_enabled && !is_named_volume {
            if let Some(relabel) = relabel {
                if source.contains(':') || volume.target.contains(':') {
                    return Err(Error::Container(format!(
                        "SELinux relabeling cannot represent a mount path containing ':' ({} -> {})",
                        source, volume.target
                    )));
                }
                let access = if volume.read_only { "ro" } else { "rw" };
                result
                    .binds
                    .push(format!("{}:{}:{access},{relabel}", source, volume.target));
                continue;
            }
        }
        result.mounts.push(Mount {
            target: Some(volume.target.clone()),
            source: Some(source),
            typ: Some(if is_named_volume {
                MountTypeEnum::VOLUME
            } else {
                MountTypeEnum::BIND
            }),
            read_only: Some(volume.read_only),
            ..Default::default()
        });
    }
    Ok(result)
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
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let mut groups: Vec<String> = spec
            .volumes
            .iter()
            .filter_map(|volume| std::fs::metadata(&volume.source).ok())
            .filter(|metadata| metadata.file_type().is_socket())
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
            "failed to fingerprint Agent container specification: {error}"
        ))
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

/// Image selection is only used when creating an environment. A new release
/// must not throw away the initialized process or installed tools.
pub(crate) fn retained_process_fingerprint(spec: &ContainerSpec) -> Result<String> {
    let mut effective = spec.clone();
    effective.image.clear();
    container_spec_fingerprint(&effective)
}

fn validate_retained_spec(
    container: &ContainerInspectResponse,
    requested: &ContainerSpec,
    installation: &str,
    agent: &str,
) -> Result<()> {
    if retained_spec_matches(container, requested, installation, agent)? {
        Ok(())
    } else {
        Err(Error::Container(format!(
            "Agent {agent}'s environment needs updated settings. Its installed tools and files have been preserved; retry the work to recover automatically."
        )))
    }
}

fn retained_spec_matches(
    container: &ContainerInspectResponse,
    requested: &ContainerSpec,
    installation: &str,
    agent: &str,
) -> Result<bool> {
    let labels = container
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    if !labels.is_some_and(|labels| project_ownership_matches(labels, installation, agent)) {
        return Err(Error::Container(format!(
            "refusing access to Agent {agent}'s environment with mismatched ownership"
        )));
    }
    // Use the original image reference, including for containers created before
    // this retention policy. All other settings still have to match exactly;
    // never silently retain an obsolete credential or host-engine grant.
    let image = container
        .config
        .as_ref()
        .and_then(|config| config.image.as_ref())
        .ok_or_else(|| {
            Error::Container(format!("Agent {agent}'s environment has no recorded image"))
        })?;
    let labels = labels.unwrap();
    let mut images = vec![requested.image.as_str(), image.as_str()];
    if let Some(original) = labels.get(IMAGE_REFERENCE_LABEL) {
        images.push(original);
    }
    // Legacy containers have only the hash; accommodate engine normalization
    // of common short references without weakening any non-image setting.
    for prefix in ["docker.io/library/", "docker.io/", "localhost/"] {
        if let Some(short) = image.strip_prefix(prefix) {
            images.push(short);
        }
    }
    let mut matches = false;
    for image in images {
        let mut effective = requested.clone();
        effective.image = image.to_string();
        if project_labels_match(
            labels,
            &container_spec_fingerprint(&effective)?,
            installation,
            agent,
        ) {
            matches = true;
            break;
        }
    }
    Ok(matches)
}

fn container_depends_on(host: &HostConfig, parent_id: &str, parent_name: &str) -> bool {
    let matches = |reference: &str| {
        let reference = reference.trim_start_matches('/');
        reference == parent_name
            || reference == parent_id
            || (reference.len() >= 12 && parent_id.starts_with(reference))
    };
    [
        &host.network_mode,
        &host.ipc_mode,
        &host.pid_mode,
        &host.uts_mode,
        &host.userns_mode,
    ]
    .into_iter()
    .filter_map(|mode| mode.as_deref())
    .any(|mode| mode.strip_prefix("container:").is_some_and(matches))
        || host
            .volumes_from
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|volume| {
                matches(
                    volume
                        .strip_suffix(":ro")
                        .or_else(|| volume.strip_suffix(":rw"))
                        .unwrap_or(volume),
                )
            })
        || host
            .links
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|link| matches(link.split(':').next().unwrap_or(link)))
}

fn project_container_name(installation_id: &str, agent_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(installation_id.as_bytes());
    digest.update([0]);
    digest.update(agent_id.as_bytes());
    let digest = digest.finalize();
    format!("xpressclaw-project-{digest:x}")
}

fn project_container_labels(
    fingerprint: &str,
    installation_id: &str,
    agent_id: &str,
) -> HashMap<String, String> {
    HashMap::from([
        (LIFECYCLE_LABEL.to_string(), PROJECT_LIFECYCLE.to_string()),
        (SPEC_FINGERPRINT_LABEL.to_string(), fingerprint.to_string()),
        (INSTALLATION_LABEL.to_string(), installation_id.to_string()),
        (AGENT_ID_LABEL.to_string(), agent_id.to_string()),
    ])
}

fn project_ownership_matches(
    labels: &HashMap<String, String>,
    installation_id: &str,
    agent_id: &str,
) -> bool {
    labels.get(LIFECYCLE_LABEL).map(String::as_str) == Some(PROJECT_LIFECYCLE)
        && labels.get(INSTALLATION_LABEL).map(String::as_str) == Some(installation_id)
        && labels.get(AGENT_ID_LABEL).map(String::as_str) == Some(agent_id)
}

fn installation_labels_match(labels: &HashMap<String, String>, installation_id: &str) -> bool {
    labels.get(INSTALLATION_LABEL).map(String::as_str) == Some(installation_id)
}

fn project_labels_match(
    labels: &HashMap<String, String>,
    fingerprint: &str,
    installation_id: &str,
    agent_id: &str,
) -> bool {
    project_ownership_matches(labels, installation_id, agent_id)
        && labels.get(SPEC_FINGERPRINT_LABEL).map(String::as_str) == Some(fingerprint)
}

fn listed_container_agent_id(
    container_name: &str,
    labels: &HashMap<String, String>,
    installation_id: Option<&str>,
) -> Option<String> {
    match (installation_id, labels.get(INSTALLATION_LABEL)) {
        (Some(expected), Some(owner)) if owner == expected => {}
        (None, None) => {}
        _ => return None,
    }

    if labels.get(LIFECYCLE_LABEL).map(String::as_str) == Some(PROJECT_LIFECYCLE) {
        let installation_id = installation_id?;
        let agent_id = labels.get(AGENT_ID_LABEL)?;
        return project_ownership_matches(labels, installation_id, agent_id)
            .then(|| agent_id.clone());
    }

    container_name
        .strip_prefix("xpressclaw-")
        .map(str::to_string)
}

fn workload_ownership_matches(
    container_name: &str,
    labels: &HashMap<String, String>,
    installation_id: Option<&str>,
    agent_id: &str,
) -> bool {
    if let Some(installation_id) = installation_id {
        if container_name == project_container_name(installation_id, agent_id) {
            return project_ownership_matches(labels, installation_id, agent_id);
        }
    }
    listed_container_agent_id(container_name, labels, installation_id).as_deref() == Some(agent_id)
}

fn recorded_container_id_matches(actual_id: Option<&str>, recorded_id: &str) -> bool {
    !recorded_id.is_empty() && actual_id == Some(recorded_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained_fixture(spec: &ContainerSpec) -> ContainerInspectResponse {
        serde_json::from_value(serde_json::json!({
            "Id": "a".repeat(64), "Name": "/retained-agent", "Image": "sha256:original",
            "Config": {"Image": spec.image, "Labels": project_container_labels(&container_spec_fingerprint(spec).unwrap(), "installation", "agent")},
            "State": {"Running": true}
        })).unwrap()
    }

    #[test]
    fn retained_environment_keeps_image_but_rejects_changed_authority_or_settings() {
        let spec = ContainerSpec {
            environment: vec!["TOKEN=secret".into()],
            ..Default::default()
        };
        let container = retained_fixture(&spec);
        let mut changed = spec.clone();
        changed.image = "new-image:latest".into();
        validate_retained_spec(&container, &changed, "installation", "agent").unwrap();
        assert_eq!(
            retained_process_fingerprint(&spec).unwrap(),
            retained_process_fingerprint(&changed).unwrap()
        );
        changed.environment.clear();
        let error = validate_retained_spec(&container, &changed, "installation", "agent")
            .unwrap_err()
            .to_string();
        assert!(error.contains("preserved"));
        assert!(!error.contains("secret"));
        for changed in [
            ContainerSpec {
                cmd: Some(vec!["other-harness".into()]),
                ..spec.clone()
            },
            ContainerSpec {
                volumes: vec![VolumeMount::parse("/host:/container").unwrap()],
                ..spec.clone()
            },
            ContainerSpec {
                working_dir: Some("/other".into()),
                ..spec.clone()
            },
            ContainerSpec {
                run_as_host_user: true,
                ..spec.clone()
            },
            ContainerSpec {
                memory_limit: Some(1),
                ..spec.clone()
            },
            ContainerSpec {
                network_mode: Some("host".into()),
                ..spec.clone()
            },
        ] {
            assert!(validate_retained_spec(&container, &changed, "installation", "agent").is_err());
        }
        assert!(validate_retained_spec(&container, &spec, "other-installation", "agent").is_err());
    }

    #[test]
    fn dependency_references_include_namespace_sharing_volumes_and_links() {
        let id = "a".repeat(64);
        for host in [
            HostConfig {
                network_mode: Some(format!("container:{id}")),
                ..Default::default()
            },
            HostConfig {
                ipc_mode: Some("container:retained-agent".into()),
                ..Default::default()
            },
            HostConfig {
                pid_mode: Some(format!("container:{}", &id[..12])),
                ..Default::default()
            },
            HostConfig {
                volumes_from: Some(vec!["/retained-agent:ro".into()]),
                ..Default::default()
            },
            HostConfig {
                links: Some(vec!["/retained-agent:/consumer/parent".into()]),
                ..Default::default()
            },
        ] {
            assert!(container_depends_on(&host, &id, "retained-agent"));
        }
        for host in [
            HostConfig::default(),
            HostConfig {
                network_mode: Some("bridge".into()),
                ..Default::default()
            },
            HostConfig {
                network_mode: Some("container:other".into()),
                ..Default::default()
            },
        ] {
            assert!(!container_depends_on(&host, &id, "retained-agent"));
        }
    }

    async fn mock_engine(
        mut responses: HashMap<String, (u16, serde_json::Value)>,
    ) -> (
        DockerManager,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        responses.insert(
            "GET /containers/xpressclaw-agent/json".into(),
            (
                404,
                serde_json::json!({"message":"absent legacy container"}),
            ),
        );
        for agent in ["agent", "zed", "attempt-test"] {
            responses
                .entry(format!(
                    "GET /containers/{}-recovery/json",
                    project_container_name("installation", agent)
                ))
                .or_insert((
                    404,
                    serde_json::json!({"message":"no recovery in progress"}),
                ));
        }
        responses
            .entry("GET /containers/json".into())
            .or_insert((200, serde_json::json!([])));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let mut socket = BufReader::new(socket);
                let mut first = String::new();
                socket.read_line(&mut first).await.unwrap();
                let mut fields = first.split_whitespace();
                let method = fields.next().unwrap_or_default();
                let url = fields.next().unwrap_or_default();
                let path = url
                    .find("/containers")
                    .map(|i| &url[i..])
                    .unwrap_or(url)
                    .split('?')
                    .next()
                    .unwrap();
                let request = format!("{method} {path}");
                recorded.lock().unwrap().push(request.clone());
                let (code, body) = responses
                    .get(&request)
                    .cloned()
                    .unwrap_or((500, serde_json::json!({"message": "unexpected request"})));
                let body = body.to_string();
                let response = format!("HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        let docker = Docker::connect_with_http(
            &format!("http://{address}"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let manager = DockerManager {
            docker,
            rootless: false,
            socket_path: None,
            runtime: "podman",
            runtime_version: None,
            docker_desktop: false,
            selinux: false,
            installation_id: Some("installation".into()),
            forwarding_lifecycle: Default::default(),
            forwards: Default::default(),
        };
        (manager, requests, server)
    }

    #[tokio::test]
    async fn incompatible_launch_does_not_stop_or_delete_existing_environment() {
        let spec = ContainerSpec::default();
        let parent = serde_json::to_value(retained_fixture(&spec)).unwrap();
        let name = project_container_name("installation", "agent");
        let (docker, requests, server) = mock_engine(HashMap::from([(
            format!("GET /containers/{name}/json"),
            (200, parent),
        )]))
        .await;
        let changed = ContainerSpec {
            environment: vec!["NEW_KEY=value".into()],
            ..spec
        };
        assert!(docker
            .launch_project_attached("agent", &changed)
            .await
            .is_err());
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET ")));
        server.abort();
    }

    #[tokio::test]
    async fn automatic_recovery_refuses_foreign_environments_and_backups() {
        for foreign_backup in [false, true] {
            let spec = ContainerSpec::default();
            let name = project_container_name("installation", "agent");
            let mut foreign = serde_json::to_value(retained_fixture(&spec)).unwrap();
            foreign["Config"]["Labels"][INSTALLATION_LABEL] = "another-installation".into();
            let reference = if foreign_backup {
                format!("{name}-recovery")
            } else {
                name
            };
            let (docker, requests, server) = mock_engine(HashMap::from([(
                format!("GET /containers/{reference}/json"),
                (200, foreign),
            )]))
            .await;
            assert!(docker
                .project_configuration_needs_recovery("agent", &spec)
                .await
                .is_err());
            assert!(docker
                .recover_project_configuration("agent", &spec)
                .await
                .is_err());
            assert!(requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.starts_with("GET ")));
            server.abort();
        }
    }

    #[tokio::test]
    async fn removal_checks_running_and_stopped_unlabelled_dependents_before_any_stop() {
        for running in [true, false] {
            let spec = ContainerSpec::default();
            let parent = serde_json::to_value(retained_fixture(&spec)).unwrap();
            let name = project_container_name("installation", "agent");
            let (docker, requests, server) = mock_engine(HashMap::from([
                (format!("GET /containers/{name}/json"), (200, parent)),
                ("GET /containers/json".into(), (200, serde_json::json!([{"Id":"child"}]))),
                ("GET /containers/child/json".into(), (200, serde_json::json!({"Id":"child", "Name":"/experiment", "State":{"Running":running}, "HostConfig":{"NetworkMode":format!("container:{}", "a".repeat(64))}}))),
            ])).await;
            let error = docker
                .remove_owned_workload("agent")
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("experiment"), "{error}");
            assert!(error.contains("preserved"));
            if running {
                let changed = ContainerSpec {
                    environment: vec!["NEW_KEY=value".into()],
                    ..spec
                };
                let recovery_error = docker
                    .recover_project_configuration("agent", &changed)
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(recovery_error.contains("experiment"), "{recovery_error}");
            }
            assert!(requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.starts_with("GET ")));
            server.abort();
        }
    }

    #[tokio::test]
    async fn recovery_does_not_repoint_stopped_helpers_that_reference_the_parent_name() {
        let spec = ContainerSpec::default();
        let name = project_container_name("installation", "agent");
        let parent = serde_json::to_value(retained_fixture(&spec)).unwrap();
        let (docker, requests, server) = mock_engine(HashMap::from([
            (format!("GET /containers/{name}/json"), (200, parent)),
            ("GET /containers/json".into(), (200, serde_json::json!([{"Id":"child"}]))),
            ("GET /containers/child/json".into(), (200, serde_json::json!({"Id":"child", "Name":"/named-helper", "State":{"Running":false}, "HostConfig":{"NetworkMode":"container:retained-agent"}}))),
        ])).await;
        let changed = ContainerSpec {
            environment: vec!["NEW_KEY=value".into()],
            ..spec
        };
        let error = docker
            .recover_project_configuration("agent", &changed)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("by name"), "{error}");
        assert!(error.contains("named-helper"));
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET ")));
        server.abort();
    }

    #[tokio::test]
    async fn dependency_scan_errors_do_not_authorize_removal() {
        let name = project_container_name("installation", "agent");
        let parent = serde_json::to_value(retained_fixture(&ContainerSpec::default())).unwrap();
        let (docker, requests, server) = mock_engine(HashMap::from([
            (format!("GET /containers/{name}/json"), (200, parent)),
            (
                "GET /containers/json".into(),
                (500, serde_json::json!({"message":"engine unavailable"})),
            ),
        ]))
        .await;
        assert!(docker
            .remove_owned_workload("agent")
            .await
            .unwrap_err()
            .to_string()
            .contains("Could not list preserved"));
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET ")));
        server.abort();
    }

    #[tokio::test]
    async fn stopping_an_absent_attempt_is_idempotent_but_inspection_failure_is_not() {
        for status in [404, 500] {
            let (docker, requests, server) = mock_engine(HashMap::from([
                (
                    format!(
                        "GET /containers/{}/json",
                        project_container_name("installation", "attempt-test")
                    ),
                    (404, serde_json::json!({"message":"no retained attempt"})),
                ),
                (
                    "GET /containers/xpressclaw-attempt-test/json".into(),
                    (status, serde_json::json!({"message":"inspect failed"})),
                ),
                (
                    "DELETE /containers/xpressclaw-attempt-test".into(),
                    (404, serde_json::json!({"message":"no attempt container"})),
                ),
            ]))
            .await;
            let result = docker.stop("attempt-test").await;
            assert_eq!(result.is_ok(), status == 404);
            if status == 500 {
                assert!(requests
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|request| request.starts_with("GET ")));
            }
            server.abort();
        }
    }

    fn mixed_owned_environments() -> HashMap<String, (u16, serde_json::Value)> {
        let name = project_container_name("installation", "zed");
        assert!(name.as_str() < "xpressclaw-zed");
        let retained = serde_json::json!({
            "Id": "current", "Name": name,
            "Config": {"Labels": project_container_labels("spec", "installation", "zed")},
            "HostConfig": {}
        });
        let legacy = serde_json::json!({
            "Id": "legacy", "Name": "xpressclaw-zed",
            "Config": {"Labels": {INSTALLATION_LABEL: "installation"}},
            "HostConfig": {}
        });
        HashMap::from([
            (
                format!("GET /containers/{name}/json"),
                (200, retained.clone()),
            ),
            ("GET /containers/current/json".into(), (200, retained)),
            (
                "GET /containers/xpressclaw-zed/json".into(),
                (200, legacy.clone()),
            ),
            ("GET /containers/legacy/json".into(), (200, legacy)),
            (
                "GET /containers/json".into(),
                (
                    200,
                    serde_json::json!([
                        {"Id":"current"}, {"Id":"legacy"}, {"Id":"child"}
                    ]),
                ),
            ),
            (
                "GET /containers/child/json".into(),
                (
                    200,
                    serde_json::json!({
                        "Id":"child", "Name":"/legacy-experiment",
                        "HostConfig":{"NetworkMode":"container:legacy"}
                    }),
                ),
            ),
            (
                format!("DELETE /containers/{name}"),
                (204, serde_json::Value::Null),
            ),
            (
                "DELETE /containers/xpressclaw-zed".into(),
                (204, serde_json::Value::Null),
            ),
        ])
    }

    #[tokio::test]
    async fn owned_removal_preflights_legacy_dependencies_and_inspection_before_any_deletion() {
        for inspection_failure in [false, true] {
            let mut responses = mixed_owned_environments();
            if inspection_failure {
                responses.insert(
                    "GET /containers/xpressclaw-zed/json".into(),
                    (
                        500,
                        serde_json::json!({"message":"legacy inspection failed"}),
                    ),
                );
            }
            for preflight_only in [false, true] {
                let (docker, requests, server) = mock_engine(responses.clone()).await;
                let result = if preflight_only {
                    docker.ensure_project_removable("zed").await
                } else {
                    docker.remove_owned_workload("zed").await
                };
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains(if inspection_failure {
                        "legacy inspection failed"
                    } else {
                        "legacy-experiment"
                    }),
                    "{error}"
                );
                assert!(requests.lock().unwrap().iter().all(|request| request.starts_with("GET ")),
                    "inspection_failure={inspection_failure}, preflight_only={preflight_only}: {:?}", requests.lock().unwrap());
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn project_preflight_checks_legacy_dependencies_without_a_retained_container() {
        let mut responses = mixed_owned_environments();
        responses.insert(
            format!(
                "GET /containers/{}/json",
                project_container_name("installation", "zed")
            ),
            (404, serde_json::json!({"message":"no retained container"})),
        );
        let (docker, requests, server) = mock_engine(responses).await;
        let error = docker
            .ensure_project_removable("zed")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("legacy-experiment"), "{error}");
        assert!(requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET ")));
        server.abort();
    }

    #[tokio::test]
    async fn owned_removal_checks_both_environments_before_deleting_either() {
        let mut responses = mixed_owned_environments();
        responses.insert(
            "GET /containers/json".into(),
            (
                200,
                serde_json::json!([
                    {"Id":"current"}, {"Id":"legacy"}
                ]),
            ),
        );
        let (docker, requests, server) = mock_engine(responses).await;
        docker.ensure_project_removable("zed").await.unwrap();
        requests.lock().unwrap().clear();
        docker.remove_owned_workload("zed").await.unwrap();
        let requests = requests.lock().unwrap();
        let first_deletion = requests
            .iter()
            .position(|request| request.starts_with("DELETE "))
            .unwrap();
        assert!(requests[..first_deletion]
            .iter()
            .any(|request| request == "GET /containers/xpressclaw-zed/json"));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("DELETE "))
                .count(),
            2
        );
        assert!(!requests.iter().any(|request| request.starts_with("POST ")));
        server.abort();
    }

    #[tokio::test]
    async fn owned_removal_preserves_foreign_legacy_container_and_its_dependents() {
        let mut responses = mixed_owned_environments();
        for reference in ["xpressclaw-zed", "legacy"] {
            responses
                .get_mut(&format!("GET /containers/{reference}/json"))
                .unwrap()
                .1["Config"]["Labels"][INSTALLATION_LABEL] =
                serde_json::json!("other-installation");
        }
        let (docker, requests, server) = mock_engine(responses).await;
        docker.ensure_project_removable("zed").await.unwrap();
        docker.remove_owned_workload("zed").await.unwrap();
        let requests = requests.lock().unwrap();
        let deleted: Vec<_> = requests
            .iter()
            .filter(|request| request.starts_with("DELETE "))
            .collect();
        assert_eq!(
            deleted,
            vec![&format!(
                "DELETE /containers/{}",
                project_container_name("installation", "zed")
            )]
        );
        server.abort();
    }

    #[test]
    fn terminal_session_names_accept_only_bounded_ascii_identifiers() {
        for name in ["xpressclaw", "login_2-test", "0", &"a".repeat(64)] {
            assert!(valid_terminal_session_name(name), "{name:?}");
        }
        for name in [
            "",
            "two words",
            "a.b",
            "a:b",
            "a/b",
            "a\nb",
            "\0",
            "$(id)",
            ";id",
            "ログイン",
            &"a".repeat(65),
        ] {
            assert!(!valid_terminal_session_name(name), "{name:?}");
        }
    }

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
    fn identifies_docker_desktop_from_daemon_info() {
        assert!(docker_desktop_from_info(
            "docker",
            Some("Docker Desktop"),
            None
        ));
        assert!(docker_desktop_from_info(
            "docker",
            None,
            Some("docker-desktop")
        ));
        assert!(!docker_desktop_from_info(
            "podman",
            Some("Docker Desktop"),
            Some("docker-desktop")
        ));
        assert!(!docker_desktop_from_info(
            "docker",
            Some("Ubuntu 24.04 LTS"),
            Some("builder")
        ));
    }

    #[test]
    fn recognizes_runtime_selinux_security_option() {
        assert!(has_security_option(
            &["name=seccomp,profile=default".into(), "name=selinux".into()],
            "selinux"
        ));
        assert!(has_security_option(&["NAME=ROOTLESS".into()], "rootless"));
        assert!(!has_security_option(
            &["name=seccomp,profile=default".into()],
            "selinux"
        ));
    }

    #[test]
    fn parses_docker_mount_access_and_selinux_options() {
        let shared = VolumeMount::parse("/srv/project:/workspace:ro,z").unwrap();
        assert!(shared.read_only);
        assert_eq!(shared.selinux_relabel, SelinuxRelabel::Shared);

        let private = VolumeMount::parse("/srv/login:/home/node/.pi:rw,Z").unwrap();
        assert!(!private.read_only);
        assert_eq!(private.selinux_relabel, SelinuxRelabel::Private);

        let ordinary = VolumeMount::parse("/srv/reference:/reference").unwrap();
        assert!(!ordinary.read_only);
        assert_eq!(ordinary.selinux_relabel, SelinuxRelabel::None);

        let windows_source = VolumeMount::parse(r"C:\Users\me\project:/workspace").unwrap();
        assert_eq!(windows_source.source, r"C:\Users\me\project");
        assert_eq!(windows_source.target, "/workspace");

        assert!(VolumeMount::parse("/srv/cache:/workspace/cache:cached").is_none());
        assert!(VolumeMount::parse("/srv/cache:/workspace/cache:ro,cached").is_none());

        let legacy: VolumeMount = serde_json::from_value(serde_json::json!({
            "source": "/srv/legacy",
            "target": "/legacy",
            "read_only": false
        }))
        .unwrap();
        assert_eq!(legacy.selinux_relabel, SelinuxRelabel::None);
    }

    #[test]
    fn selinux_bind_mounts_use_relabel_options_without_touching_sockets() {
        let volumes = vec![
            VolumeMount::parse("/srv/project:/workspace:rw,z").unwrap(),
            VolumeMount::parse("/home/user/.pi:/home/node/.pi:ro,z").unwrap(),
            VolumeMount::parse("/run/user/1000/podman.sock:/var/run/docker.sock").unwrap(),
            VolumeMount::parse("agent-cache:/cache:Z").unwrap(),
        ];
        let rendered = runtime_mounts(&volumes, true).unwrap();
        assert_eq!(
            rendered.binds,
            vec![
                "/srv/project:/workspace:rw,z",
                "/home/user/.pi:/home/node/.pi:ro,z"
            ]
        );
        assert_eq!(rendered.mounts.len(), 2);
        assert_eq!(rendered.mounts[0].typ, Some(MountTypeEnum::BIND));
        assert_eq!(
            rendered.mounts[0].target.as_deref(),
            Some("/var/run/docker.sock")
        );
        assert_eq!(rendered.mounts[1].typ, Some(MountTypeEnum::VOLUME));

        let without_selinux = runtime_mounts(&volumes, false).unwrap();
        assert!(without_selinux.binds.is_empty());
        assert_eq!(without_selinux.mounts.len(), volumes.len());
    }

    #[test]
    fn bind_mounts_normalize_windows_verbatim_sources_on_every_host() {
        let volumes = vec![
            VolumeMount {
                source: r"\\?\C:\workspace\project".into(),
                target: "/workspace".into(),
                read_only: false,
                selinux_relabel: SelinuxRelabel::None,
            },
            VolumeMount {
                source: r"\\?\UNC\server\share\project".into(),
                target: "/network-workspace".into(),
                read_only: true,
                selinux_relabel: SelinuxRelabel::None,
            },
        ];

        let rendered = runtime_mounts(&volumes, false).unwrap();
        assert_eq!(
            rendered.mounts[0].source.as_deref(),
            Some(r"C:\workspace\project")
        );
        assert_eq!(
            rendered.mounts[1].source.as_deref(),
            Some(r"\\server\share\project")
        );
    }

    #[test]
    fn socket_mounts_add_the_host_socket_group_for_any_target() {
        let source = std::env::temp_dir().join(format!(
            "xpressclaw-socket-group-test-{}",
            std::process::id()
        ));
        #[cfg(unix)]
        let _listener = std::os::unix::net::UnixListener::bind(&source).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&source, []).unwrap();
        let mut spec = ContainerSpec::default();
        spec.volumes.push(VolumeMount {
            source: source.display().to_string(),
            target: "/tmp/forwarded-agent.sock".to_string(),
            read_only: false,
            selinux_relabel: SelinuxRelabel::None,
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
        let labels = project_container_labels("expected", "installation-a", "agent-a");
        assert!(project_labels_match(
            &labels,
            "expected",
            "installation-a",
            "agent-a"
        ));
        assert!(!project_labels_match(
            &labels,
            "changed",
            "installation-a",
            "agent-a"
        ));
        assert!(!project_labels_match(
            &labels,
            "expected",
            "installation-b",
            "agent-a"
        ));

        let transient =
            HashMap::from([(SPEC_FINGERPRINT_LABEL.to_string(), "expected".to_string())]);
        assert!(!project_labels_match(
            &transient,
            "expected",
            "installation-a",
            "agent-a"
        ));
    }

    #[test]
    fn installation_network_labels_must_match_the_current_control_plane() {
        let labels =
            HashMap::from([(INSTALLATION_LABEL.to_string(), "installation-a".to_string())]);
        assert!(installation_labels_match(&labels, "installation-a"));
        assert!(!installation_labels_match(&labels, "installation-b"));
        assert!(!installation_labels_match(
            &HashMap::new(),
            "installation-a"
        ));
    }

    #[test]
    fn project_container_names_are_deterministic_docker_safe_and_unicode_agnostic() {
        let japanese = project_container_name("installation-a", "エリ-codex");
        assert_eq!(
            japanese,
            project_container_name("installation-a", "エリ-codex")
        );
        assert_ne!(
            japanese,
            project_container_name("installation-a", "別のエージェント")
        );
        assert_ne!(
            japanese,
            project_container_name("installation-b", "エリ-codex")
        );
        assert!(japanese.len() <= 255);
        assert!(japanese
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character)));
    }

    #[test]
    fn container_listing_ignores_projects_owned_by_another_installation() {
        let ours = project_container_labels("fingerprint", "installation-a", "エリ-codex");
        let theirs = project_container_labels("fingerprint", "installation-b", "エリ-codex");
        let name = project_container_name("installation-a", "エリ-codex");

        assert_eq!(
            listed_container_agent_id(&name, &ours, Some("installation-a")).as_deref(),
            Some("エリ-codex")
        );
        assert_eq!(
            listed_container_agent_id(&name, &theirs, Some("installation-a")),
            None
        );
        assert_eq!(
            listed_container_agent_id("xpressclaw-attempt-123", &HashMap::new(), None).as_deref(),
            Some("attempt-123")
        );
        assert_eq!(
            listed_container_agent_id(
                "xpressclaw-attempt-123",
                &HashMap::new(),
                Some("installation-a")
            ),
            None
        );
        assert_eq!(
            listed_container_agent_id(
                "xpressclaw-attempt-123",
                &HashMap::from([(INSTALLATION_LABEL.to_string(), "installation-a".to_string())]),
                Some("installation-a")
            )
            .as_deref(),
            Some("attempt-123")
        );
    }

    #[test]
    fn destructive_workload_cleanup_requires_matching_ownership() {
        let retained_name = project_container_name("installation-a", "atlas");
        let retained_labels = project_container_labels("fingerprint", "installation-a", "atlas");
        assert!(workload_ownership_matches(
            &retained_name,
            &retained_labels,
            Some("installation-a"),
            "atlas"
        ));
        assert!(!workload_ownership_matches(
            &retained_name,
            &project_container_labels("fingerprint", "installation-b", "atlas"),
            Some("installation-a"),
            "atlas"
        ));
        assert!(workload_ownership_matches(
            "xpressclaw-attempt-123",
            &HashMap::from([(INSTALLATION_LABEL.to_string(), "installation-a".to_string())]),
            Some("installation-a"),
            "attempt-123"
        ));
        assert!(!workload_ownership_matches(
            "xpressclaw-attempt-123",
            &HashMap::new(),
            Some("installation-a"),
            "attempt-123"
        ));
    }

    #[test]
    fn legacy_cleanup_requires_the_exact_recorded_container_id() {
        let recorded = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert!(recorded_container_id_matches(Some(recorded), recorded));
        assert!(!recorded_container_id_matches(
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"),
            recorded
        ));
        assert!(!recorded_container_id_matches(
            Some(recorded),
            &recorded[..12]
        ));
        assert!(!recorded_container_id_matches(None, recorded));
        assert!(!recorded_container_id_matches(Some(""), ""));
    }
}
