use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, LogOutput, LogsOptions, NetworkingConfig,
    RemoveContainerOptions, StopContainerOptions,
};
use bollard::models::{
    EndpointSettings, HealthConfig, HostConfig, Mount, MountTypeEnum, PortBinding, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::network::{CreateNetworkOptions, DisconnectNetworkOptions};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::{
    local_http_client_builder, network_name, resource_prefix, CollaborationConfig,
    CollaborationSecrets, GITBUCKET_INTERNAL_URL, JENKINS_INTERNAL_URL,
};
use crate::docker::manager::DockerManager;
use crate::error::{Error, Result};

pub const INSTALLATION_LABEL: &str = "io.xpressclaw.installation";
const SERVICE_USER: &str = "xpressclaw-agent";
const JENKINS_USER: &str = "xpressclaw";
const JENKINS_JOB: &str = "xpressclaw-local-build";
const JENKINS_AGENT: &str = "xpressclaw-builder";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationStackStatus {
    pub configured: bool,
    pub docker_available: bool,
    pub network: String,
    pub data_path: String,
    pub gitbucket: CollaborationServiceStatus,
    pub jenkins: CollaborationServiceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationServiceStatus {
    pub state: String,
    pub health: String,
    pub image: String,
    pub version: String,
    pub host_url: String,
    pub internal_url: String,
    pub volume: String,
    pub error: Option<String>,
}

pub struct CollaborationStack<'a> {
    docker: &'a DockerManager,
    config: &'a CollaborationConfig,
    data_dir: &'a Path,
    installation_id: &'a str,
}

impl<'a> CollaborationStack<'a> {
    pub fn new(
        docker: &'a DockerManager,
        config: &'a CollaborationConfig,
        data_dir: &'a Path,
        installation_id: &'a str,
    ) -> Self {
        Self {
            docker,
            config,
            data_dir,
            installation_id,
        }
    }

    pub fn service_user() -> &'static str {
        SERVICE_USER
    }

    pub async fn status(&self) -> CollaborationStackStatus {
        let prefix = resource_prefix(self.installation_id);
        CollaborationStackStatus {
            configured: self.config.enabled,
            docker_available: true,
            network: network_name(self.installation_id),
            data_path: self.data_dir.join("collaboration").display().to_string(),
            gitbucket: self
                .service_status(
                    &format!("{prefix}-gitbucket"),
                    &self.config.gitbucket_image,
                    &self.config.gitbucket_url(),
                    GITBUCKET_INTERNAL_URL,
                    &format!("{prefix}-gitbucket-data"),
                )
                .await,
            jenkins: self
                .service_status(
                    &format!("{prefix}-jenkins"),
                    &self.config.jenkins_image,
                    &self.config.jenkins_url(),
                    JENKINS_INTERNAL_URL,
                    &format!("{prefix}-jenkins-data"),
                )
                .await,
        }
    }

    async fn service_status(
        &self,
        name: &str,
        expected_image: &str,
        host_url: &str,
        internal_url: &str,
        volume: &str,
    ) -> CollaborationServiceStatus {
        let inspected = self.docker.inspect_by_name(name).await;
        let (state, health, image, error) = match inspected {
            None => (
                "not_installed".to_string(),
                "unknown".to_string(),
                expected_image.to_string(),
                None,
            ),
            Some(container) => {
                let labels = container
                    .config
                    .as_ref()
                    .and_then(|config| config.labels.as_ref());
                if !resource_owned(labels, self.installation_id) {
                    return CollaborationServiceStatus {
                        state: "conflict".to_string(),
                        health: "unknown".to_string(),
                        image: expected_image.to_string(),
                        version: expected_image
                            .rsplit(':')
                            .next()
                            .unwrap_or("unknown")
                            .to_string(),
                        host_url: host_url.to_string(),
                        internal_url: internal_url.to_string(),
                        volume: volume.to_string(),
                        error: Some(format!(
                            "Docker resource {name} exists but is not managed by this XpressClaw installation"
                        )),
                    };
                }
                let state = container.state.unwrap_or_default();
                let status = state
                    .status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let health = state
                    .health
                    .and_then(|value| value.status)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| {
                        if state.running.unwrap_or(false) {
                            "starting".to_string()
                        } else {
                            "unknown".to_string()
                        }
                    });
                (
                    status,
                    health,
                    container
                        .config
                        .and_then(|value| value.image)
                        .unwrap_or_else(|| expected_image.to_string()),
                    state.error.filter(|value| !value.is_empty()),
                )
            }
        };
        CollaborationServiceStatus {
            state,
            health,
            version: image.rsplit(':').next().unwrap_or("unknown").to_string(),
            image,
            host_url: host_url.to_string(),
            internal_url: internal_url.to_string(),
            volume: volume.to_string(),
            error,
        }
    }

    /// Pull, create, securely bootstrap, and start both services. Repeated
    /// calls preserve volumes and do not rotate working credentials.
    pub async fn install(&self) -> Result<CollaborationStackStatus> {
        self.config.validate().map_err(Error::ConfigValidation)?;
        if !self.config.enabled {
            return Err(Error::ConfigValidation(
                "enable Local collaboration services before installing them".to_string(),
            ));
        }
        let mut secrets = CollaborationSecrets::load_or_create(self.data_dir)?;
        self.docker.pull_image(&self.config.gitbucket_image).await?;
        self.docker.pull_image(&self.config.jenkins_image).await?;
        self.ensure_volumes().await?;
        let bootstrap_network = bootstrap_network_name(self.installation_id);
        self.ensure_network_named(&bootstrap_network, "gitbucket-bootstrap-network")
            .await?;

        // The first phase deliberately uses a separate bootstrap-only network
        // and no host port. Authorized Agent containers can join only the
        // final collaboration network, so they cannot reach root/root while a
        // fresh GitBucket volume is being hardened.
        let gitbucket_result: Result<()> = async {
            self.recreate_gitbucket(false, &bootstrap_network).await?;
            self.wait_container_healthy(&self.container_names()[0], "GitBucket")
                .await?;
            let token = self
                .bootstrap_gitbucket_internal(&secrets, &bootstrap_network)
                .await?;
            if secrets.gitbucket_service_token.as_deref() != Some(token.as_str()) {
                secrets.gitbucket_service_token = Some(token);
                secrets.save(self.data_dir)?;
            }
            self.ensure_network().await?;
            let collaboration_network = network_name(self.installation_id);
            self.recreate_gitbucket(true, &collaboration_network)
                .await?;
            self.wait_container_healthy(&self.container_names()[0], "GitBucket")
                .await?;
            self.wait_http(&self.config.gitbucket_url(), "GitBucket")
                .await?;
            self.remove_managed_network(&bootstrap_network).await?;
            Ok(())
        }
        .await;
        if let Err(error) = gitbucket_result {
            self.remove_gitbucket_after_failed_setup().await;
            return Err(error);
        }

        let jenkins_result: Result<()> = async {
            let mut first_jenkins_install = !secrets.jenkins_initialized;
            if !first_jenkins_install {
                self.recreate_jenkins(None).await?;
                self.wait_http(&self.config.jenkins_url(), "Jenkins")
                    .await?;
                first_jenkins_install = !self.jenkins_auth_valid(&secrets.jenkins_password).await;
            }
            if first_jenkins_install {
                self.recreate_jenkins(Some(secrets.jenkins_password.as_str()))
                    .await?;
                self.wait_http(&self.config.jenkins_url(), "Jenkins")
                    .await?;
                // Remove the bootstrap-only password environment from the
                // final inspectable container; the account remains in its
                // named volume.
                self.recreate_jenkins(None).await?;
                self.wait_http(&self.config.jenkins_url(), "Jenkins")
                    .await?;
            }
            self.prepare_jenkins_build_agent(&secrets.jenkins_password)
                .await?;
            self.ensure_jenkins_job(&secrets.jenkins_password).await?;
            if first_jenkins_install {
                secrets.jenkins_initialized = true;
                secrets.save(self.data_dir)?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = jenkins_result {
            // A failed bootstrap must not leave a container whose inspectable
            // environment contains the generated administrator password.
            self.remove_jenkins_after_failed_setup().await;
            return Err(error);
        }
        Ok(self.status().await)
    }

    async fn remove_gitbucket_after_failed_setup(&self) {
        for name in [
            self.container_names()[0].clone(),
            bootstrap_container_name(self.installation_id),
        ] {
            self.remove_owned_container_after_failed_setup(&name).await;
        }
        self.remove_owned_network_after_failed_setup(&bootstrap_network_name(self.installation_id))
            .await;
    }

    async fn remove_jenkins_after_failed_setup(&self) {
        for name in &self.container_names()[1..] {
            self.remove_owned_container_after_failed_setup(name).await;
        }
    }

    /// Best-effort failure cleanup must never turn a name collision into a
    /// destructive operation. Inspect ownership first and remove by immutable
    /// container ID so an external name replacement cannot redirect cleanup.
    async fn remove_owned_container_after_failed_setup(&self, name: &str) {
        let Some(existing) = self.docker.inspect_by_name(name).await else {
            return;
        };
        let labels = existing
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        if !resource_owned(labels, self.installation_id) {
            return;
        }
        let Some(id) = existing.id.as_deref() else {
            return;
        };
        let _ = self
            .docker
            .client()
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    async fn remove_owned_network_after_failed_setup(&self, name: &str) {
        let Ok(existing) = self
            .docker
            .client()
            .inspect_network::<String>(name, None)
            .await
        else {
            return;
        };
        if !resource_owned(existing.labels.as_ref(), self.installation_id) {
            return;
        }
        let _ = self.docker.client().remove_network(name).await;
    }

    async fn jenkins_auth_valid(&self, password: &str) -> bool {
        let Ok(client) = local_http_client_builder().build() else {
            return false;
        };
        client
            .get(format!("{}/me/api/json", self.config.jenkins_url()))
            .basic_auth(JENKINS_USER, Some(password))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    pub async fn start(&self) -> Result<CollaborationStackStatus> {
        let names = self.container_names();
        for name in &names {
            if !self.managed_container_present(name).await? {
                return Err(Error::Container(format!(
                    "{name} is not installed; choose Install services first"
                )));
            }
        }
        for name in names {
            if !self.docker.is_container_running(&name).await {
                self.docker
                    .client()
                    .start_container::<String>(&name, None)
                    .await
                    .map_err(|error| {
                        Error::Container(format!("failed to start {name}: {error}"))
                    })?;
            }
        }
        Ok(self.status().await)
    }

    pub async fn stop(&self) -> Result<CollaborationStackStatus> {
        for name in self.container_names().into_iter().rev() {
            if self.managed_container_present(&name).await?
                && self.docker.is_container_running(&name).await
            {
                self.docker
                    .client()
                    .stop_container(&name, Some(StopContainerOptions { t: 20 }))
                    .await
                    .map_err(|error| Error::Container(format!("failed to stop {name}: {error}")))?;
            }
        }
        Ok(self.status().await)
    }

    async fn managed_container_present(&self, name: &str) -> Result<bool> {
        let Some(existing) = self.docker.inspect_by_name(name).await else {
            return Ok(false);
        };
        let labels = existing
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        if !resource_owned(labels, self.installation_id) {
            return Err(Error::Container(format!(
                "Docker container {name} already exists but is not managed by this XpressClaw installation"
            )));
        }
        Ok(true)
    }

    pub async fn restart(&self) -> Result<CollaborationStackStatus> {
        // Docker port bindings, images, and several resource settings are
        // immutable after container creation. Use the same idempotent
        // reconciliation path as Install/Upgrade so Restart applies the saved
        // configuration instead of reporting URLs for stale containers.
        self.install().await
    }

    pub async fn upgrade(&self) -> Result<CollaborationStackStatus> {
        // Installation is the idempotent reconciliation path: it pulls the
        // configured pinned images, retains volumes, and verifies that neither
        // service has fallen back to an insecure first-run state.
        self.install().await
    }

    /// Permanently remove all managed data. The HTTP route requires a separate
    /// exact confirmation phrase before calling this method.
    pub async fn reset(&self) -> Result<CollaborationStackStatus> {
        self.stop().await?;
        for name in reset_container_names(self.installation_id) {
            if self.managed_container_present(&name).await? {
                self.docker
                    .client()
                    .remove_container(
                        &name,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                    .map_err(|error| {
                        Error::Container(format!("failed to remove container {name}: {error}"))
                    })?;
            }
        }
        // Remove networks before persistent data. Retained Agent containers
        // can still be attached after access is revoked; detach only this
        // installation's endpoints so a network failure cannot leave service
        // volumes deleted but credentials and the network stranded.
        for network in [
            bootstrap_network_name(self.installation_id),
            network_name(self.installation_id),
        ] {
            self.remove_managed_network(&network).await?;
        }
        let prefix = resource_prefix(self.installation_id);
        for volume in [
            format!("{prefix}-gitbucket-data"),
            format!("{prefix}-jenkins-data"),
        ] {
            if let Ok(existing) = self.docker.client().inspect_volume(&volume).await {
                if !resource_owned(Some(&existing.labels), self.installation_id) {
                    return Err(Error::Container(format!(
                        "Docker volume {volume} exists but is not managed by this XpressClaw installation"
                    )));
                }
                self.docker
                    .client()
                    .remove_volume(&volume, Some(RemoveVolumeOptions { force: true }))
                    .await
                    .map_err(|error| {
                        Error::Container(format!("failed to remove volume {volume}: {error}"))
                    })?;
            }
        }
        let secret_path = CollaborationSecrets::path(self.data_dir);
        if secret_path.exists() {
            std::fs::remove_file(secret_path).map_err(|error| {
                Error::Config(format!(
                    "failed to remove collaboration credentials: {error}"
                ))
            })?;
        }
        Ok(self.status().await)
    }

    fn container_names(&self) -> [String; 3] {
        let prefix = resource_prefix(self.installation_id);
        [
            format!("{prefix}-gitbucket"),
            format!("{prefix}-jenkins"),
            format!("{prefix}-jenkins-agent"),
        ]
    }

    fn labels(&self, service: &str) -> HashMap<String, String> {
        HashMap::from([
            (
                "io.xpressclaw.collaboration".to_string(),
                service.to_string(),
            ),
            (
                INSTALLATION_LABEL.to_string(),
                self.installation_id.to_string(),
            ),
        ])
    }

    async fn ensure_network(&self) -> Result<()> {
        let name = network_name(self.installation_id);
        self.ensure_network_named(&name, "network").await
    }

    async fn ensure_network_named(&self, name: &str, service: &str) -> Result<()> {
        if let Ok(existing) = self
            .docker
            .client()
            .inspect_network::<String>(name, None)
            .await
        {
            if !resource_owned(existing.labels.as_ref(), self.installation_id) {
                return Err(Error::Container(format!(
                    "Docker network {name} already exists but belongs to another installation"
                )));
            }
            return Ok(());
        }
        self.docker
            .client()
            .create_network(CreateNetworkOptions {
                name: name.to_string(),
                check_duplicate: true,
                driver: "bridge".to_string(),
                internal: false,
                attachable: false,
                ingress: false,
                labels: self.labels(service),
                ..Default::default()
            })
            .await
            .map_err(|error| {
                Error::Container(format!("failed to create network {name}: {error}"))
            })?;
        Ok(())
    }

    async fn remove_managed_network(&self, name: &str) -> Result<()> {
        let Ok(existing) = self
            .docker
            .client()
            .inspect_network::<String>(name, None)
            .await
        else {
            return Ok(());
        };
        if !resource_owned(existing.labels.as_ref(), self.installation_id) {
            return Err(Error::Container(format!(
                "Docker network {name} exists but is not managed by this XpressClaw installation"
            )));
        }
        let mut endpoints = existing
            .containers
            .unwrap_or_default()
            .into_keys()
            .collect::<Vec<_>>();
        endpoints.sort();
        for container_id in endpoints {
            let container = self
                .docker
                .client()
                .inspect_container(&container_id, None)
                .await
                .map_err(|error| {
                    Error::Container(format!(
                        "failed to inspect container {container_id} attached to network {name}: {error}"
                    ))
                })?;
            let labels = container
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            if !resource_owned(labels, self.installation_id) {
                let container_name = container
                    .name
                    .as_deref()
                    .unwrap_or(&container_id)
                    .trim_start_matches('/');
                return Err(Error::Container(format!(
                    "Docker network {name} is still attached to container {container_name}, which is not managed by this XpressClaw installation; disconnect it before resetting"
                )));
            }
            self.docker
                .client()
                .disconnect_network(
                    name,
                    DisconnectNetworkOptions {
                        container: container_id.clone(),
                        force: true,
                    },
                )
                .await
                .map_err(|error| {
                    Error::Container(format!(
                        "failed to detach managed container {container_id} from network {name}: {error}"
                    ))
                })?;
        }
        self.docker
            .client()
            .remove_network(name)
            .await
            .map_err(|error| Error::Container(format!("failed to remove network {name}: {error}")))
    }

    async fn ensure_volumes(&self) -> Result<()> {
        let prefix = resource_prefix(self.installation_id);
        for (name, service) in [
            (format!("{prefix}-gitbucket-data"), "gitbucket"),
            (format!("{prefix}-jenkins-data"), "jenkins"),
        ] {
            if let Ok(existing) = self.docker.client().inspect_volume(&name).await {
                if !resource_owned(Some(&existing.labels), self.installation_id) {
                    return Err(Error::Container(format!(
                        "Docker volume {name} already exists but belongs to another installation"
                    )));
                }
                continue;
            }
            self.docker
                .client()
                .create_volume(CreateVolumeOptions {
                    name: name.clone(),
                    driver: "local".to_string(),
                    labels: self.labels(service),
                    ..Default::default()
                })
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to create volume {name}: {error}"))
                })?;
        }
        Ok(())
    }

    async fn recreate_gitbucket(&self, publish_host_port: bool, network: &str) -> Result<()> {
        let prefix = resource_prefix(self.installation_id);
        self.recreate_container(
            gitbucket_service(self.config, &prefix, publish_host_port),
            None,
            network,
        )
        .await
    }

    async fn recreate_jenkins(&self, bootstrap_password: Option<&str>) -> Result<()> {
        let prefix = resource_prefix(self.installation_id);
        let mut environment = vec!["JAVA_OPTS=-Djenkins.install.runSetupWizard=false".to_string()];
        let command = bootstrap_password.map(|password| {
            environment.push(format!("XPRESSCLAW_JENKINS_PASSWORD={password}"));
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!(
                    "mkdir -p /usr/share/jenkins/ref/init.groovy.d && printf '%s' {} > /usr/share/jenkins/ref/init.groovy.d/xpressclaw.groovy && exec /usr/bin/tini -- /usr/local/bin/jenkins.sh",
                    shell_single_quote(JENKINS_BOOTSTRAP_GROOVY)
                ),
            ]
        });
        let network = network_name(self.installation_id);
        self.recreate_container(
            ServiceContainer {
                name: format!("{prefix}-jenkins"),
                alias: "jenkins".to_string(),
                image: self.config.jenkins_image.clone(),
                host_port: Some(self.config.jenkins_port),
                volume: format!("{prefix}-jenkins-data"),
                volume_target: "/var/jenkins_home".to_string(),
                memory: 2 * 1024 * 1024 * 1024,
                environment,
                health_command: "curl -fsS http://127.0.0.1:8080/login >/dev/null || exit 1"
                    .to_string(),
            },
            command,
            &network,
        )
        .await
    }

    async fn recreate_jenkins_agent(&self, secret: &str) -> Result<()> {
        let prefix = resource_prefix(self.installation_id);
        let name = format!("{prefix}-jenkins-agent");
        if let Some(existing) = self.docker.inspect_by_name(&name).await {
            let labels = existing
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            if !resource_owned(labels, self.installation_id) {
                return Err(Error::Container(format!(
                    "container {name} exists but belongs to another installation"
                )));
            }
            self.docker
                .client()
                .remove_container(
                    &name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|error| Error::Container(format!("failed to replace {name}: {error}")))?;
        }

        let network = network_name(self.installation_id);
        let config = ContainerConfig {
            image: Some(self.config.jenkins_image.clone()),
            entrypoint: Some(vec!["/bin/sh".to_string(), "-c".to_string()]),
            cmd: Some(vec![JENKINS_AGENT_COMMAND.to_string()]),
            env: Some(vec![format!("XPRESSCLAW_JENKINS_AGENT_SECRET={secret}")]),
            host_config: Some(HostConfig {
                memory: Some(1024 * 1024 * 1024),
                nano_cpus: Some(2_000_000_000),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                network_mode: Some(network.clone()),
                mounts: Some(Vec::new()),
                ..Default::default()
            }),
            labels: Some(self.labels("jenkins-agent")),
            networking_config: Some(NetworkingConfig {
                endpoints_config: HashMap::from([(
                    network,
                    EndpointSettings {
                        aliases: Some(vec!["jenkins-agent".to_string()]),
                        ..Default::default()
                    },
                )]),
            }),
            ..Default::default()
        };
        let created = self
            .docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| Error::Container(format!("failed to create {name}: {error}")))?;
        self.docker
            .client()
            .start_container::<String>(&created.id, None)
            .await
            .map_err(|error| Error::Container(format!("failed to start {name}: {error}")))?;
        Ok(())
    }

    /// Replace the build Agent's entire writable container layer and rotate
    /// its Jenkins node secret before an accepted build. No repository code or
    /// background process from the previous job can reach the next job.
    pub async fn prepare_jenkins_build_agent(&self, password: &str) -> Result<()> {
        let agent_secret = self.ensure_jenkins_agent_node(password).await?;
        self.recreate_jenkins_agent(&agent_secret).await?;
        self.wait_jenkins_agent_online(password).await
    }

    async fn recreate_container(
        &self,
        service: ServiceContainer,
        command: Option<Vec<String>>,
        network: &str,
    ) -> Result<()> {
        if let Some(existing) = self.docker.inspect_by_name(&service.name).await {
            let labels = existing
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            if !resource_owned(labels, self.installation_id) {
                return Err(Error::Container(format!(
                    "container {} exists but belongs to another installation",
                    service.name
                )));
            }
            if existing
                .state
                .as_ref()
                .is_some_and(|state| state.running == Some(true))
            {
                self.docker
                    .client()
                    .stop_container(&service.name, Some(StopContainerOptions { t: 20 }))
                    .await
                    .map_err(|error| {
                        Error::Container(format!(
                            "failed to stop {} before replacement: {error}",
                            service.name
                        ))
                    })?;
            }
            self.docker
                .client()
                .remove_container(
                    &service.name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to replace {}: {error}", service.name))
                })?;
        }

        let port = "8080/tcp".to_string();
        let port_bindings = service.host_port.map(|host_port| {
            HashMap::from([(
                port.clone(),
                Some(vec![PortBinding {
                    host_ip: Some(self.config.bind_address.clone()),
                    host_port: Some(host_port.to_string()),
                }]),
            )])
        });
        let exposed_ports = service
            .host_port
            .map(|_| HashMap::from([(port.clone(), HashMap::new())]));
        let config = ContainerConfig {
            image: Some(service.image),
            env: (!service.environment.is_empty()).then_some(service.environment),
            host_config: Some(HostConfig {
                memory: Some(service.memory),
                nano_cpus: Some(2_000_000_000),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                network_mode: Some(network.to_string()),
                port_bindings,
                mounts: Some(vec![Mount {
                    target: Some(service.volume_target),
                    source: Some(service.volume),
                    typ: Some(MountTypeEnum::VOLUME),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            exposed_ports,
            healthcheck: Some(HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), service.health_command]),
                interval: Some(15_000_000_000),
                timeout: Some(5_000_000_000),
                retries: Some(6),
                start_period: Some(60_000_000_000),
                start_interval: Some(5_000_000_000),
            }),
            labels: Some(self.labels(&service.name)),
            networking_config: Some(NetworkingConfig {
                endpoints_config: HashMap::from([(
                    network.to_string(),
                    EndpointSettings {
                        aliases: Some(vec![service.alias]),
                        ..Default::default()
                    },
                )]),
            }),
            cmd: command,
            ..Default::default()
        };
        let created = self
            .docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: service.name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| port_or_container_error(&service.name, service.host_port, error))?;
        self.docker
            .client()
            .start_container::<String>(&created.id, None)
            .await
            .map_err(|error| port_or_container_error(&service.name, service.host_port, error))?;
        Ok(())
    }

    async fn wait_container_healthy(&self, name: &str, service: &str) -> Result<()> {
        for _ in 0..90 {
            if let Some(container) = self.docker.inspect_by_name(name).await {
                let state = container.state.unwrap_or_default();
                let health = state
                    .health
                    .and_then(|health| health.status)
                    .map(|status| status.to_string())
                    .unwrap_or_default();
                if health == "healthy" {
                    return Ok(());
                }
                if health == "unhealthy" || state.running == Some(false) {
                    return Err(Error::Container(format!(
                        "{service} stopped or became unhealthy during private setup; inspect its Docker logs"
                    )));
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(Error::Container(format!(
            "{service} did not become healthy during private setup; inspect its Docker logs"
        )))
    }

    async fn wait_http(&self, url: &str, service: &str) -> Result<()> {
        let client = local_http_client_builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| Error::Container(format!("failed to build health client: {error}")))?;
        for _ in 0..90 {
            if client
                .get(url)
                .send()
                .await
                .is_ok_and(|response| response.status().as_u16() < 500)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(Error::Container(format!(
            "{service} did not become healthy at {url}; inspect its logs in Local collaboration settings"
        )))
    }

    async fn bootstrap_gitbucket_internal(
        &self,
        secrets: &CollaborationSecrets,
        network: &str,
    ) -> Result<String> {
        let helper = bootstrap_container_name(self.installation_id);
        if let Some(existing) = self.docker.inspect_by_name(&helper).await {
            let labels = existing
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            if !resource_owned(labels, self.installation_id) {
                return Err(Error::Container(format!(
                    "bootstrap container {helper} belongs to another installation"
                )));
            }
            self.docker
                .client()
                .remove_container(
                    &helper,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to clear stale bootstrap helper: {error}"))
                })?;
        }

        let config = ContainerConfig {
            image: Some(self.config.jenkins_image.clone()),
            entrypoint: Some(vec!["/bin/sh".to_string(), "-c".to_string()]),
            cmd: Some(vec![GITBUCKET_BOOTSTRAP_SCRIPT.to_string()]),
            env: Some(vec![
                format!(
                    "GITBUCKET_ROOT_PASSWORD={}",
                    secrets.gitbucket_root_password
                ),
                format!(
                    "GITBUCKET_SERVICE_PASSWORD={}",
                    secrets.gitbucket_service_password
                ),
                format!(
                    "GITBUCKET_TOKEN={}",
                    secrets.gitbucket_service_token.as_deref().unwrap_or("")
                ),
            ]),
            host_config: Some(HostConfig {
                memory: Some(256 * 1024 * 1024),
                nano_cpus: Some(1_000_000_000),
                network_mode: Some(network.to_string()),
                ..Default::default()
            }),
            labels: Some(self.labels("gitbucket-bootstrap")),
            networking_config: Some(NetworkingConfig {
                endpoints_config: HashMap::from([(
                    network.to_string(),
                    EndpointSettings {
                        aliases: Some(vec!["gitbucket-bootstrap".to_string()]),
                        ..Default::default()
                    },
                )]),
            }),
            ..Default::default()
        };
        let created = self
            .docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: helper.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| {
                Error::Container(format!(
                    "failed to create private GitBucket bootstrap: {error}"
                ))
            })?;
        let setup_result: Result<String> = async {
            self.docker
                .client()
                .start_container::<String>(&created.id, None)
                .await
                .map_err(|error| {
                    Error::Container(format!(
                        "failed to start private GitBucket bootstrap: {error}"
                    ))
                })?;
            let mut exit_code = None;
            for _ in 0..600 {
                let inspected = self
                    .docker
                    .client()
                    .inspect_container(&created.id, None)
                    .await
                    .map_err(|error| {
                        Error::Container(format!("failed to inspect GitBucket bootstrap: {error}"))
                    })?;
                let state = inspected.state.unwrap_or_default();
                if state.running == Some(false) {
                    exit_code = Some(state.exit_code.unwrap_or(-1));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let exit_code = exit_code.ok_or_else(|| {
                Error::Container("private GitBucket bootstrap timed out".to_string())
            })?;
            let mut logs = self.docker.client().logs::<String>(
                &created.id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: false,
                    tail: "all".to_string(),
                    ..Default::default()
                }),
            );
            let mut output = String::new();
            while let Some(chunk) = logs.next().await {
                let chunk = chunk.map_err(|error| {
                    Error::Container(format!(
                        "failed to read GitBucket bootstrap result: {error}"
                    ))
                })?;
                if let LogOutput::StdOut { message } = chunk {
                    if output.len() + message.len() > 1024 * 1024 {
                        return Err(Error::Container(
                            "GitBucket bootstrap returned an oversized response".to_string(),
                        ));
                    }
                    output.push_str(&String::from_utf8_lossy(&message));
                }
            }
            if exit_code != 0 {
                return Err(Error::Container(
                    "private GitBucket bootstrap failed; the insecure container was removed"
                        .to_string(),
                ));
            }
            extract_generated_token(&output).ok_or_else(|| {
                Error::Container("GitBucket did not return its generated service token".to_string())
            })
        }
        .await;
        let cleanup = self
            .docker
            .client()
            .remove_container(
                &created.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        match (setup_result, cleanup) {
            (Ok(token), Ok(())) => Ok(token),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(Error::Container(format!(
                "failed to remove private GitBucket bootstrap helper: {error}"
            ))),
        }
    }

    async fn run_jenkins_script(&self, password: &str, script: &str) -> Result<String> {
        let base = self.config.jenkins_url();
        let client = local_http_client_builder().build().map_err(|error| {
            Error::Container(format!("failed to build Jenkins HTTP client: {error}"))
        })?;
        let (crumb_field, crumb, session_cookie) = jenkins_crumb(&client, &base, password).await?;
        let mut request = client
            .post(format!("{base}/scriptText"))
            .basic_auth(JENKINS_USER, Some(password))
            .header(crumb_field, crumb)
            .form(&[("script", script)]);
        if let Some(cookie) = session_cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request.send().await.map_err(|error| {
            Error::Container(format!("failed to configure Jenkins build Agent: {error}"))
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            Error::Container(format!(
                "failed to read Jenkins build Agent configuration: {error}"
            ))
        })?;
        if !status.is_success() {
            return Err(Error::Container(format!(
                "Jenkins rejected build Agent configuration (HTTP {status}): {}",
                body.trim().chars().take(500).collect::<String>()
            )));
        }
        Ok(body)
    }

    async fn ensure_jenkins_agent_node(&self, password: &str) -> Result<String> {
        let secret = self
            .run_jenkins_script(password, JENKINS_AGENT_GROOVY)
            .await?
            .trim()
            .to_string();
        if secret.is_empty()
            || !secret
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(Error::Container(
                "Jenkins returned an invalid inbound build Agent secret".to_string(),
            ));
        }
        Ok(secret)
    }

    async fn wait_jenkins_agent_online(&self, password: &str) -> Result<()> {
        let client = local_http_client_builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| Error::Container(format!("failed to build health client: {error}")))?;
        let url = format!(
            "{}/computer/{JENKINS_AGENT}/api/json",
            self.config.jenkins_url()
        );
        for _ in 0..90 {
            let online = match client
                .get(&url)
                .basic_auth(JENKINS_USER, Some(password))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    response
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|body| body.get("offline").and_then(serde_json::Value::as_bool))
                        == Some(false)
                }
                _ => false,
            };
            if online {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(Error::Container(
            "the isolated Jenkins build Agent did not connect; inspect its Docker logs".to_string(),
        ))
    }

    async fn ensure_jenkins_job(&self, password: &str) -> Result<()> {
        let base = self.config.jenkins_url();
        let client = local_http_client_builder().build().map_err(|error| {
            Error::Container(format!("failed to build Jenkins HTTP client: {error}"))
        })?;
        let existing = client
            .get(format!("{base}/job/{JENKINS_JOB}/api/json"))
            .basic_auth(JENKINS_USER, Some(password))
            .send()
            .await
            .map_err(|error| Error::Container(format!("failed to inspect Jenkins job: {error}")))?;
        let exists = existing.status().is_success();
        if !exists && existing.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(Error::Container(format!(
                "Jenkins rejected managed job inspection (HTTP {})",
                existing.status()
            )));
        }
        let (crumb_field, crumb, session_cookie) = jenkins_crumb(&client, &base, password).await?;
        let url = if exists {
            format!("{base}/job/{JENKINS_JOB}/config.xml")
        } else {
            format!("{base}/createItem?name={JENKINS_JOB}")
        };
        let mut request = client
            .post(url)
            .basic_auth(JENKINS_USER, Some(password))
            .header(crumb_field, crumb)
            .header(reqwest::header::CONTENT_TYPE, "application/xml")
            .body(JENKINS_JOB_XML);
        if let Some(cookie) = session_cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Container(format!("failed to create Jenkins job: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::Container(format!(
                "Jenkins rejected the managed build job configuration (HTTP {})",
                response.status()
            )));
        }
        Ok(())
    }
}

