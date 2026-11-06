use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, LogsOptions, NetworkingConfig,
    RemoveContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{EndpointSettings, HostConfig, Mount, MountTypeEnum};
use bollard::network::CreateNetworkOptions;
use bollard::volume::CreateVolumeOptions;
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::ProxyConfig;
use crate::config::{AgentConfig, ContainerEngineAccess};
use crate::docker::manager::{ContainerSpec, DockerManager, SelinuxRelabel, VolumeMount};
use crate::error::{Error, Result};

// Multi-architecture upstream release, shared by Docker and Podman installations.
pub const IMAGE: &str = "docker.io/mitmproxy/mitmproxy:12.2.1@sha256:743b6cdc817211d64bc269f5defacca8d14e76e647fc474e5c7244dbcb645141";
const OWNER: &str = "io.xpressclaw.installation";
const REVISION: &str = "io.xpressclaw.proxy-revision";
const GATEWAY: &str = include_str!("gateway.py");
const CA_PATH: &str = "/opt/xpressclaw/proxy/ca.pem";
const BUNDLE: &str = "/tmp/xpressclaw-proxy-ca.pem";
const PROXY_URL: &str = "http://xpressclaw-proxy:8080";
static LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Container(format!("Proxy: {error}"))
}

fn prefix(installation: &str) -> String {
    format!(
        "xpressclaw-proxy-{}",
        &format!("{:x}", Sha256::digest(installation.as_bytes()))[..16]
    )
}

pub fn network_name(installation: &str) -> String {
    format!("{}-runners", prefix(installation))
}

fn owned(labels: Option<&HashMap<String, String>>, installation: &str) -> Result<()> {
    if labels
        .and_then(|labels| labels.get(OWNER))
        .map(String::as_str)
        != Some(installation)
    {
        return Err(failure("resource belongs to another installation"));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("missing parent directory"))?;
    std::fs::create_dir_all(parent).map_err(failure)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
    use std::io::Write;
    file.write_all(bytes).map_err(failure)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(failure)?;
    }
    file.persist(path).map_err(failure)?;
    Ok(())
}

pub fn write_policy(data_dir: &Path, config: &ProxyConfig) -> Result<()> {
    config.validate().map_err(Error::ConfigValidation)?;
    let directory = data_dir.join("proxy/policy");
    atomic_write(
        &directory.join("policy.json"),
        &serde_json::to_vec(config).map_err(failure)?,
    )?;
    atomic_write(&directory.join("gateway.py"), GATEWAY.as_bytes())
}

pub struct ProxyService<'a> {
    pub docker: &'a DockerManager,
    pub data_dir: &'a Path,
    pub installation: &'a str,
}

#[derive(Debug, Serialize)]
pub struct ProxyStatus {
    pub state: String,
    pub image: &'static str,
    pub events: Vec<serde_json::Value>,
}

impl ProxyStatus {
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable".into(),
            image: IMAGE,
            events: vec![],
        }
    }
}

