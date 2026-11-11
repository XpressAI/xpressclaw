//! One firewall namespace per proxied Agent. Only the guard has NET_ADMIN.
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, ListContainersOptions,
    RemoveContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::HostConfig;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use super::{
    network::render_rules,
    service::{atomic_write, failure, prefix, LIFECYCLE, OWNER},
    ProxyConfig,
};
use crate::docker::manager::DockerManager;
use crate::error::Result;

// The image supplies iptables and BusyBox. Its Docker daemon is never started.
pub const IMAGE: &str = "docker.io/library/docker:29.6.1-dind@sha256:66d292e5c26bd33a6f6f61cacb880de2186339a524ecba1ce098dbbaceed6515";
const SCRIPT: &str = include_str!("network-guard.sh");
const KIND: &str = "io.xpressclaw.network-guard";
const AGENT: &str = "io.xpressclaw.agent-id";
const REVISION: &str = "io.xpressclaw.network-revision";

pub fn write_policy(data_dir: &Path, config: &ProxyConfig) -> Result<()> {
    let directory = data_dir.join("proxy/policy");
    let address = std::fs::read_to_string(directory.join("gateway.address"))
        .ok()
        .filter(|address| address.parse::<std::net::IpAddr>().is_ok());
    let mut rules = address
        .map(|address| format!("proxy\t{address}\n"))
        .unwrap_or_default();
    rules.push_str(&render_rules(&config.network_rules).map_err(failure)?);
    atomic_write(&directory.join("network.rules"), rules.as_bytes())?;
    atomic_write(
        &directory.join("network-guard.sh"),
        SCRIPT.replace("\r\n", "\n").as_bytes(),
    )
}

fn filters(installation: &str, agent: &str) -> HashMap<String, Vec<String>> {
    HashMap::from([(
        "label".into(),
        vec![
            format!("{OWNER}={installation}"),
            format!("{AGENT}={agent}"),
            format!("{KIND}=1"),
        ],
    )])
}

fn belongs_to(labels: Option<&HashMap<String, String>>, installation: &str, agent: &str) -> bool {
    labels.is_some_and(|labels| {
        [
            (OWNER, installation),
            (AGENT, agent),
            (KIND, "1"),
            ("io.xpressclaw.lifecycle", "service"),
        ]
        .iter()
        .all(|(key, value)| labels.get(*key).map(String::as_str) == Some(*value))
    })
}