struct ServiceContainer {
    name: String,
    alias: String,
    image: String,
    host_port: Option<u16>,
    volume: String,
    volume_target: String,
    memory: i64,
    environment: Vec<String>,
    health_command: String,
}

fn gitbucket_service(
    config: &CollaborationConfig,
    prefix: &str,
    publish_host_port: bool,
) -> ServiceContainer {
    ServiceContainer {
        name: format!("{prefix}-gitbucket"),
        alias: "gitbucket".to_string(),
        image: config.gitbucket_image.clone(),
        host_port: publish_host_port.then_some(config.gitbucket_port),
        volume: format!("{prefix}-gitbucket-data"),
        volume_target: "/gitbucket".to_string(),
        memory: 1024 * 1024 * 1024,
        environment: Vec::new(),
        health_command: "wget -q -O /dev/null http://127.0.0.1:8080/ || exit 1".to_string(),
    }
}

#[cfg(test)]
async fn sign_in(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
) -> Result<String> {
    let response = client
        .post(format!("{base}/signin"))
        .form(&[("userName", username), ("password", password), ("hash", "")])
        .send()
        .await
        .map_err(|error| Error::Container(format!("failed to sign in to GitBucket: {error}")))?;
    if !response.status().is_redirection() {
        return Err(Error::Container(format!(
            "GitBucket rejected a managed account during secure setup (HTTP {})",
            response.status()
        )));
    }
    if response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|location| location.contains("/signin"))
    {
        return Err(Error::Container(
            "GitBucket rejected a managed account during secure setup".to_string(),
        ));
    }
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    if cookies.is_empty() {
        return Err(Error::Container(
            "GitBucket did not establish a secure setup session".to_string(),
        ));
    }
    Ok(cookies)
}

