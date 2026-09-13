# ADR-036: Opt-in host SSH access

## Status

Accepted

## Context

Most existing workspaces are Git clones, and many use SSH remotes. Mounting
the workspace gives a retained Agent the repository and its Git metadata, but
not the host credential needed to fetch or push. Mounting `~/.ssh` exposes its
private-key files to the runner, so that access must be an explicit user choice
rather than a default.

GitHub repositories already have a narrower path: the project-scoped GitHub
connector supplies an HTTPS credential helper and rewrites standard GitHub SSH
remote forms. Other Git hosts, custom SSH aliases, and GitHub projects without
connector access still need an explicit bridge.

## Decision

Each ACP Agent has an opt-in `runner.ssh_agent_forwarding` setting.

When enabled, XpressClaw:

- mounts the host `~/.ssh` directory read-write at the runner user's normal SSH
  location, giving the trusted harness the same file access it would have when
  run directly on the host;
- discovers `SSH_AUTH_SOCK` and common desktop/user-service Unix socket
  locations and forwards a live agent when one is available, without blocking
  the runner when one is not;
- uses Docker Desktop's `/run/host-services/ssh-auth.sock` bridge when the
  selected daemon identifies itself as Docker Desktop on macOS, while other
  runtimes mount the detected socket directly;
- skips agent forwarding entirely, keeping the `~/.ssh` mount, when the host
  and runtime cannot carry a socket into a runner at all;
- points Git's SSH transport at the forwarded socket when present, and in that
  case keeps new host keys in private Agent-scoped storage so they
  survive container recreation, and uses the host known-host set as an
  additional trust source;
- overlays the effective SSH client configuration with the forwarded socket,
  so host `IdentityAgent` paths do not break direct `ssh`, `scp`, or `rsync`
  commands inside the runner;
- uses a shared SELinux label for those explicit mounts and adds the socket's
  host group to the container, matching rootless Podman and Docker hosts; and
- includes opaque socket and known-host device/inode generations plus the
  effective materialized SSH configuration in the retained-container
  specification, so replacing any source or changing an included config
  recreates the Agent container on the next turn.

Setup inspects only Git metadata to flag workspaces with SSH remotes. Both
setup and Agent settings keep this access off by default and state that enabling
it shares host SSH files and any detected agent.

Whether a host agent can reach a runner at all is a property of the host and
its container runtime, so one function in `docker::ssh_forwarding` answers it
for every caller. On macOS under Podman the answer is no: `podman machine`
shares only the user's home directory into its VM, and virtiofs carries file
contents rather than proxying `connect()`, so no host agent socket is reachable
from inside a container ([podman#23785](https://github.com/containers/podman/issues/23785)).

When forwarding is impossible, XpressClaw mounts `~/.ssh` and stops there. It
does not set `SSH_AUTH_SOCK` and does not overlay `IdentityAgent`, because
aiming those at a socket the runtime cannot carry would point every `ssh` call
in the runner at a dead socket and break key-file access that otherwise works.
This is the same graceful degradation already applied when no host agent is
running.

Setup and Agent settings report that reason from the server rather than
inferring one from the client browser's platform, and state it before the
choice is made instead of interrupting afterwards. The option itself always
stays configurable: key-file access is still useful, and a user must be able to
enable it once a future runtime release fixes forwarding.

## Consequences

Users can explicitly give a trusted harness the same SSH access it would have
when run directly on the host. Standard GitHub work continues to prefer the
narrower project-scoped HTTPS credential bridge.

The mount is not repository-scoped: a trusted process can read or change files
in `~/.ssh`, and a forwarded agent can sign with any loaded key. The option
therefore remains off by default and must be enabled only for trusted runner
images and tasks. When XpressClaw forwards an agent, host keys learned through
its `accept-new` Git transport remain in that Agent's private runtime data
until the XpressClaw data directory is removed.
