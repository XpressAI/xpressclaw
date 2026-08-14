# ADR-038: Instances, Clients, and Remote Access

## Status

Accepted

## Context

XpressClaw historically used “working directory” for the directory containing
`xpressclaw.yaml`. The same product also asks an Agent for a working repository
or workspace. That overloaded wording made `xpressclaw init` look like a
per-repository step and encouraged users to create or locate control-plane
configuration inside each source checkout.

The actual runtime already has a different shape. One long-running server owns
SQLite state, queues, schedules, workflows, and many Project and Agent
workspaces. Desktop silently starts that server from `~/.xpressclaw`, while the
browser UI acts as a client. Multiple configuration directories were possible,
but their database, PID, and log defaults could still overlap, so they were not
a reliable independent-instance abstraction.

Remote use is a core scenario: a machine with repositories, credentials, and a
container runtime should keep Agents running while a person disconnects and
later reconnects from another device. The server previously listened on every
network interface with permissive CORS despite having no user authentication.
Desktop, meanwhile, assumes one local sidecar and has no connection-profile
model.

## Decision

### The server-side unit is an instance

An **instance** is one XpressClaw control plane and its configuration, database,
managed workspaces, background-process metadata, and runtime policy. One
instance owns many Projects; a Project owns Agents, Conversations, Tasks, and
workflows. An Agent workspace can point at any repository available on the
instance host.

The normal installation has one automatically discovered local instance at
`~/.xpressclaw`:

- Desktop creates and starts it without a CLI initialization step.
- `xpressclaw up` discovers or creates it and opens first-run setup.
- `xpressclaw init` remains optional for provisioning and advanced alternate
  instances; it never means “add this repository.”

New explicitly initialized instances write instance-local data and managed
workspace paths. Their detached logs and PID files also live under the
instance directory. Operators may use alternate instances for a concrete
independent state, credential/security, environment, testing, or remote-host
boundary, and must give concurrently running instances distinct ports.

The CLI names this advanced selector `--instance`. `--workdir` remains a
deprecated alias, explicit `init <PATH>` remains valid, and a current-directory
`xpressclaw.yaml` remains discoverable when no default instance is configured.
Existing YAML data paths are not silently migrated. Bare `init` now targets the
default instance; `init .` preserves the former current-directory behavior and
is the migration for scripts that depended on it.

### Repositories are workspaces, not instances

A repository becomes useful to XpressClaw when it is selected as an Agent's
workspace in the UI. Repository-local harness instructions—such as
`AGENTS.md`, `CLAUDE.md`, skills, hooks, or equivalent native product files—stay
with the repository and remain owned by the harness. Machine-specific image,
mount, credential, and container-engine settings stay in the instance.

The optional `.xpressclaw.yml` file remains a pointer for explicit portable
Project synchronization and is distinct from `xpressclaw.yaml`. A future
portable Agent-template format may share safe runner definitions, but it must
separate repository intent from host paths and credentials. This ADR does not
invent that format or duplicate sync-init discovery work.

### Clients are disposable; the instance is durable

Desktop windows and browsers are **clients**. Client disconnect, sleep, or
closure does not stop queued or running work. Reconnecting to the same instance
loads its durable state and resumes polling/event streams. A server process
restart is a separate lifecycle event: interrupted work is recovered into the
queue and workflow bookkeeping resumes on startup.

A future client-side **connection profile** will identify an instance URL and
protected credential. “Profile” is not another name for a server directory.
Desktop currently starts one local sidecar, enforces one app process, and
routes every window to it. Native local/remote profile switching and
per-window profile binding are intentionally deferred until authentication and
credential storage exist.

### Remote access is loopback-safe by default

The user-facing server binds to loopback and does not emit permissive
cross-origin access headers. Because Linux container host gateways cannot
reach that loopback listener, the server also opens a separate ephemeral
runner callback listener. Every callback request requires a random per-process
capability injected only into bundled runner MCP processes; the callback port
is neither stable nor a client connection surface.

Remote clients use an SSH tunnel or an authenticated TLS reverse proxy that
keeps the user-facing XpressClaw backend on loopback. A non-loopback CLI bind
requires the explicit `--allow-insecure-remote` acknowledgement because
XpressClaw has no built-in user authentication today. The acknowledgement
changes no security property and is intended only for deployments protected by
another complete access-control layer.

## Consequences

- Basic Desktop and CLI onboarding no longer exposes a directory choice or
  requires `init`.
- Documentation and settings can explain one coherent hierarchy: client →
  instance → Project → Agent → repository workspace.
- Existing `--workdir`, explicit init paths, and current-directory control
  planes continue to work while new help and warnings guide users to
  `--instance`.
- Newly initialized alternate instances are actually independent by default;
  existing installations keep their prior database locations until an
  operator deliberately changes them.
- The previous implicit LAN exposure is removed. Direct remote access remains
  an advanced, externally secured deployment rather than a misleadingly safe
  default.
- Desktop remote profiles, per-window instance selection, application-level
  authentication, and portable Agent templates remain necessary follow-up
  work. They should land together with explicit threat models and migrations,
  not as URL fields that imply security the server does not provide.