fn extract_generated_token(html: &str) -> Option<String> {
    let id = html.find("id=\"generated-token\"")?;
    let before = &html[..id];
    let value_start = before.rfind("value=\"")? + "value=\"".len();
    let token = before[value_start..].split('"').next()?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

async fn jenkins_crumb(
    client: &reqwest::Client,
    base: &str,
    password: &str,
) -> Result<(String, String, Option<String>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Crumb {
        crumb_request_field: String,
        crumb: String,
    }
    let response = client
        .get(format!("{base}/crumbIssuer/api/json"))
        .basic_auth(JENKINS_USER, Some(password))
        .send()
        .await
        .map_err(|error| Error::Container(format!("failed to request Jenkins crumb: {error}")))?;
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>();
    let session_cookie = (!cookies.is_empty()).then(|| cookies.join("; "));
    let crumb = response
        .json::<Crumb>()
        .await
        .map_err(|error| Error::Container(format!("invalid Jenkins crumb response: {error}")))?;
    Ok((crumb.crumb_request_field, crumb.crumb, session_cookie))
}

fn port_or_container_error(name: &str, port: Option<u16>, error: bollard::errors::Error) -> Error {
    let detail = error.to_string();
    if port.is_some()
        && (detail.contains("port is already allocated")
            || detail.contains("address already in use"))
    {
        Error::Container(format!(
            "host port {} is already in use; choose another port for {name}",
            port.unwrap_or_default()
        ))
    } else {
        Error::Container(format!("failed to start {name}: {detail}"))
    }
}

fn resource_owned(labels: Option<&HashMap<String, String>>, installation_id: &str) -> bool {
    labels
        .and_then(|labels| labels.get(INSTALLATION_LABEL))
        .is_some_and(|installation| installation == installation_id)
}

fn bootstrap_container_name(installation_id: &str) -> String {
    format!("{}-gitbucket-bootstrap", resource_prefix(installation_id))
}

fn bootstrap_network_name(installation_id: &str) -> String {
    format!("{}-bootstrap-network", resource_prefix(installation_id))
}

fn reset_container_names(installation_id: &str) -> Vec<String> {
    let prefix = resource_prefix(installation_id);
    vec![
        bootstrap_container_name(installation_id),
        format!("{prefix}-jenkins-agent"),
        format!("{prefix}-jenkins"),
        format!("{prefix}-gitbucket"),
    ]
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
fn docker_integration_opted_in(value: Option<&str>) -> bool {
    value == Some("1")
}

const JENKINS_BOOTSTRAP_GROOVY: &str = r#"import jenkins.model.Jenkins
import hudson.security.FullControlOnceLoggedInAuthorizationStrategy
import hudson.security.HudsonPrivateSecurityRealm
def instance = Jenkins.get()
instance.setNumExecutors(0)
def realm = new HudsonPrivateSecurityRealm(false)
realm.createAccount('xpressclaw', System.getenv('XPRESSCLAW_JENKINS_PASSWORD'))
instance.setSecurityRealm(realm)
def authorization = new FullControlOnceLoggedInAuthorizationStrategy()
authorization.setAllowAnonymousRead(false)
instance.setAuthorizationStrategy(authorization)
instance.save()
new File('/var/jenkins_home/init.groovy.d/xpressclaw.groovy').delete()
"#;

const JENKINS_AGENT_GROOVY: &str = r#"import jenkins.model.Jenkins
import hudson.model.Node
import hudson.slaves.DumbSlave
import hudson.slaves.JNLPLauncher
import hudson.slaves.RetentionStrategy
def instance = Jenkins.get()
instance.setNumExecutors(0)
def existing = instance.getNode('xpressclaw-builder')
if (existing != null) {
  instance.removeNode(existing)
}
def agent = new DumbSlave('xpressclaw-builder', '/tmp/xpressclaw-jenkins-agent', new JNLPLauncher())
agent.setNumExecutors(1)
agent.setMode(Node.Mode.EXCLUSIVE)
agent.setLabelString('xpressclaw-isolated')
agent.setRetentionStrategy(new RetentionStrategy.Always())
instance.addNode(agent)
instance.save()
print instance.getComputer('xpressclaw-builder').getJnlpMac()
"#;

const JENKINS_AGENT_COMMAND: &str = r#"set -eu
work=/tmp/xpressclaw-jenkins-agent
mkdir -p "$work"
curl -fsS --connect-timeout 5 --max-time 60 http://jenkins:8080/jnlpJars/agent.jar -o "$work/agent.jar"
agent_secret="$XPRESSCLAW_JENKINS_AGENT_SECRET"
unset XPRESSCLAW_JENKINS_AGENT_SECRET
exec java -jar "$work/agent.jar" -url http://jenkins:8080/ -secret "$agent_secret" -name xpressclaw-builder -webSocket -workDir "$work"
"#;

const GITBUCKET_BOOTSTRAP_SCRIPT: &str = r#"set -eu
base=http://gitbucket:8080
cookies=/tmp/gitbucket-cookies
request() {
  curl --connect-timeout 5 --max-time 30 "$@"
}
signin() {
  rm -f "$cookies"
  headers=/tmp/gitbucket-signin-headers
  status="$(request -sS -D "$headers" -o /dev/null -c "$cookies" \
    --data-urlencode "userName=$1" --data-urlencode "password=$2" \
    --data-urlencode "hash=" -w '%{http_code}' "$base/signin")"
  test "$status" = 302
  ! grep -Eqi '^Location: .*\/signin([;?]|$)' "$headers"
}
if signin root root; then
  request -fsS -o /dev/null -b "$cookies" \
    --data-urlencode "password=$GITBUCKET_ROOT_PASSWORD" \
    --data-urlencode "fullName=XpressClaw Administrator" \
    --data-urlencode "mailAddress=root@localhost" \
    --data-urlencode "description=Managed by XpressClaw" \
    --data-urlencode "url=" --data-urlencode "clearImage=false" "$base/root/_edit"
fi
signin root "$GITBUCKET_ROOT_PASSWORD"
if test -n "${GITBUCKET_TOKEN:-}"; then
  attempt=0
  while test "$attempt" -lt 30; do
    if request -fsS -o /dev/null -H "Authorization: token $GITBUCKET_TOKEN" "$base/api/v3/user"; then
      printf '<input value="%s" id="generated-token">' "$GITBUCKET_TOKEN"
      exit 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
fi
create_status="$(request -sS -o /dev/null -b "$cookies" -w '%{http_code}' \
  -H 'Content-Type: application/json' \
  --data "{\"login\":\"xpressclaw-agent\",\"password\":\"$GITBUCKET_SERVICE_PASSWORD\",\"email\":\"agent@localhost\",\"fullName\":\"XpressClaw Agents\",\"isAdmin\":false}" \
  "$base/api/v3/admin/users")"
case "$create_status" in 2*|4*) ;; *) exit 1;; esac
signin xpressclaw-agent "$GITBUCKET_SERVICE_PASSWORD"
request -fsS -o /dev/null -b "$cookies" \
  --data-urlencode "note=XpressClaw local collaboration" \
  "$base/xpressclaw-agent/_personalToken"
