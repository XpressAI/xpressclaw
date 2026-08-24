//! Optional, instance-scoped local collaboration services.
//!
//! Git hosting and build execution are deliberately separate capabilities.
//! This keeps the existing GitHub review lifecycle intact while allowing a
//! local forge and build provider to be introduced without teaching task and
//! workflow code about a second vendor.

mod providers;
mod secrets;
pub mod stack;

pub mod gitbucket;
pub mod jenkins;

pub use providers::{
    Build, BuildCapabilities, BuildProvider, BuildRequest, ForgeCapabilities, ForgeProvider, Issue,
    PullRequest, Repository,
};
pub use secrets::CollaborationSecrets;

use serde::{Deserialize, Serialize};

pub const GITBUCKET_IMAGE: &str = "ghcr.io/gitbucket/gitbucket:4.46.1";
pub const JENKINS_IMAGE: &str = "jenkins/jenkins:2.568.1-jdk21";
pub const GITBUCKET_INTERNAL_URL: &str = "http://gitbucket:8080";
pub const JENKINS_INTERNAL_URL: &str = "http://jenkins:8080";

/// Local collaboration credentials must travel directly to the configured
/// service endpoint. Inherited host proxy settings are outside XpressClaw's
/// managed trust boundary and must never receive forge or build credentials.
pub(crate) fn local_http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}

fn default_gitbucket_port() -> u16 {
    8088
}

fn default_jenkins_port() -> u16 {
    8089
}

fn default_gitbucket_image() -> String {
    GITBUCKET_IMAGE.to_string()
}

fn default_jenkins_image() -> String {
    JENKINS_IMAGE.to_string()
}

/// Non-secret, file-backed configuration for this control-plane instance.
/// Existing installations remain disabled after upgrading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollaborationConfig {
    pub enabled: bool,
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_gitbucket_port")]
    pub gitbucket_port: u16,
    #[serde(default = "default_jenkins_port")]
    pub jenkins_port: u16,
    #[serde(default = "default_gitbucket_image")]
    pub gitbucket_image: String,
    #[serde(default = "default_jenkins_image")]
    pub jenkins_image: String,
    /// Agent IDs that receive the collaboration MCP tools and managed Docker
    /// network. No Agent is authorized by default.
    #[serde(default)]
    pub authorized_agents: Vec<String>,
}

impl Default for CollaborationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_bind_address(),
            gitbucket_port: default_gitbucket_port(),
            jenkins_port: default_jenkins_port(),
            gitbucket_image: default_gitbucket_image(),
            jenkins_image: default_jenkins_image(),
            authorized_agents: Vec::new(),
        }
    }
}

impl CollaborationConfig {
    pub fn agent_authorized(&self, agent_id: &str) -> bool {
        self.enabled
            && self
                .authorized_agents
                .iter()
                .any(|candidate| candidate == agent_id)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        let bind_address = self
            .bind_address
            .parse::<std::net::IpAddr>()
            .map_err(|_| "local collaboration bind_address must be an IP address".to_string())?;
        if bind_address.is_unspecified() {
            return Err(
                "local collaboration bind_address cannot be a wildcard (0.0.0.0 or ::); use a connectable IP such as 127.0.0.1 or the host's LAN address"
                    .to_string(),
            );
        }
        if self.gitbucket_port == self.jenkins_port {
            return Err("GitBucket and Jenkins must use different host ports".to_string());
        }
        if self.gitbucket_port == 0 || self.jenkins_port == 0 {
            return Err("local collaboration host ports must be between 1 and 65535".to_string());
        }
        validate_pinned_image("GitBucket", &self.gitbucket_image)?;
        validate_pinned_image("Jenkins", &self.jenkins_image)?;
        Ok(())
    }

    pub fn gitbucket_url(&self) -> String {
        format!("http://{}:{}", self.url_host(), self.gitbucket_port)
    }

    pub fn jenkins_url(&self) -> String {
        format!("http://{}:{}", self.url_host(), self.jenkins_port)
    }

    fn url_host(&self) -> String {
        match self.bind_address.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V6(address)) => format!("[{address}]"),
            _ => self.bind_address.clone(),
        }
    }
}

fn validate_pinned_image(service: &str, image: &str) -> std::result::Result<(), String> {
    let image = image.trim();
    let last_component = image.rsplit('/').next().unwrap_or(image);
    let pinned = image.contains('@')
        || last_component
            .rsplit_once(':')
            .is_some_and(|(_, tag)| !tag.is_empty() && tag != "latest");
    if !pinned {
        return Err(format!(
            "{service} image must use an explicit version tag or digest; latest is not supported"
        ));
    }
    Ok(())
}

pub fn resource_prefix(installation_id: &str) -> String {
    let safe = installation_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>();
    format!("xpressclaw-collaboration-{safe}")
}

