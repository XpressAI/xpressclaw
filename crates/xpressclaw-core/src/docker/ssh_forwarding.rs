//! Whether a host SSH agent can reach an isolated runner.
//!
//! The answer depends on the host operating system and the selected container
//! runtime, and two callers need it: the native worker builds the container
//! spec from it, and setup reports it so the UI can state the limitation
//! instead of inferring one from the browser's platform.

use std::path::{Path, PathBuf};

/// Docker Desktop publishes the host agent at a fixed path inside its VM.
pub const DOCKER_DESKTOP_SSH_AGENT_SOURCE: &str = "/run/host-services/ssh-auth.sock";

/// Why a host agent cannot reach a runner on macOS under Podman.
///
/// `podman machine` shares only the user's home directory into the VM, and
/// virtiofs carries file contents rather than proxying `connect()`, so no
/// host agent socket is reachable from inside a container. Tracked upstream as
/// <https://github.com/containers/podman/issues/23785>.
pub const PODMAN_MACOS_REASON: &str =
    "Podman on macOS cannot forward a host SSH agent into a container, because virtiofs \
     does not proxy Unix socket connections into the Podman machine VM";

/// How a host SSH agent socket reaches the runner, if it can at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentForwarding {
    /// Bind the detected host socket straight into the runner.
    HostSocket,
    /// Use Docker Desktop's macOS host-services bridge.
    DockerDesktopBridge,
    /// No socket can reach the runner. Mounted `~/.ssh` files still work.
    Unsupported(&'static str),
}

impl SshAgentForwarding {
    /// Host path to bind at the runner's agent socket, or `None` when the
    /// runtime cannot carry an agent at all.
    pub fn mount_source(&self, host_socket: &Path) -> Option<PathBuf> {
        match self {
            Self::HostSocket => Some(host_socket.to_path_buf()),
            Self::DockerDesktopBridge => Some(PathBuf::from(DOCKER_DESKTOP_SSH_AGENT_SOURCE)),
            Self::Unsupported(_) => None,
        }
    }

    /// Explanation to show a user, when forwarding cannot work.
    pub fn unsupported_reason(&self) -> Option<&'static str> {
        match self {
            Self::Unsupported(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Decide how a host SSH agent reaches runners on this host.
///
/// `runtime` is the Docker-compatible runtime name reported by the daemon's
/// compatibility API, as returned by `DockerManager::runtime`.
pub fn ssh_agent_forwarding(
    macos_host: bool,
    runtime: &str,
    docker_desktop: bool,
) -> SshAgentForwarding {
    if !macos_host {
        // Linux hosts share the socket's own namespace with the runner.
        return SshAgentForwarding::HostSocket;
    }
    if docker_desktop {
        return SshAgentForwarding::DockerDesktopBridge;
    }
    if runtime == "podman" {
        return SshAgentForwarding::Unsupported(PODMAN_MACOS_REASON);
    }
    // Other macOS runtimes keep the existing direct mount: we have no evidence
    // that they need the bridge, and guessing would break working setups.
    SshAgentForwarding::HostSocket
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_SOCKET: &str = "/private/tmp/com.apple.launchd.abc/Listeners";

    #[test]
    fn linux_hosts_mount_the_detected_socket() {
        for runtime in ["docker", "podman"] {
            let forwarding = ssh_agent_forwarding(false, runtime, false);
            assert_eq!(
                forwarding.mount_source(Path::new(HOST_SOCKET)),
                Some(PathBuf::from(HOST_SOCKET))
            );
            assert_eq!(forwarding.unsupported_reason(), None);
        }
    }

    #[test]
    fn docker_desktop_on_macos_uses_the_host_services_bridge() {
        let forwarding = ssh_agent_forwarding(true, "docker", true);
        assert_eq!(
            forwarding.mount_source(Path::new(HOST_SOCKET)),
            Some(PathBuf::from(DOCKER_DESKTOP_SSH_AGENT_SOURCE))
        );
    }

    #[test]
    fn podman_on_macos_cannot_forward_an_agent() {
        let forwarding = ssh_agent_forwarding(true, "podman", false);
        assert_eq!(forwarding.mount_source(Path::new(HOST_SOCKET)), None);
        assert_eq!(forwarding.unsupported_reason(), Some(PODMAN_MACOS_REASON));
    }

    #[test]
    fn other_macos_runtimes_keep_the_direct_mount() {
        let forwarding = ssh_agent_forwarding(true, "docker", false);
        assert_eq!(
            forwarding.mount_source(Path::new(HOST_SOCKET)),
            Some(PathBuf::from(HOST_SOCKET))
        );
    }
}
