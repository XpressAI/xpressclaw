use std::collections::HashMap;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, ListContainersOptions, LogsOptions,
    RemoveContainerOptions, StopContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

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

/// Manages Docker/Podman containers for agent isolation.
pub struct DockerManager {
    docker: Docker,
}

impl DockerManager {
    /// Connect to the Docker/Podman daemon.
    pub async fn connect() -> Result<Self> {
        // Use connect_with_defaults so DOCKER_HOST is honored for all
        // schemes (unix, tcp, http, npipe). This is needed for rootless
        // Podman which sets DOCKER_HOST to a user-level socket.
        let docker = Docker::connect_with_defaults()
            .map_err(|e| Error::DockerNotAvailable(e.to_string()))?;

        // Verify connection
        docker.ping().await.map_err(|e| {
            Error::DockerNotAvailable(format!(
                "Cannot reach Docker/Podman daemon: {e}\n\
                 Ensure Docker or Podman is running."
            ))
        })?;

        info!("connected to container runtime");
        Ok(Self { docker })
    }

    /// Launch an agent container.
    pub async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo> {
        let container_name = format!("xpressclaw-{agent_id}");

        // Remove existing container if present
        let _ = self.remove(&container_name).await;

        // Build mounts — detect named volumes vs bind mounts
        let mounts: Vec<Mount> = spec
            .volumes
            .iter()
            .map(|v| {
                // Named volumes don't start with / or ~ (they're just names like "xpressclaw-workspace-dev")
                let is_named_volume = !v.source.starts_with('/') && !v.source.starts_with('~');
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
            env: Some(env),
            host_config: Some(host_config),
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            cmd: spec.cmd.clone(),
            working_dir: spec.working_dir.clone(),
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

    /// Stop and remove an agent container.
    pub async fn stop(&self, agent_id: &str) -> Result<()> {
        let container_name = format!("xpressclaw-{agent_id}");

        let stop_opts = StopContainerOptions { t: 10 };
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
    }
}