impl ProxyService<'_> {
    pub async fn status(&self) -> Result<ProxyStatus> {
        let name = prefix(self.installation);
        let inspected = match self.docker.client().inspect_container(&name, None).await {
            Ok(inspected) => inspected,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                return Ok(ProxyStatus {
                    state: "not_started".into(),
                    image: IMAGE,
                    events: vec![],
                });
            }
            Err(error) => return Err(failure(error)),
        };
        owned(
            inspected
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref()),
            self.installation,
        )?;
        let running = inspected
            .state
            .and_then(|state| state.running)
            .unwrap_or(false);
        let mut events = Vec::new();
        let mut logs = self.docker.client().logs(
            &name,
            Some(LogsOptions::<String> {
                stdout: true,
                stderr: false,
                tail: "200".into(),
                ..Default::default()
            }),
        );
        while let Some(Ok(chunk)) = logs.next().await {
            for line in chunk.to_string().lines() {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                    if value["proxy_event"] == true {
                        events.push(value);
                    }
                }
            }
        }
        events.reverse();
        events.truncate(100);
        Ok(ProxyStatus {
            state: if running { "running" } else { "stopped" }.into(),
            image: IMAGE,
            events,
        })
    }

    pub async fn ensure(&self, config: &ProxyConfig) -> Result<PathBuf> {
        let _guard = LIFECYCLE.lock().await;
        self.ensure_locked(config).await
    }

    /// Validate with the actual gateway before atomically publishing new rules.
    pub async fn apply(&self, config: &ProxyConfig) -> Result<()> {
        config.validate().map_err(Error::ConfigValidation)?;
        let _guard = LIFECYCLE.lock().await;
        if config.enabled {
            self.ensure_locked(&ProxyConfig::default()).await?;
            self.exec(vec![
                "python".into(),
                "-c".into(),
                GATEWAY.into(),
                "--validate".into(),
                serde_json::to_string(config).map_err(failure)?,
            ])
            .await
            .map_err(|_| {
                Error::ConfigValidation(
                    "A header rule contains a regular expression unsupported by the proxy".into(),
                )
            })?;
        }
        write_policy(self.data_dir, config)
    }

    async fn network(&self, name: &str, internal: bool) -> Result<()> {
        match self
            .docker
            .client()
            .inspect_network::<String>(name, None)
            .await
        {
            Ok(network) => {
                owned(network.labels.as_ref(), self.installation)?;
                if network.internal != Some(internal) || network.driver.as_deref() != Some("bridge")
                {
                    return Err(failure(format!(
                        "network {name} has an unexpected configuration"
                    )));
                }
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.docker
                    .client()
                    .create_network(CreateNetworkOptions {
                        name: name.to_string(),
                        driver: "bridge".into(),
                        internal,
                        enable_ipv6: false,
                        labels: HashMap::from([(OWNER.into(), self.installation.into())]),
                        ..Default::default()
                    })
                    .await
                    .map_err(failure)?;
            }
            Err(error) => return Err(failure(error)),
        }
        Ok(())
    }

    async fn ensure_locked(&self, config: &ProxyConfig) -> Result<PathBuf> {
        let name = prefix(self.installation);
        let network = network_name(self.installation);
        let outside = format!("{name}-outside");
        let volume = format!("{name}-ca");
        let revision = format!(
            "{:x}",
            Sha256::digest(format!("{IMAGE}\0{GATEWAY}\0{}", self.data_dir.display()).as_bytes())
        );
        // Task snapshots never overwrite a newer policy saved by Settings.
        if !self.data_dir.join("proxy/policy/policy.json").exists() {
            write_policy(self.data_dir, config)?;
        }
        self.network(&network, true).await?;
        self.network(&outside, false).await?;
        match self.docker.client().inspect_volume(&volume).await {
            Ok(existing) => owned(Some(&existing.labels), self.installation)?,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.docker
                    .client()
                    .create_volume(CreateVolumeOptions {
                        name: volume.clone(),
                        labels: HashMap::from([(OWNER.to_string(), self.installation.to_string())]),
                        ..Default::default()
                    })
                    .await
                    .map_err(failure)?;
            }
            Err(error) => return Err(failure(error)),
        }
        let mut existing = match self.docker.client().inspect_container(&name, None).await {
            Ok(container) => Some(container),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => None,
            Err(error) => return Err(failure(error)),
        };
        if let Some(container) = &existing {
            let labels = container
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            owned(labels, self.installation)?;
            if labels.and_then(|labels| labels.get(REVISION)) != Some(&revision) {
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
                    .map_err(failure)?;
                existing = None;
            }
        }
        if existing.is_none() {
            if self.docker.client().inspect_image(IMAGE).await.is_err() {
                self.docker.pull_image(IMAGE).await?;
            }
            atomic_write(
                &self.data_dir.join("proxy/policy/gateway.py"),
                GATEWAY.as_bytes(),
            )?;
            let policy_mount = self.data_dir.join("proxy/policy").display().to_string();
            self.docker
                .client()
                .create_container(
                    Some(CreateContainerOptions {
                        name: name.clone(),
                        platform: None,
                    }),
                    ContainerConfig {
                        image: Some(IMAGE.to_string()),
                        user: Some("0".into()),
                        entrypoint: Some(vec!["python".into(), "/policy/gateway.py".into()]),
                        env: Some(vec!["PYTHONDONTWRITEBYTECODE=1".into()]),
                        host_config: Some(HostConfig {
                            network_mode: Some(outside.clone()),
                            extra_hosts: (self.docker.runtime() == "docker")
                                .then(|| vec!["host.docker.internal:host-gateway".into()]),
                            binds: Some(vec![format!("{policy_mount}:/policy:ro,z")]),
                            mounts: Some(vec![Mount {
                                source: Some(volume),
                                target: Some("/ca".into()),
                                typ: Some(MountTypeEnum::VOLUME),
                                ..Default::default()
                            }]),
                            cap_drop: Some(vec!["ALL".into()]),
                            security_opt: Some(vec!["no-new-privileges:true".into()]),
                            memory: Some(512 * 1024 * 1024),
                            ..Default::default()
                        }),
                        networking_config: Some(NetworkingConfig {
                            endpoints_config: HashMap::from([
                                (outside, EndpointSettings::default()),
                                (
                                    network,
                                    EndpointSettings {
                                        aliases: Some(vec!["xpressclaw-proxy".into()]),
                                        ..Default::default()
                                    },
                                ),
                            ]),
                        }),
                        labels: Some(HashMap::from([
                            (OWNER.into(), self.installation.into()),
                            (REVISION.into(), revision),
                            ("io.xpressclaw.lifecycle".into(), "service".into()),
                        ])),
                        ..Default::default()
                    },
                )
                .await
                .map_err(failure)?;
        }
        if existing
            .as_ref()
            .and_then(|container| container.state.as_ref())
            .is_none_or(|state| state.running != Some(true))
        {
            self.docker
                .client()
                .start_container::<String>(&name, None)
                .await
                .map_err(failure)?;
        }
        // Read only the public certificate after the gateway has bound its listener.
        let certificate = self.exec(vec!["python".into(), "-c".into(),
            "import pathlib,socket,time\nfor attempt in range(100):\n try:\n  socket.create_connection(('127.0.0.1',8080),timeout=1).close()\n  print(pathlib.Path('/ca/mitmproxy-ca-cert.pem').read_text(),end='')\n  break\n except (OSError,FileNotFoundError):\n  time.sleep(.1)\nelse:\n raise SystemExit(1)".into()
        ]).await?;
        if !certificate.starts_with("-----BEGIN CERTIFICATE-----")
            || certificate.contains("PRIVATE KEY")
        {
            return Err(failure("invalid public CA certificate"));
        }
        let certificate_path = self.data_dir.join("proxy/public/ca.pem");
        if std::fs::read_to_string(&certificate_path).ok().as_deref() != Some(certificate.as_str())
        {
            atomic_write(&certificate_path, certificate.as_bytes())?;
        }
        Ok(certificate_path)
    }

    async fn exec(&self, command: Vec<String>) -> Result<String> {
        tokio::time::timeout(Duration::from_secs(20), async {
            let id = self
                .docker
                .client()
                .create_exec(
                    &prefix(self.installation),
                    CreateExecOptions {
                        cmd: Some(command),
                        attach_stdout: Some(true),
                        attach_stderr: Some(false),
                        ..Default::default()
                    },
                )
                .await
                .map_err(failure)?
                .id;
            let mut text = String::new();
            if let StartExecResults::Attached { mut output, .. } = self
                .docker
                .client()
                .start_exec(&id, None)
                .await
                .map_err(failure)?
            {
                while let Some(chunk) = output.next().await {
                    text.push_str(&chunk.map_err(failure)?.to_string());
                }
            }
            if self
                .docker
                .client()
                .inspect_exec(&id)
                .await
                .map_err(failure)?
                .exit_code
                != Some(0)
            {
                return Err(failure("gateway validation or readiness check failed"));
            }
            Ok(text)
        })
        .await
        .map_err(|_| failure("gateway readiness timed out"))?
    }
}

