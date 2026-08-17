use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, NetworkingConfig, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::models::{
    EndpointSettings, HealthConfig, HostConfig, Mount, MountTypeEnum, PortBinding, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::network::CreateNetworkOptions;
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use serde::{Deserialize, Serialize};

use super::{
    network_name, resource_prefix, CollaborationConfig, CollaborationSecrets,
    GITBUCKET_INTERNAL_URL, JENKINS_INTERNAL_URL,
};
use crate::docker::manager::DockerManager;
use crate::error::{Error, Result};

pub const INSTALLATION_LABEL: &str = "io.xpressclaw.installation";
const SERVICE_USER: &str = "xpressclaw-agent";
const JENKINS_USER: &str = "xpressclaw";
const JENKINS_JOB: &str = "xpressclaw-local-build";

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
        self.ensure_network().await?;
        self.ensure_volumes().await?;

        self.recreate_gitbucket().await?;
        if let Err(error) = self
            .wait_http(&self.config.gitbucket_url(), "GitBucket")
            .await
        {
            self.stop_gitbucket_after_insecure_setup().await;
            return Err(error);
        }
        let gitbucket_ready = match secrets.gitbucket_service_token.as_deref() {
            Some(token) => self.gitbucket_token_valid(token).await,
            None => false,
        };
        if !gitbucket_ready {
            match self.bootstrap_gitbucket(&secrets).await {
                Ok(token) => {
                    secrets.gitbucket_service_token = Some(token);
                    secrets.save(self.data_dir)?;
                }
                Err(error) => {
                    self.stop_gitbucket_after_insecure_setup().await;
                    return Err(error);
                }
            }
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

    async fn stop_gitbucket_after_insecure_setup(&self) {
        let name = self.container_names()[0].clone();
        let _ = self
            .docker
            .client()
            .stop_container(&name, Some(StopContainerOptions { t: 5 }))
            .await;
    }

    async fn remove_jenkins_after_failed_setup(&self) {
        let name = self.container_names()[1].clone();
        let _ = self
            .docker
            .client()
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    async fn gitbucket_token_valid(&self, token: &str) -> bool {
        reqwest::Client::new()
            .get(format!("{}/api/v3/user", self.config.gitbucket_url()))
            .header(reqwest::header::AUTHORIZATION, format!("token {token}"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    async fn jenkins_auth_valid(&self, password: &str) -> bool {
        reqwest::Client::new()
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
        for name in self.container_names() {
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
        self.stop().await?;
        self.start().await
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
        for name in self.container_names() {
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
        let network = network_name(self.installation_id);
        if let Ok(existing) = self
            .docker
            .client()
            .inspect_network::<String>(&network, None)
            .await
        {
            if !resource_owned(existing.labels.as_ref(), self.installation_id) {
                return Err(Error::Container(format!(
                    "Docker network {network} exists but is not managed by this XpressClaw installation"
                )));
            }
            self.docker
                .client()
                .remove_network(&network)
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to remove network {network}: {error}"))
                })?;
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

    fn container_names(&self) -> [String; 2] {
        let prefix = resource_prefix(self.installation_id);
        [format!("{prefix}-gitbucket"), format!("{prefix}-jenkins")]
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
        if let Ok(existing) = self
            .docker
            .client()
            .inspect_network::<String>(&name, None)
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
                name: name.clone(),
                check_duplicate: true,
                driver: "bridge".to_string(),
                internal: false,
                attachable: false,
                ingress: false,
                labels: self.labels("network"),
                ..Default::default()
            })
            .await
            .map_err(|error| {
                Error::Container(format!("failed to create network {name}: {error}"))
            })?;
        Ok(())
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

    async fn recreate_gitbucket(&self) -> Result<()> {
        let prefix = resource_prefix(self.installation_id);
        self.recreate_container(
            ServiceContainer {
                name: format!("{prefix}-gitbucket"),
                alias: "gitbucket".to_string(),
                image: self.config.gitbucket_image.clone(),
                host_port: self.config.gitbucket_port,
                volume: format!("{prefix}-gitbucket-data"),
                volume_target: "/gitbucket".to_string(),
                memory: 1024 * 1024 * 1024,
                environment: Vec::new(),
                health_command: "wget -q -O /dev/null http://127.0.0.1:8080/ || exit 1".to_string(),
            },
            None,
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
        self.recreate_container(
            ServiceContainer {
                name: format!("{prefix}-jenkins"),
                alias: "jenkins".to_string(),
                image: self.config.jenkins_image.clone(),
                host_port: self.config.jenkins_port,
                volume: format!("{prefix}-jenkins-data"),
                volume_target: "/var/jenkins_home".to_string(),
                memory: 2 * 1024 * 1024 * 1024,
                environment,
                health_command: "curl -fsS http://127.0.0.1:8080/login >/dev/null || exit 1"
                    .to_string(),
            },
            command,
        )
        .await
    }

    async fn recreate_container(
        &self,
        service: ServiceContainer,
        command: Option<Vec<String>>,
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
        let network = network_name(self.installation_id);
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
                network_mode: Some(network.clone()),
                port_bindings: Some(HashMap::from([(
                    port.clone(),
                    Some(vec![PortBinding {
                        host_ip: Some(self.config.bind_address.clone()),
                        host_port: Some(service.host_port.to_string()),
                    }]),
                )])),
                mounts: Some(vec![Mount {
                    target: Some(service.volume_target),
                    source: Some(service.volume),
                    typ: Some(MountTypeEnum::VOLUME),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            exposed_ports: Some(HashMap::from([(port, HashMap::new())])),
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
                    network,
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

    async fn wait_http(&self, url: &str, service: &str) -> Result<()> {
        let client = reqwest::Client::builder()
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

    async fn bootstrap_gitbucket(&self, secrets: &CollaborationSecrets) -> Result<String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                Error::Container(format!("failed to build GitBucket client: {error}"))
            })?;
        let base = self.config.gitbucket_url();
        // A prior install may have changed the built-in password and then
        // stopped before persisting the service token. Accept either state so
        // a retry remains idempotent without ever restoring root/root.
        if let Ok(default_cookie) = sign_in(&client, &base, "root", "root").await {
            let changed = client
                .post(format!("{base}/root/_edit"))
                .header(reqwest::header::COOKIE, default_cookie)
                .form(&[
                    ("password", secrets.gitbucket_root_password.as_str()),
                    ("fullName", "XpressClaw Administrator"),
                    ("mailAddress", "root@localhost"),
                    ("description", "Managed by XpressClaw"),
                    ("url", ""),
                    ("clearImage", "false"),
                ])
                .send()
                .await
                .map_err(|error| {
                    Error::Container(format!("failed to secure GitBucket: {error}"))
                })?;
            if !changed.status().is_redirection() && !changed.status().is_success() {
                return Err(Error::Container(format!(
                    "GitBucket rejected its generated administrator password (HTTP {})",
                    changed.status()
                )));
            }
        }

        let root_cookie = sign_in(&client, &base, "root", &secrets.gitbucket_root_password).await?;
        let created = client
            .post(format!("{base}/api/v3/admin/users"))
            .header(reqwest::header::COOKIE, root_cookie)
            .json(&serde_json::json!({
                "login": SERVICE_USER,
                "password": secrets.gitbucket_service_password,
                "email": "agent@localhost",
                "fullName": "XpressClaw Agents",
                "isAdmin": false,
            }))
            .send()
            .await
            .map_err(|error| {
                Error::Container(format!("failed to create GitBucket service user: {error}"))
            })?;
        let created_status = created.status();
        let service_cookie = sign_in(
            &client,
            &base,
            SERVICE_USER,
            &secrets.gitbucket_service_password,
        )
        .await
        .map_err(|error| {
            if created_status.is_success() {
                error
            } else {
                Error::Container(format!(
                    "GitBucket could not create or reuse its non-admin service user (HTTP {created_status}): {error}"
                ))
            }
        })?;
        let generated = client
            .post(format!("{base}/{SERVICE_USER}/_personalToken"))
            .header(reqwest::header::COOKIE, &service_cookie)
            .form(&[("note", "XpressClaw local collaboration")])
            .send()
            .await
            .map_err(|error| {
                Error::Container(format!("failed to create GitBucket token: {error}"))
            })?;
        if !generated.status().is_redirection() && !generated.status().is_success() {
            return Err(Error::Container(format!(
                "GitBucket rejected its managed service token (HTTP {})",
                generated.status()
            )));
        }
        let application = client
            .get(format!("{base}/{SERVICE_USER}/_application"))
            .header(reqwest::header::COOKIE, service_cookie)
            .send()
            .await
            .map_err(|error| Error::Container(format!("failed to read GitBucket token: {error}")))?
            .text()
            .await
            .map_err(|error| {
                Error::Container(format!("failed to read GitBucket token: {error}"))
            })?;
        extract_generated_token(&application).ok_or_else(|| {
            Error::Container("GitBucket did not return its generated service token".to_string())
        })
    }

    async fn ensure_jenkins_job(&self, password: &str) -> Result<()> {
        let base = self.config.jenkins_url();
        let client = reqwest::Client::new();
        let existing = client
            .get(format!("{base}/job/{JENKINS_JOB}/api/json"))
            .basic_auth(JENKINS_USER, Some(password))
            .send()
            .await
            .map_err(|error| Error::Container(format!("failed to inspect Jenkins job: {error}")))?;
        if existing.status().is_success() {
            return Ok(());
        }
        let (crumb_field, crumb) = jenkins_crumb(&client, &base, password).await?;
        let response = client
            .post(format!("{base}/createItem?name={JENKINS_JOB}"))
            .basic_auth(JENKINS_USER, Some(password))
            .header(crumb_field, crumb)
            .header(reqwest::header::CONTENT_TYPE, "application/xml")
            .body(JENKINS_JOB_XML)
            .send()
            .await
            .map_err(|error| Error::Container(format!("failed to create Jenkins job: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::Container(format!(
                "Jenkins rejected the managed build job (HTTP {})",
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
    host_port: u16,
    volume: String,
    volume_target: String,
    memory: i64,
    environment: Vec<String>,
    health_command: String,
}

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
) -> Result<(String, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Crumb {
        crumb_request_field: String,
        crumb: String,
    }
    let crumb = client
        .get(format!("{base}/crumbIssuer/api/json"))
        .basic_auth(JENKINS_USER, Some(password))
        .send()
        .await
        .map_err(|error| Error::Container(format!("failed to request Jenkins crumb: {error}")))?
        .json::<Crumb>()
        .await
        .map_err(|error| Error::Container(format!("invalid Jenkins crumb response: {error}")))?;
    Ok((crumb.crumb_request_field, crumb.crumb))
}

fn port_or_container_error(name: &str, port: u16, error: bollard::errors::Error) -> Error {
    let detail = error.to_string();
    if detail.contains("port is already allocated") || detail.contains("address already in use") {
        Error::Container(format!(
            "host port {port} is already in use; choose another port for {name}"
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

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

const JENKINS_BOOTSTRAP_GROOVY: &str = r#"import jenkins.model.Jenkins
import hudson.security.FullControlOnceLoggedInAuthorizationStrategy
import hudson.security.HudsonPrivateSecurityRealm
def instance = Jenkins.get()
def realm = new HudsonPrivateSecurityRealm(false)
realm.createAccount('xpressclaw', System.getenv('XPRESSCLAW_JENKINS_PASSWORD'))
instance.setSecurityRealm(realm)
def authorization = new FullControlOnceLoggedInAuthorizationStrategy()
authorization.setAllowAnonymousRead(false)
instance.setAuthorizationStrategy(authorization)
instance.save()
new File('/var/jenkins_home/init.groovy.d/xpressclaw.groovy').delete()
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
  <scm class="hudson.scm.NullSCM"/><canRoam>true</canRoam><disabled>false</disabled>
  <blockBuildWhenDownstreamBuilding>false</blockBuildWhenDownstreamBuilding>
  <blockBuildWhenUpstreamBuilding>false</blockBuildWhenUpstreamBuilding>
  <triggers/><concurrentBuild>false</concurrentBuild>
  <builders><hudson.tasks.Shell><command>set -eu
case "$REPOSITORY_URL" in http://gitbucket:8080/xpressclaw-agent/*.git) ;; *) echo "Repository is outside the managed local forge account" >&amp;2; exit 2;; esac
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
        assert!(!JENKINS_JOB_XML.contains("docker.sock"));
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
        assert!(port_or_container_error("gitbucket", 8088, error)
            .to_string()
            .contains("host port 8088 is already in use"));
    }

    #[tokio::test]
    #[ignore = "opt-in Docker integration test; pulls GitBucket and Jenkins images"]
    async fn docker_stack_survives_restart_and_builds_a_fixture() {
        assert_eq!(
            std::env::var("XPRESSCLAW_DOCKER_INTEGRATION").as_deref(),
            Ok("1"),
            "set XPRESSCLAW_DOCKER_INTEGRATION=1 before running ignored tests"
        );
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
            "#!/bin/sh\nset -eu\necho xpressclaw-local-build-ok\n",
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
        let builds = JenkinsProvider::new(
            &config.jenkins_url(),
            JENKINS_USER,
            &secrets.jenkins_password,
        );
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
        assert!(builds
            .logs(build.number, 100_000)
            .await
            .unwrap()
            .contains("xpressclaw-local-build-ok"));
        stack.reset().await.unwrap();
    }
}