request -fsS -b "$cookies" "$base/xpressclaw-agent/_application"
"#;

const JENKINS_JOB_XML: &str = r#"<?xml version="1.1" encoding="UTF-8"?>
<project>
  <actions/>
  <description>Managed by XpressClaw. Runs .xpressclaw/jenkins.sh from a public local GitBucket repository.</description>
  <keepDependencies>false</keepDependencies>
  <properties><hudson.model.ParametersDefinitionProperty><parameterDefinitions>
    <hudson.model.StringParameterDefinition><name>REPOSITORY_URL</name><defaultValue></defaultValue><trim>true</trim></hudson.model.StringParameterDefinition>
    <hudson.model.StringParameterDefinition><name>GIT_REF</name><defaultValue>main</defaultValue><trim>true</trim></hudson.model.StringParameterDefinition>
  </parameterDefinitions></hudson.model.ParametersDefinitionProperty></properties>
  <scm class="hudson.scm.NullSCM"/><assignedNode>xpressclaw-isolated</assignedNode><canRoam>false</canRoam><disabled>false</disabled>
  <blockBuildWhenDownstreamBuilding>false</blockBuildWhenDownstreamBuilding>
  <blockBuildWhenUpstreamBuilding>false</blockBuildWhenUpstreamBuilding>
  <triggers/><concurrentBuild>false</concurrentBuild>
  <builders><hudson.tasks.Shell><command>set -eu