pub fn network_name(installation_id: &str) -> String {
    format!("{}-network", resource_prefix(installation_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    static PROXY_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct ProxyEnvironment(Vec<(&'static str, Option<String>)>);

    impl ProxyEnvironment {
        fn point_at(proxy: &str) -> Self {
            let values = [
                ("HTTP_PROXY", proxy),
                ("http_proxy", proxy),
                ("HTTPS_PROXY", proxy),
                ("https_proxy", proxy),
                ("ALL_PROXY", proxy),
                ("all_proxy", proxy),
                ("NO_PROXY", ""),
                ("no_proxy", ""),
            ];
            let previous = values
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in values {
                std::env::set_var(key, value);
            }
            Self(previous)
        }
    }

    impl Drop for ProxyEnvironment {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..) {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }

    struct LocalHttpTestServer {
        address: SocketAddr,
        shutdown: mpsc::Sender<()>,
        handle: Option<std::thread::JoinHandle<Vec<String>>>,
    }

    impl LocalHttpTestServer {
        fn start(body: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let (shutdown, shutdown_receiver) = mpsc::channel();
            let handle = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut requests = Vec::new();
                while Instant::now() < deadline {
                    match shutdown_receiver.try_recv() {
                        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_millis(250)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_millis(250)))
                                .unwrap();
                            requests.push(read_http_request(&mut stream));
                            write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("local HTTP test server failed: {error}"),
                    }
                }
                requests
            });
            Self {
                address,
                shutdown,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> Vec<String> {
            let _ = self.shutdown.send(());
            self.handle
                .take()
                .unwrap()
                .join()
                .expect("local HTTP test server panicked")
        }
    }

    impl Drop for LocalHttpTestServer {
        fn drop(&mut self) {
            let _ = self.shutdown.send(());
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        const MAX_REQUEST_SIZE: usize = 16 * 1024;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        while request.len() < MAX_REQUEST_SIZE {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(bytes_read) => {
                    request.extend_from_slice(&buffer[..bytes_read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("local HTTP test server could not read request: {error}"),
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    #[test]
    fn defaults_are_opt_in_and_loopback_only() {
        let config = CollaborationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.bind_address, "127.0.0.1");
        assert!(config.authorized_agents.is_empty());
        assert_eq!(config.gitbucket_image, GITBUCKET_IMAGE);
        assert_eq!(config.jenkins_image, JENKINS_IMAGE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_http_clients_ignore_inherited_proxy_settings() {
        const TEST_HOST: &str = "collaboration.xpressclaw.test";
        const PROXY_CANARY: &str = "would-leak-without-no-proxy";
        const LOCAL_CREDENTIAL: &str = "local-collaboration-credential";

        let _environment_lock = PROXY_ENV_LOCK.lock().await;
        let target_server = LocalHttpTestServer::start("target");
        let proxy_server = LocalHttpTestServer::start("proxy");
        let target_url = format!("http://{TEST_HOST}:{}/health", target_server.address.port());
        let proxy_url = format!("http://{}", proxy_server.address);
        let _proxy_environment = ProxyEnvironment::point_at(&proxy_url);

        let proxied_response = reqwest::Client::builder()
            .resolve(TEST_HOST, target_server.address)
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(&target_url)
            .bearer_auth(PROXY_CANARY)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let direct_response = local_http_client_builder()
            .resolve(TEST_HOST, target_server.address)
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(&target_url)
            .bearer_auth(LOCAL_CREDENTIAL)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let proxy_requests = proxy_server.finish();
        let target_requests = target_server.finish();
        drop(_proxy_environment);

        assert_eq!(proxied_response, "proxy");
        assert_eq!(direct_response, "target");
        assert_eq!(proxy_requests.len(), 1, "{proxy_requests:#?}");
        assert_eq!(target_requests.len(), 1, "{target_requests:#?}");

        let proxy_request = proxy_requests[0].to_ascii_lowercase();
        assert!(proxy_request.contains(&target_url));
        assert!(proxy_request.contains(&format!("authorization: bearer {PROXY_CANARY}")));
        assert!(!proxy_request.contains(LOCAL_CREDENTIAL));

        let target_request = target_requests[0].to_ascii_lowercase();
        assert!(target_request.contains(&format!("host: {TEST_HOST}")));
        assert!(target_request.contains(&format!("authorization: bearer {LOCAL_CREDENTIAL}")));
        assert!(!target_request.contains(PROXY_CANARY));
    }

    #[test]
    fn authorization_requires_both_stack_and_agent_opt_in() {
        let mut config = CollaborationConfig {
            authorized_agents: vec!["platform".to_string()],
            ..Default::default()
        };
        assert!(!config.agent_authorized("platform"));
        config.enabled = true;
        assert!(config.agent_authorized("platform"));
        assert!(!config.agent_authorized("other"));
    }

    #[test]
    fn images_remain_explicitly_pinned_and_ipv6_urls_are_valid() {
        let mut config = CollaborationConfig {
            enabled: true,
            bind_address: "::1".to_string(),
            ..Default::default()
        };
        assert_eq!(config.gitbucket_url(), "http://[::1]:8088");
        assert!(config.validate().is_ok());
        config.jenkins_image = "jenkins/jenkins:latest".to_string();
        assert!(config.validate().unwrap_err().contains("explicit version"));
    }

    #[test]
    fn wildcard_bind_addresses_are_rejected_as_unconnectable() {
        for bind_address in ["0.0.0.0", "::"] {
            let config = CollaborationConfig {
                bind_address: bind_address.to_string(),
                ..Default::default()
            };
            let error = config.validate().unwrap_err();
            assert!(error.contains("cannot be a wildcard"), "{error}");
            assert!(error.contains("connectable IP"), "{error}");
        }
    }
}
