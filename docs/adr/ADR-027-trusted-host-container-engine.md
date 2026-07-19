# ADR-027: Trusted Host Container-Engine Access

## Status

Accepted

## Context

Many development repositories use Docker Compose for their test environment or
build container images as part of delivery. A runner that can edit the project
but cannot execute those workflows is incomplete for those repositories.

Installing a daemon inside every agent image duplicates storage, complicates
networking, and requires a privileged nested runtime. A separate managed
development environment remains the preferred long-term abstraction, but is
not yet available. The practical local engine may be rootful Docker, Docker
Desktop, or rootless Podman.

The expected threat model is trusted agents and trusted runner images. The
short-lived worker should contain accidental damage to its own root filesystem,
but the host container engine is not treated as a boundary against a malicious
worker.

## Decision

Each project has a `runner.container_engine` setting with two values:

- `none`, the default, exposes no container-engine socket;
- `host` mounts the local Docker-compatible Unix socket used by the control
  plane into the worker as `/var/run/docker.sock`.

Host mode selects a distinct built-in image per agent product. The normal
Codex, Claude Code, and OpenCode images remain minimal. Their `-docker` variants
add Docker CLI, the Compose plugin, and Buildx, and carry a compatibility label
that the control plane verifies before running them. Custom ACP images may
supply their own client.

Xpressclaw detects `DOCKER_HOST` Unix sockets plus conventional Docker, Docker
Desktop, and rootless Podman socket paths. TCP and Windows named-pipe endpoints
are not mounted. The socket's host group is added to rootful worker containers
when needed.

In host mode the project workspace and additional folders are mounted into the
worker at the same absolute paths they have on the host. This path parity is
required because Compose sends bind-mount source paths to the host daemon; a
worker-only `/workspace` path would not identify the project on the host.

The setup and runner settings UI state the authority being granted. Enabling
the option for a built-in agent automatically switches between the minimal and
host-engine image variants without overwriting a custom image.

## Consequences

### Positive

- Existing Compose-based test environments work without rebuilding the agent
  image around every project's services.
- Agents can build deployment images and can create richer development
  containers using the host engine and its image cache.
- The default runner download and attack surface do not grow.
- Docker and rootless Podman use the same Docker-compatible client path.

### Negative

- A worker with socket access can control the host engine, including removing
  its containers and volumes or mounting host paths available to the daemon.
- The worker is no longer isolated from data reachable through that engine.
- Host path parity reveals absolute project paths inside the worker.
- Remote TCP engines and native Windows named pipes require a future design.

## Alternatives considered

### Docker-in-Docker

Rejected as the default because it needs a nested daemon, additional privilege,
duplicated image storage, and more complex lifecycle and networking.

### Put Docker tooling in every runner

Rejected because most tasks do not need it and the extra client and plugins
would make every product image larger.

### Wait for managed development environments

Rejected as the only option because it would leave common repository test and
release workflows unusable in the current implementation.