pub async fn configure_runner(
    docker: &DockerManager,
    data_dir: &Path,
    installation: &str,
    config: &ProxyConfig,
    agent: &AgentConfig,
    spec: &mut ContainerSpec,
    callback_port: u16,
) -> Result<()> {
    if !config.applies_to(agent.runner.proxy) {
        return Ok(());
    }
    if agent.runner.container_engine == ContainerEngineAccess::Host {
        return Err(failure("host container-engine access requires this Agent's networking to be set to Connect directly, or host-engine access to be disabled"));
    }
    let host = if docker.runtime() == "podman" {
        "host.containers.internal"
    } else {
        "host.docker.internal"
    };
    atomic_write(
        &data_dir.join("proxy/policy/callback.json"),
        &serde_json::to_vec(&serde_json::json!({"host":host, "port":callback_port}))
            .map_err(failure)?,
    )?;
    let certificate = ProxyService {
        docker,
        data_dir,
        installation,
    }
    .ensure(config)
    .await?;
    apply_runner(spec, installation, certificate);
    Ok(())
}

fn apply_runner(spec: &mut ContainerSpec, installation: &str, certificate: PathBuf) {
    spec.network_mode = Some(network_name(installation));
    spec.volumes.push(VolumeMount {
        source: certificate.display().to_string(),
        target: CA_PATH.into(),
        read_only: true,
        selinux_relabel: SelinuxRelabel::Shared,
    });
    let values = [
        ("HTTP_PROXY", PROXY_URL),
        ("http_proxy", PROXY_URL),
        ("HTTPS_PROXY", PROXY_URL),
        ("https_proxy", PROXY_URL),
        ("ALL_PROXY", PROXY_URL),
        ("all_proxy", PROXY_URL),
        ("NO_PROXY", "localhost,127.0.0.1,::1,xpressclaw-proxy"),
        ("no_proxy", "localhost,127.0.0.1,::1,xpressclaw-proxy"),
        ("NODE_EXTRA_CA_CERTS", CA_PATH),
        ("NODE_USE_ENV_PROXY", "1"),
        ("SSL_CERT_FILE", BUNDLE),
        ("REQUESTS_CA_BUNDLE", BUNDLE),
        ("CURL_CA_BUNDLE", BUNDLE),
        ("GIT_SSL_CAINFO", BUNDLE),
    ];
    spec.environment.retain(|value| {
        !values
            .iter()
            .any(|(name, _)| value.starts_with(&format!("{name}=")))
    });
    spec.environment
        .extend(values.map(|(name, value)| format!("{name}={value}")));
    let command = spec.cmd.take().unwrap_or_default();
    let script = format!("set -eu\numask 022\n: > {BUNDLE}\nfor source in /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt; do\n if [ -f \"$source\" ]; then cat \"$source\" >> {BUNDLE}; break; fi\ndone\ncat {CA_PATH} >> {BUNDLE}\nexec \"$@\"");
    spec.cmd = Some(
        [
            vec![
                "sh".into(),
                "-c".into(),
                script,
                "xpressclaw-proxy-trust".into(),
            ],
            command,
        ]
        .concat(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runner_configuration_is_shared_and_overrides_proxy_escape_variables() {
        let mut spec = ContainerSpec {
            cmd: Some(vec!["harness".into(), "--stdio".into()]),
            environment: vec![
                "HTTPS_PROXY=http://elsewhere".into(),
                "NO_PROXY=*".into(),
                "OTHER=value".into(),
            ],
            ..Default::default()
        };
        apply_runner(&mut spec, "instance", PathBuf::from("/data/public.pem"));
        assert_eq!(
            spec.network_mode.as_deref(),
            Some(network_name("instance").as_str())
        );
        assert!(!spec.environment.contains(&"NO_PROXY=*".into()));
        assert!(spec.environment.contains(&"OTHER=value".into()));
        assert_eq!(
            spec.environment
                .iter()
                .filter(|value| value.starts_with("HTTPS_PROXY="))
                .count(),
            1
        );
        assert!(spec.volumes[0].read_only);
        assert_eq!(&spec.cmd.as_ref().unwrap()[4..], ["harness", "--stdio"]);
    }
    #[test]
    fn network_names_isolate_installations() {
        assert_ne!(network_name("one"), network_name("two"));
        assert!(network_name("日本語").is_ascii());
        assert!(!ProxyConfig::default().applies_to(super::super::AgentProxyMode::Direct));
    }

    #[tokio::test]
    #[ignore = "requires Docker/Podman and XPRESSCLAW_PROXY_TEST_ROOT shared with its daemon"]
    async fn isolated_gateway_roundtrip() {
        use bollard::models::PortBinding;
        use bollard::volume::RemoveVolumeOptions;
        use futures_util::FutureExt;
        let Ok(root) = std::env::var("XPRESSCLAW_PROXY_TEST_ROOT") else {
            return;
        };
        std::fs::create_dir_all(&root).unwrap();
        let directory = tempfile::tempdir_in(root).unwrap();
        let installation = format!("proxy-test-{}", uuid::Uuid::new_v4());
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let service = ProxyService {
            docker: &docker,
            data_dir: directory.path(),
            installation: &installation,
        };
        let name = prefix(&installation);
        let outside = format!("{name}-outside");
        let origin = format!("{name}-origin");
        let guest = format!("{name}-guest");
        let result = std::panic::AssertUnwindSafe(async {
            let certificate = service.ensure(&ProxyConfig::default()).await.unwrap();
            let original_ca = std::fs::read(&certificate).unwrap();
            assert!(!String::from_utf8_lossy(&original_ca).contains("PRIVATE KEY"));
            docker.client().create_container(Some(CreateContainerOptions { name: origin.clone(), platform: None }), ContainerConfig {
                image: Some(IMAGE.to_string()), entrypoint: Some(vec!["python".into()]), cmd: Some(vec!["-m".into(), "http.server".into(), "8000".into()]),
                exposed_ports: Some(HashMap::from([("8000/tcp".into(), HashMap::new())])),
                host_config: Some(HostConfig { network_mode: Some(outside.clone()), port_bindings: Some(HashMap::from([("8000/tcp".into(), Some(vec![PortBinding { host_ip: Some("0.0.0.0".into()), host_port: Some("0".into()) }]))])), ..Default::default() }),
                networking_config: Some(NetworkingConfig { endpoints_config: HashMap::from([(outside.clone(), EndpointSettings { aliases: Some(vec!["proxy-origin".into()]), ..Default::default() })]) }),
                ..Default::default()
            }).await.unwrap();
            docker.client().start_container::<String>(&origin, None).await.unwrap();
            let inspected = docker.client().inspect_container(&origin, None).await.unwrap();
            let network = inspected.network_settings.unwrap();
            let origin_ip = network.networks.unwrap()[&outside].ip_address.clone().unwrap();
            let host_port = network.ports.unwrap()["8000/tcp"].as_ref().unwrap()[0].host_port.as_ref().unwrap().parse::<u16>().unwrap();
            let host = if docker.runtime() == "podman" { "host.containers.internal" } else { "host.docker.internal" };
            atomic_write(&directory.path().join("proxy/policy/callback.json"), &serde_json::to_vec(&serde_json::json!({"host":host,"port":host_port})).unwrap()).unwrap();
            // The test runner is on the same isolated network as real Agents.
            docker.client().create_container(Some(CreateContainerOptions { name: guest.clone(), platform: None }), ContainerConfig {
                image: Some(IMAGE.to_string()), entrypoint: Some(vec!["sleep".into()]), cmd: Some(vec!["infinity".into()]),
                host_config: Some(HostConfig { network_mode: Some(network_name(&installation)), ..Default::default() }), ..Default::default()
            }).await.unwrap();
            docker.client().start_container::<String>(&guest, None).await.unwrap();
            let run_guest = |script: String| {
                let guest = &guest;
                let docker = &docker;
                async move {
                    let id = docker.client().create_exec(guest, CreateExecOptions { cmd: Some(vec!["python".into(), "-c".into(), script]), attach_stdout: Some(true), attach_stderr: Some(true), ..Default::default() }).await.unwrap().id;
                    let mut text = String::new();
                    if let StartExecResults::Attached { mut output, .. } = docker.client().start_exec(&id, None).await.unwrap() { while let Some(chunk) = output.next().await { text.push_str(&chunk.unwrap().to_string()); } }
                    let status = docker.client().inspect_exec(&id).await.unwrap().exit_code;
                    assert_eq!(status, Some(0), "{text}");
                    text
                }
            };
            run_guest(format!("import http.client,socket,time\ntime.sleep(.5)\nc=http.client.HTTPConnection('xpressclaw-proxy',8080,timeout=5)\nc.request('GET','http://proxy-origin:8000/')\nassert c.getresponse().status==200\nc.close()\nc=http.client.HTTPConnection('xpressclaw-proxy',9080,timeout=5)\nc.request('GET','/')\nassert c.getresponse().status==200\nc.close()\ntry:\n socket.create_connection(('{origin_ip}',8000),timeout=1)\nexcept OSError:\n pass\nelse:\n raise AssertionError('direct egress succeeded')")).await;
            let config = ProxyConfig { blocked_domains: vec!["proxy-origin".into()], ..Default::default() };
            service.apply(&config).await.unwrap();
            run_guest("import http.client\nc=http.client.HTTPConnection('xpressclaw-proxy',8080,timeout=5)\nc.request('GET','http://proxy-origin:8000/')\nassert c.getresponse().status==403".into()).await;
            assert!(service.status().await.unwrap().events.iter().any(|event| event["action"] == "blocked_domain"));
            docker.client().stop_container(&name, None).await.unwrap();
            run_guest("import socket\ntry:\n socket.create_connection(('xpressclaw-proxy',8080),timeout=1)\nexcept OSError:\n pass\nelse:\n raise AssertionError('stopped proxy remained reachable')".into()).await;
            service.ensure(&config).await.unwrap();
            assert_eq!(std::fs::read(certificate).unwrap(), original_ca);
        }).catch_unwind().await;
        for container in [&guest, &origin, &name] {
            let _ = docker
                .client()
                .remove_container(
                    container,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
        let _ = docker
            .client()
            .remove_volume(
                &format!("{name}-ca"),
                Some(RemoveVolumeOptions { force: true }),
            )
            .await;
        let _ = docker
            .client()
            .remove_network(&network_name(&installation))
            .await;
        let _ = docker.client().remove_network(&outside).await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