case "$REPOSITORY_URL" in http://gitbucket:8080/xpressclaw-agent/*.git) ;; *) echo "Repository is outside the managed local forge account" >&amp;2; exit 2;; esac
repository_name=${REPOSITORY_URL#http://gitbucket:8080/xpressclaw-agent/}
repository_name=${repository_name%.git}
case "$repository_name" in ''|.|..|*[!A-Za-z0-9._-]*) echo "Repository is outside the managed local forge account" >&amp;2; exit 2;; esac
rm -rf source
git clone --depth 1 --branch "$GIT_REF" "$REPOSITORY_URL" source
cd source
test -f .xpressclaw/jenkins.sh || { echo "Missing .xpressclaw/jenkins.sh" >&amp;2; exit 2; }
/bin/sh .xpressclaw/jenkins.sh</command><configuredLocalRules/></hudson.tasks.Shell></builders>
  <publishers/><buildWrappers/>
</project>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::gitbucket::GitBucketProvider;
    use crate::collaboration::jenkins::JenkinsProvider;
    use crate::collaboration::{BuildProvider, BuildRequest, ForgeProvider};

    #[test]
    fn extracts_only_the_generated_token_field() {
        let html =
            r#"<input value="safe-token-123" class="form-control" id="generated-token" readonly>"#;
        assert_eq!(
            extract_generated_token(html).as_deref(),
            Some("safe-token-123")
        );
        assert_eq!(extract_generated_token("<input value=\"other\">"), None);
    }

    #[test]
    fn resource_names_are_installation_scoped() {
        assert_eq!(
            resource_prefix("12345678-90ab-cdef"),
            "xpressclaw-collaboration-1234567890ab"
        );
    }

    #[test]
    fn bootstrap_and_job_never_mount_the_container_engine() {
        assert!(JENKINS_BOOTSTRAP_GROOVY.contains("System.getenv"));
        assert!(GITBUCKET_BOOTSTRAP_SCRIPT.contains("http://gitbucket:8080"));
        assert!(!GITBUCKET_BOOTSTRAP_SCRIPT.contains("127.0.0.1"));
        assert!(GITBUCKET_BOOTSTRAP_SCRIPT.contains("Location: .*\\/signin"));
        assert!(
            GITBUCKET_BOOTSTRAP_SCRIPT
                .find("signin root \"$GITBUCKET_ROOT_PASSWORD\"")
                .unwrap()
                < GITBUCKET_BOOTSTRAP_SCRIPT
                    .find("if test -n \"${GITBUCKET_TOKEN:-}\"")
                    .unwrap(),
            "the generated administrator password must be verified before a saved token can short-circuit setup"
        );
        assert!(!JENKINS_JOB_XML.contains("docker.sock"));
        assert!(JENKINS_BOOTSTRAP_GROOVY.contains("setNumExecutors(0)"));
        assert!(JENKINS_AGENT_GROOVY.contains("Node.Mode.EXCLUSIVE"));
        assert!(JENKINS_JOB_XML.contains("<assignedNode>xpressclaw-isolated</assignedNode>"));
        assert!(JENKINS_JOB_XML.contains("<canRoam>false</canRoam>"));
        assert!(JENKINS_JOB_XML
            .contains("repository_name=${REPOSITORY_URL#http://gitbucket:8080/xpressclaw-agent/}"));
        assert!(JENKINS_JOB_XML.contains("''|.|..|*[!A-Za-z0-9._-]*"));
        assert!(JENKINS_AGENT_COMMAND.contains("-webSocket"));
        assert!(!JENKINS_AGENT_COMMAND.contains("/var/jenkins_home"));
    }

    #[test]
    fn gitbucket_is_unpublished_until_private_bootstrap_completes() {
        let config = CollaborationConfig::default();
        assert_eq!(gitbucket_service(&config, "test", false).host_port, None);
        assert_eq!(
            gitbucket_service(&config, "test", true).host_port,
            Some(config.gitbucket_port)
        );
    }

    #[test]
    fn gitbucket_bootstrap_resources_are_isolated_and_resettable() {
        let installation = "12345678-90ab-cdef";
        let bootstrap_container = bootstrap_container_name(installation);
        let bootstrap_network = bootstrap_network_name(installation);
        assert_ne!(bootstrap_network, network_name(installation));
        assert!(bootstrap_container.ends_with("-gitbucket-bootstrap"));
        assert!(bootstrap_network.ends_with("-bootstrap-network"));
        assert!(reset_container_names(installation).contains(&bootstrap_container));
    }

    #[test]
    fn docker_integration_test_requires_an_explicit_opt_in() {
        assert!(!docker_integration_opted_in(None));
        assert!(!docker_integration_opted_in(Some("true")));
        assert!(docker_integration_opted_in(Some("1")));
    }

    #[test]
    fn destructive_lifecycle_requires_the_installation_label() {
        let labels = HashMap::from([(
            INSTALLATION_LABEL.to_string(),
            "this-installation".to_string(),
        )]);
        assert!(resource_owned(Some(&labels), "this-installation"));
        assert!(!resource_owned(Some(&labels), "another-installation"));
        assert!(!resource_owned(None, "this-installation"));
    }

    #[test]
    fn port_conflicts_are_actionable() {
        let error = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "driver failed: port is already allocated".to_string(),
        };
        assert!(port_or_container_error("gitbucket", Some(8088), error)
            .to_string()
            .contains("host port 8088 is already in use"));
    }

    #[tokio::test]
    #[ignore = "opt-in Docker integration test; pulls GitBucket and Jenkins images"]
    async fn docker_failure_cleanup_preserves_an_unowned_name_collision() {
        let opt_in = std::env::var("XPRESSCLAW_DOCKER_INTEGRATION").ok();
        if !docker_integration_opted_in(opt_in.as_deref()) {
            eprintln!(
                "skipping Docker collaboration integration test; set XPRESSCLAW_DOCKER_INTEGRATION=1 to run it"
            );
            return;
        }

        let data = tempfile::tempdir().unwrap();
        let installation = uuid::Uuid::new_v4().to_string();
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let config = CollaborationConfig {
            enabled: true,
            ..Default::default()
        };
        let stack = CollaborationStack::new(&docker, &config, data.path(), &installation);
        docker.pull_image(&config.gitbucket_image).await.unwrap();
        let collision = stack.container_names()[0].clone();
        docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: collision.clone(),
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(config.gitbucket_image.clone()),
                    labels: Some(HashMap::from([(
                        INSTALLATION_LABEL.to_string(),
                        "another-installation".to_string(),
                    )])),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let collision_error = stack.install().await.unwrap_err().to_string();
        assert!(collision_error.contains("belongs to another installation"));
        assert!(docker.inspect_by_name(&collision).await.is_some());

        docker
            .client()
            .remove_container(
                &collision,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        stack.reset().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "opt-in Docker integration test; pulls GitBucket and Jenkins images"]
    async fn docker_stack_survives_restart_and_builds_a_fixture() {
        let opt_in = std::env::var("XPRESSCLAW_DOCKER_INTEGRATION").ok();
        if !docker_integration_opted_in(opt_in.as_deref()) {
            eprintln!(
                "skipping Docker collaboration integration test; set XPRESSCLAW_DOCKER_INTEGRATION=1 to run it"
            );
            return;
        }
        fn free_port() -> u16 {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        }
        fn git(directory: &Path, arguments: &[&str]) {
            let output = std::process::Command::new("git")
                .args(arguments)
                .current_dir(directory)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let data = tempfile::tempdir().unwrap();
        let installation = uuid::Uuid::new_v4().to_string();
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let config = CollaborationConfig {
            enabled: true,
            gitbucket_port: free_port(),
            jenkins_port: free_port(),
            ..Default::default()
        };
        let stack = CollaborationStack::new(&docker, &config, data.path(), &installation);
        stack.install().await.unwrap();
        stack.install().await.unwrap();

        let helper = bootstrap_container_name(&installation);
        assert!(docker.inspect_by_name(&helper).await.is_none());
        let bootstrap_network = bootstrap_network_name(&installation);
        assert!(docker
            .client()
            .inspect_network::<String>(&bootstrap_network, None)
            .await
            .is_err());
        let gitbucket = docker
            .inspect_by_name(&stack.container_names()[0])
            .await
            .unwrap();
        assert!(gitbucket
            .host_config
            .and_then(|host| host.port_bindings)
            .is_some_and(|bindings| bindings.contains_key("8080/tcp")));
        let jenkins_agent = docker
            .inspect_by_name(&stack.container_names()[2])
            .await
            .unwrap();
        assert!(jenkins_agent
            .host_config
            .and_then(|host| host.mounts)
            .is_none_or(|mounts| mounts.is_empty()));
        let client = local_http_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert!(sign_in(&client, &config.gitbucket_url(), "root", "root")
            .await
            .is_err());

        let secrets = CollaborationSecrets::load(data.path()).unwrap().unwrap();
        let forge = GitBucketProvider::new(
            &config.gitbucket_url(),
            SERVICE_USER,
            secrets.gitbucket_service_token.as_deref().unwrap(),
        )
        .unwrap();
        forge
            .create_repository("integration-fixture", false)
            .await
            .unwrap();

        let checkout = tempfile::tempdir().unwrap();
        git(checkout.path(), &["init", "-b", "main"]);
        git(checkout.path(), &["config", "user.name", "XpressClaw test"]);
        git(checkout.path(), &["config", "user.email", "test@localhost"]);
        std::fs::create_dir_all(checkout.path().join(".xpressclaw")).unwrap();
        std::fs::write(
            checkout.path().join(".xpressclaw/jenkins.sh"),
            "#!/bin/sh\nset -eu\necho xpressclaw-local-build-ok\necho node=$NODE_NAME\n",
        )
        .unwrap();
        git(checkout.path(), &["add", ".xpressclaw/jenkins.sh"]);
        git(checkout.path(), &["commit", "-m", "Add build fixture"]);
        let askpass = checkout.path().join("askpass.sh");
        std::fs::write(
            &askpass,
            "#!/bin/sh\ncase \"$1\" in *Username*) printf '%s' \"$GIT_USER\";; *) printf '%s' \"$GIT_TOKEN\";; esac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let remote = format!(
            "{}/{SERVICE_USER}/integration-fixture.git",
            config.gitbucket_url()
        );
        let output = std::process::Command::new("git")
            .args(["push", &remote, "main:main"])
            .current_dir(checkout.path())
            .env("GIT_ASKPASS", &askpass)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_USER", SERVICE_USER)
            .env(
                "GIT_TOKEN",
                secrets.gitbucket_service_token.as_deref().unwrap(),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "push failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        git(checkout.path(), &["checkout", "-b", "feature/local-build"]);
        std::fs::write(checkout.path().join("fixture.txt"), "local collaboration\n").unwrap();
        git(checkout.path(), &["add", "fixture.txt"]);
        git(checkout.path(), &["commit", "-m", "Exercise collaboration"]);
        let output = std::process::Command::new("git")
            .args(["push", &remote, "feature/local-build:feature/local-build"])
            .current_dir(checkout.path())
            .env("GIT_ASKPASS", &askpass)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_USER", SERVICE_USER)
            .env(
                "GIT_TOKEN",
                secrets.gitbucket_service_token.as_deref().unwrap(),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "feature push failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let issue = forge
            .create_issue(
                SERVICE_USER,
                "integration-fixture",
                "Verify local build",
                "Created by the opt-in integration test.",
            )
            .await
            .unwrap();
        assert_eq!(
            forge
                .get_issue(SERVICE_USER, "integration-fixture", issue.number)
                .await
                .unwrap()
                .title,
            "Verify local build"
        );
        let pull_request = forge
            .create_pull_request(
                SERVICE_USER,
                "integration-fixture",
                "Exercise local collaboration",
                "Repository, pull request, comment, and build fixture.",
                "feature/local-build",
                "main",
            )
            .await
            .unwrap();
        forge
            .comment_on_pull_request(
                SERVICE_USER,
                "integration-fixture",
                pull_request.number,
                "Build requested.",
            )
            .await
            .unwrap();
        assert_eq!(
            forge
                .get_pull_request(SERVICE_USER, "integration-fixture", pull_request.number,)
                .await
                .unwrap()
                .head,
            "feature/local-build"
        );

        stack.stop().await.unwrap();
        stack.start().await.unwrap();
        assert!(sign_in(&client, &config.gitbucket_url(), "root", "root")
            .await
            .is_err());
        let builds = JenkinsProvider::new(
            &config.jenkins_url(),
            JENKINS_USER,
            &secrets.jenkins_password,
        )
        .unwrap();
        let build = builds
            .trigger(&BuildRequest {
                repository: format!(
                    "{GITBUCKET_INTERNAL_URL}/{SERVICE_USER}/integration-fixture.git"
                ),
                git_ref: "feature/local-build".to_string(),
            })
            .await
            .unwrap();
        let mut completed = None;
        for _ in 0..120 {
            let current = builds.get(build.number).await.unwrap();
            if current.state != "running" && current.state != "queued" {
                completed = Some(current);
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert_eq!(completed.unwrap().state, "success");
        let logs = builds.logs(build.number, 100_000).await.unwrap();
        assert!(logs.contains("xpressclaw-local-build-ok"));
        assert!(logs.contains("node=xpressclaw-builder"));

        let completed_agent_id = docker
            .inspect_by_name(&stack.container_names()[2])
            .await
            .and_then(|container| container.id)
            .unwrap();
        builds.ensure_idle().await.unwrap();
        stack
            .prepare_jenkins_build_agent(&secrets.jenkins_password)
            .await
            .unwrap();
        let replacement_agent_id = docker
            .inspect_by_name(&stack.container_names()[2])
            .await
            .and_then(|container| container.id)
            .unwrap();
        assert_ne!(completed_agent_id, replacement_agent_id);

        // Simulate a crash after the credential-bearing bootstrap helper and
        // private network were created. Reset must remove both before it can
        // safely delete the collaboration data and final network.
        stack
            .ensure_network_named(&bootstrap_network, "gitbucket-bootstrap-network")
            .await
            .unwrap();
        docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: helper.clone(),
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(config.jenkins_image.clone()),
                    env: Some(vec!["GITBUCKET_ROOT_PASSWORD=must-be-removed".to_string()]),
                    host_config: Some(HostConfig {
                        network_mode: Some(bootstrap_network.clone()),
                        ..Default::default()
                    }),
                    labels: Some(stack.labels("gitbucket-bootstrap")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // A retained Agent may still be attached to the final network after
        // its access is revoked. Reset must detach it without deleting its
        // reusable project environment.
        let collaboration_network = network_name(&installation);
        let retained_agent = format!("xpressclaw-reset-agent-{}", uuid::Uuid::new_v4().simple());
        docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: retained_agent.clone(),
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(config.jenkins_image.clone()),
                    host_config: Some(HostConfig {
                        network_mode: Some(collaboration_network.clone()),
                        ..Default::default()
                    }),
                    labels: Some(HashMap::from([
                        (INSTALLATION_LABEL.to_string(), installation.to_string()),
                        ("io.xpressclaw.lifecycle".to_string(), "project".to_string()),
                        (
                            "io.xpressclaw.agent-id".to_string(),
                            "reset-fixture".to_string(),
                        ),
                    ])),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // An endpoint without this installation's ownership label must block
        // network removal before persistent volumes or credentials are lost.
        let unowned_endpoint =
            format!("xpressclaw-reset-blocker-{}", uuid::Uuid::new_v4().simple());
        docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name: unowned_endpoint.clone(),
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(config.jenkins_image.clone()),
                    host_config: Some(HostConfig {
                        network_mode: Some(collaboration_network.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let reset_error = stack.reset().await.unwrap_err().to_string();
        assert!(reset_error.contains("is not managed by this XpressClaw installation"));
        let prefix = resource_prefix(&installation);
        for volume in [
            format!("{prefix}-gitbucket-data"),
            format!("{prefix}-jenkins-data"),
        ] {
            assert!(docker.client().inspect_volume(&volume).await.is_ok());
        }
        assert!(CollaborationSecrets::path(data.path()).exists());
        docker
            .client()
            .remove_container(
                &unowned_endpoint,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();

        stack.reset().await.unwrap();
        for container in reset_container_names(&installation) {
            assert!(docker.inspect_by_name(&container).await.is_none());
        }
        for volume in [
            format!("{prefix}-gitbucket-data"),
            format!("{prefix}-jenkins-data"),
        ] {
            assert!(docker.client().inspect_volume(&volume).await.is_err());
        }
        assert!(docker
            .client()
            .inspect_network::<String>(&collaboration_network, None)
            .await
            .is_err());
        assert!(docker
            .client()
            .inspect_network::<String>(&bootstrap_network, None)
            .await
            .is_err());
        let retained = docker.inspect_by_name(&retained_agent).await.unwrap();
        assert!(retained
            .network_settings
            .and_then(|settings| settings.networks)
            .is_none_or(|networks| !networks.contains_key(&collaboration_network)));
        docker
            .client()
            .remove_container(
                &retained_agent,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
    }
}
