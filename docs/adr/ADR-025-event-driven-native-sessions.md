# ADR-025: Event-Driven Sessions with Native Workers

## Status

Accepted

## Context

XpressClaw originally supplied its own agent layer: a persistent Python
harness, an LLM router, tool-call conventions, and a desired-state controller
that kept one container alive per agent. That put the project in competition
with the rapidly improving agent products people already choose to use, such
as Codex, Claude Code, and OpenCode.

The useful part of XpressClaw is elsewhere:

- a durable task queue with dependencies;
- schedules, connectors, and workflow triggers;
- multi-agent workflow coordination;
- isolated devices and workspaces, including the Android capability described
  by the in-progress ADR-024 work;
- a UI where a person can understand and steer work without watching a shell.

A persistent native agent context also conflates two things. The user wants
one stable identity and history, but a long-running coding task should not make
that history unavailable for fifteen minutes. Identity belongs to the control
plane; execution contexts belong to workers.

## Decision

### One logical session, many work attempts

Each configured profile has one durable `logical_session`. Messages from a
person, task, schedule, connector, or workflow are appended to its event log.
The session remains writable while work is queued or running.

Executable work is represented by a `work_attempt`. An attempt records:

- the task and queue item that caused it;
- provenance and kind (`interactive`, `scheduled`, `workflow`, or `task`);
- native runner, native session ID, and ephemeral container ID;
- lifecycle (`queued`, `preparing`, `running`, `waiting_for_input`, `review`,
  `completed`, `failed`, or `cancelled`);
- structured results, errors, and artifacts.

The UI renders these records as activity, status, results, questions, and
review decisions. It does not expose an interactive terminal or raw container
logs. A captured native event stream is retained as a diagnostic artifact but
is deliberately excluded from the primary artifact view.

### Native products own the agent loop

The durable task queue is consumed by short-lived native CLI workers. The
initial adapters are:

- Codex via `codex exec --json`;
- Claude Code via `claude -p --output-format stream-json`;
- OpenCode via `opencode run --format json`;
- a configurable command adapter for other products.

XpressClaw constructs context, launches the worker, parses semantic events,
and records outcomes. It does not implement model turns, tool selection, or
context compaction. The old Python harness dispatcher and per-agent
desired-state reconciler are no longer started by the server.

Each native product has its own minimal image. Codex, Claude Code, and
OpenCode are not bundled together. This makes the selected product and its
credential boundary explicit, keeps downloads smaller, and allows each image
to be versioned independently.

Attempts for the same logical session are serialized initially because they
share a mounted workspace. The API still accepts messages immediately and
shows them as queued. Isolated Git worktrees or remote sandboxes can later
allow safe parallel attempts without changing the session model.

### Agent runners and development environments are separate resources

The native runner image contains the selected agent product and only the
small set of utilities needed to operate it. Java, .NET, Ruby, databases,
browsers, and other project dependencies do not define the agent and should
not accumulate in that image.

A richer development environment is instead a control-plane-managed resource
that can be created, leased to an attempt, and destroyed independently. The
runner receives a scoped way to edit the shared workspace and execute tools in
that environment. It does not receive the host Docker socket or unrestricted
container-daemon credentials. The same resource model can later cover Android
devices, desktops, browsers, and remote machines.

The first implementation still bind-mounts the configured workspace directly
into a runner. Custom runner images remain supported as an escape hatch until
the scoped development-environment gateway is implemented. We explicitly do
not use a Docker-socket mount as an interim solution because it would turn a
worker compromise into control over every host container and mount.

### Subscription authentication

When `runner.subscription_auth` is enabled, a worker mounts the standard host
login directory for its selected CLI:

- `~/.codex` for Codex;
- `~/.claude` and `~/.claude.json` for Claude Code;
- `~/.local/share/opencode` and `~/.config/opencode` for OpenCode.

This permits a user to sign in once with the native CLI and reuse the eligible
subscription inside workers. The mount is writable because OAuth credentials
may be refreshed. Only trusted worker images should be used. A host-side
credential proxy is preferred future work because it can keep bearer tokens
out of the container entirely.

### Tasks and workflows are messages with lifecycle

Manual tasks, schedules, workflow steps, connector events, and session
messages all converge on the existing durable task queue. Enqueueing creates a
work attempt and a provenance-rich session event. Existing scheduling and
workflow semantics therefore survive the runtime pivot.

Workflows remain the coordination mechanism for multiple native products. A
code workflow can, for example, alternate a Codex implementation step and a
Claude review step until an approval condition is satisfied, then run a PR
publication step. That policy belongs in the workflow definition, not in a
new general-purpose agent loop.

### Devices are resources

Android emulators, physical phones, browsers, and desktops are capabilities
assigned to an attempt. They are not special agent types. Their structured
events and artifacts join the same session timeline. This lets the Android
work land behind the native-worker boundary without coupling it to a Python
MCP harness.

## Consequences

### Positive

- Users can choose the native agent product that best fits each workflow step.
- The project differentiates on orchestration, isolation, devices, and UX
  instead of maintaining another agent framework.
- Session history remains coherent even though native execution contexts are
  disposable.
- Schedules, connectors, task dependencies, and workflows share one dispatch
  path and one audit model.
- The UI can be designed around intent, progress, evidence, and decisions
  rather than terminal multiplexing.

### Negative

- The selected native runner image must be built or supplied before work can execute.
- Mounting host OAuth state into a container is a meaningful trust boundary.
- The first implementation serializes attempts per session, so an interactive
  message can be queued behind current work even though the session UI remains
  responsive.
- Existing conversation and legacy harness code remains temporarily for data
  compatibility, but it is no longer the server's task execution path. It can
  be deleted after configuration and CLI migration are complete.

## Migration

Schema migration 24 creates `logical_sessions`, `session_events`,
`work_attempts`, and `attempt_artifacts`, links tasks and queue items to their
attempts, and converts queued legacy work.

Existing backends map as follows unless `runner.kind` is set explicitly:

- names containing `codex` -> Codex;
- names containing `claude` (including `claude-sdk`) -> Claude Code;
- names containing `opencode` -> OpenCode;
- other names require `runner.command`.

Build the runner images you use with:

```bash
docker build -t xpressclaw-runner-codex:latest harnesses/native/codex
docker build -t xpressclaw-runner-claude:latest harnesses/native/claude
docker build -t xpressclaw-runner-opencode:latest harnesses/native/opencode
```

Then sign in on the host with the selected CLI. API-key based LLM router
configuration remains readable during migration but is not used by native
task workers.

When subscription authentication is enabled, the control plane bind-mounts
the selected CLI's host login directory writable so the CLI can refresh OAuth
credentials. It also exposes `.gitconfig` and GitHub CLI auth read-only when
present. SSH keys are never mounted implicitly. This is a deliberate trust
boundary: use only runner images you control or have audited.

## Related ADRs

- ADR-002: Agent Backend Abstraction (superseded for task execution)
- ADR-003: Container Isolation (retained, changed to per-attempt containers)
- ADR-018: Desired-State Controller (superseded for agent execution)
- ADR-019: Background Conversations (event durability retained)
- ADR-020: Task Dependencies (retained)
- ADR-021: Agent Sessions / Actor Model (superseded)
- ADR-022: Connectors and Workflows (retained)
- ADR-024: Android device capability work (designed to attach as a resource)
