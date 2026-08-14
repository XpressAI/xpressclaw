# Configuration Reference

Xpressclaw stores local control-plane configuration in `xpressclaw.yaml`.
Create and edit durable Agents through the web UI. An Agent owns work history,
memory, and workspace configuration while its replaceable ACP harness owns
reasoning, tools, and subagents. Xpressclaw does not define a persona or system
prompt for the harness.

The packaged Desktop app uses the default local **instance** at
`~/.xpressclaw` and starts its bundled server automatically. The CLI discovers
the same instance, so ordinary use is simply `xpressclaw up`. An instance is
an installation-level control plane that can manage many Projects and Agent
workspaces; it is not a requirement to place XpressClaw configuration in every
source repository. The default instance keeps its configuration, database,
managed workspaces, background log, and PID file under the same root.

This local `xpressclaw.yaml` is distinct from the optional
`.xpressclaw.yml` Project synchronization manifest. The latter is a small,
portable pointer to a separate Git store and must never contain credentials or
machine-specific runner settings. See [Git-backed Project
synchronization](project-sync.md).

`xpressclaw init` is optional. It provisions the default instance ahead of
time, or an explicitly named advanced instance:

```bash
xpressclaw init
xpressclaw init /srv/xpressclaw/staging
```

A newly initialized instance writes an empty starting point with explicit
instance-local data paths:

```yaml
system:
  isolation: docker
  data_dir: /home/me/.xpressclaw
  workspace_dir: /home/me/.xpressclaw/workspaces

agents: []
```

## Agent and ACP harness representation

Internally, durable sessions remain in the `agents` array for file and API
compatibility. New entries use only the ACP runner fields:

```yaml
agents:
  - name: site-codex # stable internal ID
    backend: codex
    runner:
      kind: codex
      image: ghcr.io/xpressai/xpressclaw-runner-codex:latest
      workspace: /home/me/projects/site
      project_name: Site maintainer # user-facing Agent name
      subscription_auth: true
      ssh_agent_forwarding: false
      container_engine: none
    volumes: []
```

### Runner fields

| Field | Description |
|---|---|
| `kind` | `codex`, `claude`, `opencode`, or `custom` |
| `image` | Product-specific ACP server image or compatible derivative |
| `workspace` | Host project mounted read-write at `/workspace`, or at the same absolute path in host-engine mode |
| `project_name` | User-facing Agent name; falls back to the workspace folder when omitted |
| `model` | Optional model value ID applied through ACP session configuration |
| `subscription_auth` | Reuse the built-in product's host login directory |
| `ssh_agent_forwarding` | Forward a detected host SSH agent plus SSH config/known hosts; private keys are never mounted (default `false`) |
| `container_engine` | `none` (default) or trusted `host` Docker/Podman socket access |
| `command` | ACP server argument list; required for custom harnesses and supports `{workspace}` |

Additional `volumes` use Docker-style `host:container[:options]` syntax. Options
may combine `ro` or `rw` with `z` (a shared SELinux label) or `Z` (a private
label), for example `/srv/shared:/workspace/shared:ro,z`. Xpressclaw
automatically requests a shared label for the managed workspace and built-in
harness login/configuration mounts when the Docker-compatible runtime reports
SELinux support. Explicit mounts are never relabeled unless they include `z` or
`Z`; in particular, Xpressclaw never relabels the Docker or Podman socket.

The UI label uses `runner.project_name`, falling back to the workspace folder.
The top-level `name` field is only a stable internal reference used by tasks,
schedules, and workflows. Codex and Claude receive no Xpressclaw profile or identity prompt;
they retain ownership of their own instructions, tools, and subagents. The
control plane sends task text as an ACP `session/prompt` and records standard
ACP updates. Older profile fields are removed automatically when a
configuration is loaded.

For another harness, set `kind: custom`, provide an image, and enter the command
that starts its ACP server over stdin/stdout. Authentication and credential
mounts for custom harnesses are explicit volumes because Xpressclaw does not know
their host directory conventions.

## Built-in images

| Product | Minimal image | Host-engine image |
|---|---|---|
| Codex | `ghcr.io/xpressai/xpressclaw-runner-codex:latest` | `ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest` |
| Claude Code | `ghcr.io/xpressai/xpressclaw-runner-claude:latest` | `ghcr.io/xpressai/xpressclaw-runner-claude-docker:latest` |
| OpenCode | `ghcr.io/xpressai/xpressclaw-runner-opencode:latest` | `ghcr.io/xpressai/xpressclaw-runner-opencode-docker:latest` |