pub async fn ensure(
    docker: &DockerManager,
    data_dir: &Path,
    installation: &str,
    agent: &str,
) -> Result<String> {
    let _guard = LIFECYCLE.lock().await;
    let policy = data_dir.join("proxy/policy");
    let expected = format!(
        "{:x}",
        Sha256::digest(std::fs::read(policy.join("network.rules")).map_err(failure)?)
    );
    let revision = format!(
        "{:x}",
        Sha256::digest(format!("{IMAGE}\0{SCRIPT}\0{}", policy.display()).as_bytes())
    );
    let guards = docker
        .client()
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: filters(installation, agent),
            ..Default::default()
        }))
        .await
        .map_err(failure)?;
    // Stopped or obsolete guards can still be referenced by retained runtimes.
    // Use a fresh ID so normal runtime recovery notices the namespace change.
    let id = if let Some(existing) = guards
        .iter()
        .find(|guard| {
            belongs_to(guard.labels.as_ref(), installation, agent)
                && guard.state.as_deref() == Some("running")
                && guard
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(REVISION))
                    == Some(&revision)
        })
        .and_then(|guard| guard.id.clone())
    {
        existing
    } else {
        if docker.client().inspect_image(IMAGE).await.is_err() {
            docker.pull_image(IMAGE).await?;
        }
        let agent_hash = format!("{:x}", Sha256::digest(agent.as_bytes()));
        let name = format!(
            "{}-net-{}-{}",
            prefix(installation),
            &agent_hash[..12],
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let response = docker
            .client()
            .create_container(
                Some(CreateContainerOptions {
                    name,
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(IMAGE.to_string()),
                    user: Some("0".into()),
                    entrypoint: Some(vec!["sh".into(), "/policy/network-guard.sh".into()]),
                    cmd: Some(vec![]),
                    labels: Some(HashMap::from([
                        (OWNER.into(), installation.into()),
                        (AGENT.into(), agent.into()),
                        (KIND.into(), "1".into()),
                        (REVISION.into(), revision),
                        ("io.xpressclaw.lifecycle".into(), "service".into()),
                    ])),
                    host_config: Some(HostConfig {
                        network_mode: Some(format!("{}-outside", prefix(installation))),
                        extra_hosts: (docker.runtime() == "docker")
                            .then(|| vec!["host.docker.internal:host-gateway".into()]),
                        cap_drop: Some(vec!["ALL".into()]),
                        cap_add: Some(vec!["NET_ADMIN".into()]),
                        security_opt: Some(vec!["no-new-privileges:true".into()]),
                        readonly_rootfs: Some(true),
                        tmpfs: Some(HashMap::from([
                            ("/run".into(), "rw,nosuid,nodev,size=16m".into()),
                            ("/tmp".into(), "rw,nosuid,nodev,size=1m".into()),
                            ("/var/lib/docker".into(), "rw,nosuid,nodev,size=1m".into()),
                        ])),
                        binds: Some(vec![format!("{}:/policy:ro,z", policy.display())]),
                        memory: Some(128 * 1024 * 1024),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(failure)?;
        docker
            .client()
            .start_container::<String>(&response.id, None)
            .await
            .map_err(failure)?;
        response.id
    };
    wait_ready(docker, &id, &expected).await?;
    Ok(id)
}

pub async fn wait_ready(docker: &DockerManager, guard: &str, expected: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let inspected = docker
                .client()
                .inspect_container(guard, None)
                .await
                .map_err(failure)?;
            if inspected
                .state
                .is_none_or(|state| state.running != Some(true))
            {
                return Err(failure(
                    "network guard stopped before installing its firewall",
                ));
            }
            let exec = docker
                .client()
                .create_exec(
                    guard,
                    CreateExecOptions {
                        cmd: Some(vec!["cat", "/run/xpressclaw-network/ready"]),
                        attach_stdout: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(failure)?;
            let mut value = String::new();
            if let StartExecResults::Attached { mut output, .. } = docker
                .client()
                .start_exec(&exec.id, None)
                .await
                .map_err(failure)?
            {
                while let Some(chunk) = output.next().await {
                    value.push_str(&chunk.map_err(failure)?.to_string());
                }
            }
            if value.trim() == expected {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| failure("network guard did not apply the current permissions within 30 seconds"))?
}

/// Check unexpected namespace dependents before deleting any Agent files.
pub async fn preflight_removal(
    docker: &DockerManager,
    installation: &str,
    agent: &str,
    removing: &[String],
) -> Result<()> {
    let guards = docker
        .client()
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: filters(installation, agent),
            ..Default::default()
        }))
        .await
        .map_err(failure)?;
    for guard in guards {
        if !belongs_to(guard.labels.as_ref(), installation, agent) {
            continue;
        }
        let Some(id) = guard.id else {
            continue;
        };
        for dependent in docker.container_dependents(&id).await? {
            let name = dependent
                .name
                .as_deref()
                .unwrap_or_default()
                .trim_start_matches('/');
            if !removing.iter().any(|candidate| {
                candidate == name || Some(candidate.as_str()) == dependent.id.as_deref()
            }) {
                return Err(failure(format!("network namespace is still used by {name}; migrate or remove that container before deleting this Agent")));
            }
        }
    }
    Ok(())
}

/// Called only after the Agent's retained and preserved runtimes are removed.
pub async fn remove_for_agent(
    docker: &DockerManager,
    installation: &str,
    agent: &str,
) -> Result<()> {
    let _guard = LIFECYCLE.lock().await;
    let guards = docker
        .client()
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            filters: filters(installation, agent),
            ..Default::default()
        }))
        .await
        .map_err(failure)?;
    for guard in guards {
        if !belongs_to(guard.labels.as_ref(), installation, agent) {
            continue;
        }
        if let Some(id) = guard.id {
            docker.ensure_no_container_dependents(&id).await?;
            docker
                .client()
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(failure)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        network::{NetworkProtocol, NetworkRule},
        service::{configure_runner, ProxyService},
    };
    use super::*;
    use crate::config::AgentConfig;
    use crate::docker::manager::ContainerSpec;
    use bollard::container::NetworkingConfig;
    use bollard::models::EndpointSettings;
    use bollard::volume::RemoveVolumeOptions;
    use futures_util::FutureExt;

    #[test]
    fn namespace_ownership_requires_installation_agent_and_service_identity() {
        let mut labels = HashMap::from([
            (OWNER.into(), "one".into()),
            (AGENT.into(), "agent".into()),
            (KIND.into(), "1".into()),
            ("io.xpressclaw.lifecycle".into(), "service".into()),
        ]);
        assert!(belongs_to(Some(&labels), "one", "agent"));
        assert!(!belongs_to(Some(&labels), "two", "agent"));
        assert!(!belongs_to(Some(&labels), "one", "other"));
        labels.remove(KIND);
        assert!(!belongs_to(Some(&labels), "one", "agent"));
    }

    async fn command(
        docker: &DockerManager,
        container: &str,
        script: &str,
        root: bool,
    ) -> (i64, String) {
        let exec = docker
            .client()
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(vec!["sh", "-c", script]),
                    user: root.then_some("0"),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            docker.client().start_exec(&exec.id, None).await.unwrap()
        {
            while let Some(chunk) = output.next().await {
                text.push_str(&chunk.unwrap().to_string());
            }
        }
        (
            docker
                .client()
                .inspect_exec(&exec.id)
                .await
                .unwrap()
                .exit_code
                .unwrap(),
            text,
        )
    }

    async fn success(docker: &DockerManager, container: &str, script: &str) -> String {
        let (status, text) = command(docker, container, script, false).await;
        assert_eq!(status, 0, "{text}");
        text
    }

    async fn applied(docker: &DockerManager, guard: &str, directory: &Path) {
        let expected = format!(
            "{:x}",
            Sha256::digest(std::fs::read(directory.join("proxy/policy/network.rules")).unwrap())
        );
        wait_ready(docker, guard, &expected).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the network test image and a daemon-shared XPRESSCLAW_PROXY_TEST_ROOT"]
    async fn network_permissions_roundtrip() {
        let Ok(root) = std::env::var("XPRESSCLAW_PROXY_TEST_ROOT") else {
            return;
        };
        let image = std::env::var("XPRESSCLAW_NETWORK_TEST_IMAGE")
            .unwrap_or_else(|_| "xpressclaw-environment-test:local".into());
        std::fs::create_dir_all(&root).unwrap();
        let directory = tempfile::tempdir_in(root).unwrap();
        let installation = format!("network-test-{}", uuid::Uuid::new_v4());
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
        let other = format!("{name}-other");
        let helper = format!("{name}-helper");
        let agent = AgentConfig {
            name: "network-client".into(),
            ..Default::default()
        };
        let mut guest_id = None;
        let result = std::panic::AssertUnwindSafe(async {
            let mut config = ProxyConfig { web_ports: vec![80, 443, 9000], ..Default::default() };
            service.apply(&config).await.unwrap();
            // Real SSH authentication plus small TCP/UDP services, all local to
            // this test. No external service or model credentials are involved.
            let fixture = r#"/usr/sbin/sshd -o PermitRootLogin=yes -o PasswordAuthentication=no -o AuthorizedKeysFile=/etc/ssh/test-authorized-keys
exec python3 -u - <<'PY'
import http.server,json,socket,socketserver,threading
class Web(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  body=json.dumps(dict(self.headers)).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args): pass
class TCP(socketserver.BaseRequestHandler):
 def handle(self): self.request.sendall(b'service-ok')
class UDP(socketserver.BaseRequestHandler):
 def handle(self): self.request[1].sendto(self.request[0],self.client_address)
for port in (5432,5555):
 server=socketserver.ThreadingTCPServer(('0.0.0.0',port),TCP);threading.Thread(target=server.serve_forever,daemon=True).start()
server=socketserver.ThreadingUDPServer(('0.0.0.0',5353),UDP);threading.Thread(target=server.serve_forever,daemon=True).start()
http.server.HTTPServer(('0.0.0.0',9000),Web).serve_forever()
PY"#;
            for (container, alias) in [(&origin, "network-target"), (&other, "other-target")] {
            docker.client().create_container(Some(CreateContainerOptions { name: container.clone(), platform: None }), ContainerConfig {
                image: Some(image.clone()), user: Some("0".into()), entrypoint: Some(vec!["sh".into(), "-c".into(), fixture.into()]), cmd: Some(vec![]),
                host_config: Some(HostConfig { network_mode: Some(outside.clone()), ..Default::default() }),
                networking_config: Some(NetworkingConfig { endpoints_config: HashMap::from([(outside.clone(), EndpointSettings { aliases: Some(vec![alias.into()]), ..Default::default() })]) }),
                ..Default::default()
            }).await.unwrap();
            docker.client().start_container::<String>(container, None).await.unwrap();
            }
            let server_ip = docker.client().inspect_container(&origin, None).await.unwrap().network_settings.unwrap().networks.unwrap()[&outside].ip_address.clone().unwrap();
            // The fixture has its own node account and no writable host mounts.
            // An arbitrary host UID (1001 in CI) has no passwd entry for OpenSSH.
            let mut spec = ContainerSpec { image: image.clone(), expose_port: None, cmd: Some(vec!["sleep".into(), "infinity".into()]), run_as_host_user: false, ..Default::default() };
            configure_runner(&docker, directory.path(), &installation, &config, &agent, &mut spec, 12345).await.unwrap();
            let guard = spec.network_mode.as_deref().unwrap().strip_prefix("container:").unwrap().to_string();
            assert_eq!(ensure(&docker, directory.path(), &installation, &agent.name).await.unwrap(), guard);
            let guest = docker.launch_project_attached(&agent.name, &spec).await.unwrap();
            guest_id = Some(guest.info.container_id.clone());
            let guest = guest.info.container_id;
            assert_eq!(success(&docker, &guest, "id -un").await.trim(), "node");
            success(&docker, &guest, "cp /test-key /tmp/ssh-key && chmod 600 /tmp/ssh-key").await;
            let ssh = "ssh -i /tmp/ssh-key -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2 root@network-target printf ssh-ok";
            assert!(success(&docker, &guest, ssh).await.contains("ssh-ok"));
            let denied = r#"python3 - <<'PY'
import socket
for port in (5432,5555,9000):
 try: socket.create_connection(('network-target',port),timeout=.4)
 except OSError: pass
 else: raise AssertionError(f'unexpected direct access to {port}')
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(.4)
try: s.sendto(b'blocked',('network-target',5353));s.recv(20)
except OSError: pass
else: raise AssertionError('unexpected UDP access')
PY"#;
            success(&docker, &guest, denied).await;
            let web = r#"python3 - <<'PY'
import urllib.request,json
request=urllib.request.Request('http://network-target:9000/',headers={'User-Agent':'test (private@example.invalid)'})
with urllib.request.urlopen(request,timeout=5) as response: assert json.load(response)['User-Agent']=='XpressCLAW/1.0'
PY"#;
            success(&docker, &guest, web).await;
            success(&docker, &guest, "printf retained > /tmp/network-sentinel").await;
            let restarted = docker.launch_project_attached(&agent.name, &spec).await.unwrap();
            assert_eq!(restarted.info.container_id, guest);
            assert_eq!(success(&docker, &guest, "cat /tmp/network-sentinel").await, "retained");
            assert!(success(&docker, &guest, ssh).await.contains("ssh-ok"));
            // Even container root cannot remove the firewall or use raw sockets.
            assert_ne!(command(&docker, &guest, "iptables -F OUTPUT", true).await.0, 0);
            let (status, output) = command(&docker, &guest, "python3 -c 'import os,socket\nassert os.getuid()==0\ntry: socket.socket(socket.AF_PACKET,socket.SOCK_RAW)\nexcept PermissionError: pass\nelse: raise AssertionError(\"raw sockets available\")'", true).await;
            assert_eq!(status, 0, "{output}");
            config.network_rules.extend([
                NetworkRule { name: "Database".into(), destination: "network-target".into(), ports: "5432-5433".into(), ..Default::default() },
                NetworkRule { name: "UDP".into(), protocol: NetworkProtocol::Udp, destination: format!("{server_ip}/32"), ports: "5353".into(), ..Default::default() },
            ]);
            service.apply(&config).await.unwrap();
            applied(&docker, &guard, directory.path()).await;
            success(&docker, &guest, "python3 -c 'import socket\ns=socket.create_connection((\"network-target\",5432),timeout=2);assert s.recv(20)==b\"service-ok\"\nu=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.settimeout(2);u.sendto(b\"allowed\",(\"network-target\",5353));assert u.recv(20)==b\"allowed\"'").await;
            success(&docker, &guest, web).await;
            success(&docker, &guest, "python3 -c 'import socket\naddress=socket.gethostbyname(\"other-target\")\ntry: socket.create_connection((address,5432),timeout=.4)\nexcept OSError: pass\nelse: raise AssertionError(\"destination rule did not restrict access\")'").await;
            // A runner's hosts-file edits must not grant the firewall a new IP.
            let other_ip = docker.client().inspect_container(&other, None).await.unwrap().network_settings.unwrap().networks.unwrap()[&outside].ip_address.clone().unwrap();
            assert_eq!(command(&docker, &guest, &format!("cp /etc/hosts /tmp/original-hosts; printf '%s\\n' '{other_ip} network-target' >> /etc/hosts"), true).await.0, 0);
            config.network_rules[1].ports = "5432-5434".into();
            service.apply(&config).await.unwrap();
            applied(&docker, &guard, directory.path()).await;
            success(&docker, &guest, "python3 -c 'import socket\ntry: socket.create_connection((\"network-target\",5432),timeout=.4)\nexcept OSError: pass\nelse: raise AssertionError(\"hosts edit expanded network permission\")'").await;
            assert_eq!(command(&docker, &guest, "cat /tmp/original-hosts > /etc/hosts", true).await.0, 0);
            config.network_rules[0].enabled = false;
            config.inspection_bypass = vec!["network-target".into()];
            service.apply(&config).await.unwrap();
            applied(&docker, &guard, directory.path()).await;
            assert_ne!(command(&docker, &guest, ssh, false).await.0, 0);
            success(&docker, &guest, "python3 -c 'import socket\ns=socket.create_connection((\"xpressclaw-proxy\",8080),timeout=2);s.sendall(b\"CONNECT network-target:22 HTTP/1.1\\r\\nHost: network-target:22\\r\\n\\r\\n\");assert b\"403\" in s.recv(200)'").await;
            docker.client().create_container(Some(CreateContainerOptions { name: helper.clone(), platform: None }), ContainerConfig {
                image: Some(image.clone()), entrypoint: Some(vec!["sleep".into()]), cmd: Some(vec!["infinity".into()]),
                host_config: Some(HostConfig { network_mode: Some(format!("container:{guard}")), ..Default::default() }), ..Default::default()
            }).await.unwrap();
            docker.client().start_container::<String>(&helper, None).await.unwrap();
            assert!(docker.remove_owned_workload(&agent.name).await.unwrap_err().to_string().contains("network namespace is still used"));
            assert!(docker.client().inspect_container(&guest, None).await.is_ok());
            docker.client().remove_container(&helper, Some(RemoveContainerOptions { force: true, ..Default::default() })).await.unwrap();
            docker.remove_owned_workload(&agent.name).await.unwrap();
            guest_id = None;
            assert!(docker.client().inspect_container(&guard, None).await.is_err());
        }).catch_unwind().await;
        let _ = docker
            .client()
            .remove_container(
                &helper,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        if let Some(guest) = guest_id {
            let _ = docker
                .client()
                .remove_container(
                    &guest,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
        let _ = remove_for_agent(&docker, &installation, &agent.name).await;
        for container in [&origin, &other, &name] {
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
            .remove_network(&super::super::service::network_name(&installation))
            .await;
        let _ = docker.client().remove_network(&outside).await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