The host-engine variants add Docker CLI, Compose, and Buildx. When
`container_engine: host` is enabled, Xpressclaw discovers the local Unix
socket used by the control plane, mounts it at `/var/run/docker.sock`, and
selects the matching host-engine image for a built-in runner. The project and
additional folders are mounted at their absolute host paths so Compose bind
mounts resolve against the same paths in the host engine.

Docker versus Podman is not a configuration choice. Xpressclaw automatically
uses an explicit `DOCKER_HOST`, then a live user-level Docker Desktop or
rootless Podman socket, then the platform Docker default. Image inspection,
pulls, worker launches, and trusted host-engine access all use that one
selected endpoint.

This mode gives the runner the authority of the host Docker or Podman daemon:
it can manage containers, images, networks, and volumes and ask the daemon to
mount host paths. It is intentionally opt-in and is suitable only when the
agent and runner image are trusted. It is not a security boundary.

The retired `xpressclaw-native-runner:latest` tag is migrated to the image for
the configured product when an older file is loaded.

Codex starts in its `agent-full-access` mode by default. This disables Codex's
nested filesystem sandbox and approval prompts **inside the Agent's retained
container**; Docker or Podman remains the security boundary, and the
host container engine remains unavailable unless `container_engine: host` is
explicitly selected. Set `runner.environment.INITIAL_AGENT_MODE` to another
Codex ACP mode (for example, `agent`) when a project needs the additional inner
sandbox. A mode selected through the Agent or task controls remains
authoritative for that session.

## Existing repositories and SSH remotes

An existing workspace is mounted read-write, including its `.git` directory.
For GitHub repositories, XpressClaw prefers its project-scoped HTTPS credential
helper and rewrites the standard `git@github.com:` remote forms when a GitHub
connector is available. No SSH key is needed for that path.

For GitLab, self-hosted Git servers, SSH host aliases, or a GitHub repository
without connector access, enable `runner.ssh_agent_forwarding`. XpressClaw
bind-mounts the live SSH-agent Unix socket and `~/.ssh/known_hosts` when it
exists. It materializes a private, read-only configuration from
`~/.ssh/config` and recursively referenced regular `Include` files under the
host home directory, preserving their lexical order and inline position.
Included files must contain only recognized OpenSSH client directives;
unknown, binary, and private-key-format files matched by broad globs are
skipped. Private keys are never exposed to the container. New host keys
accepted by the runner are kept in private,
Agent-scoped storage under XpressClaw's data directory, so changed-key checks
survive retained-container recreation. The container is recreated
automatically when the host agent replaces its socket or the effective SSH
configuration or known-host source changes.

On macOS, when the selected daemon identifies itself as Docker Desktop,
XpressClaw uses Docker Desktop's `/run/host-services/ssh-auth.sock` bridge
instead of trying to bind-mount the native macOS socket through its Linux VM.
Other Docker-compatible runtimes continue to mount the detected host socket.

The setting is deliberately opt-in: every process in that Agent's retained
container can request signatures from every key currently loaded in the host
agent. If XpressClaw runs as a user service and cannot see the desktop value,
import `SSH_AUTH_SOCK` into the service manager and restart XpressClaw, for
example with `systemctl --user import-environment SSH_AUTH_SOCK`. The Agent
readiness panel reports the detected socket or a specific configuration issue.

## Advanced independent instances

Most people should use one default instance and create several Projects and
Agents inside it. A separate instance is useful only when configuration,
SQLite state, credentials, managed workspaces, logs, and runtime policy need
an independent boundary—for example production versus testing, separate
security contexts, or different remote hosts.

Create and run an alternate instance on its own port:

```bash
xpressclaw init /srv/xpressclaw/staging
xpressclaw up --instance /srv/xpressclaw/staging --port 9001
```

`--workdir` remains a deprecated alias for `--instance`. Existing CLI
installations that have `xpressclaw.yaml` in the current directory remain
discoverable when the default instance has not yet been configured. Existing
YAML files also retain their configured/default data paths; XpressClaw does
not silently move databases. To turn an older directory into a truly isolated
instance, set `system.data_dir` explicitly before starting it. The former bare
`xpressclaw init` current-directory behavior remains available as the explicit
`xpressclaw init .` spelling.

The word **profile** is reserved for a future client-side saved connection to
an instance. It is not another name for the server's configuration directory.
