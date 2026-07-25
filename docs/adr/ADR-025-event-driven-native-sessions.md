# ADR-025: Event-Driven Sessions with Native Workers

## Status

Partially superseded by ADR-026

The runtime adapter section is superseded by ADR-026. The project/task UX,
short-lived attempt isolation, and development-environment decisions remain
in force.

## Context

XpressClaw originally supplied its own agent layer: a persistent Python
harness, an LLM router, tool-call conventions, and a desired-state controller
that kept one container alive per agent. That put the project in competition
with the rapidly improving agent products people already choose to use, such
as Codex, Claude Code, and OpenCode.

The useful part of XpressClaw is elsewhere:

- a durable task queue with dependencies;
- schedules and workflow triggers, with connector ingestion deferred until the
  lifecycle and routing requirements in ADR-022 are met;
- multi-agent workflow coordination;
- isolated devices and workspaces, including the Android capability described
  by the in-progress ADR-024 work;
- a UI where a person can understand and steer work without watching a shell.

A persistent native agent context also conflates two things. The user wants
one stable identity and history, but a long-running coding task should not make
that history unavailable for fifteen minutes. Identity belongs to the control
plane; execution contexts belong to workers.

## Decision

### One project, one active native conversation, many task branches

Each configured workspace/runner pair is presented as a project and backed by
one durable `logical_session`. Messages from a person, task, schedule, or
workflow become durable tasks. A task inherits the project's active native
Codex, Claude Code, or OpenCode context as its own conversation branch when
the runner supports it. The project remains writable while work is queued or
running. Future connector messages must use the same path once that runtime is
restored.

The native conversation is resumed across short-lived worker containers. A
task can explicitly request a fresh conversation. A dependent task branches
from the native conversation of its prerequisite when that prerequisite used
the same project and runner. A follow-up message on the currently active task
resumes its branch. Reopening an older task forks from that task's saved branch
so new work does not rewrite its earlier context. The precedence is therefore:

1. an earlier turn on the same task;
2. the conversation of a dependency;
3. a requested fresh conversation;
4. the project's most recent conversation.

If a runner does not support conversation forks, XpressClaw continues the
selected source conversation as a compatibility fallback and records that
fallback in the task timeline.

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
that environment. The same resource model can later cover Android devices,
desktops, browsers, and remote machines.

The first implementation still bind-mounts the configured workspace directly
into a runner. Custom runner images remain supported as an escape hatch until
the scoped development-environment gateway is implemented. ADR-027 later adds
an explicit trusted host-engine mode for projects whose existing build and test
workflows depend on Docker Compose or image builds; the default remains the
minimal runner without daemon access.

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

Manual tasks, schedules, workflow steps, and session
messages all converge on the existing durable task queue. Enqueueing creates a
work attempt and a provenance-rich session event. Future connector events must
converge here as well. Existing scheduling and
workflow semantics therefore survive the runtime pivot.

Workflows remain the coordination mechanism for multiple native products. A
code workflow can, for example, alternate a Codex implementation step and a
Claude review step until an approval condition is satisfied, then run a PR
publication step. That policy belongs in the workflow definition, not in a
new general-purpose agent loop.

### The UI is a task multiplexer, not an attempt inspector

The primary interface exposes projects, tasks, steps, and automations. It does
not ask users to understand logical sessions, native session IDs, work
attempts, queue records, containers, or event protocol artifacts.

The project view is the multiplexer: it shows whether the agent is working,
queued, waiting for the user, or ready; accepts another task immediately; and
links to each durable task conversation. A task view keeps its conversation,
semantic progress, native plan steps, result, and reply composer together.
Diagnostics may retain implementation records, but they are not duplicated in
the primary work view.

Navigation must work as a remote control from a phone. Desktop keeps a project
switcher in the sidebar. Narrow viewports use an overlay project switcher and a
bottom product-area bar, leaving the full viewport width for the task
conversation and composer.

### Devices are resources

Android emulators, physical phones, browsers, and desktops are capabilities
assigned to an attempt. They are not special agent types. Their structured
events and artifacts join the same session timeline. This lets the Android
work land behind the native-worker boundary without coupling it to a Python
MCP harness.

### Native extension surface

XpressClaw treats skills, plugins, hooks, custom subagents, and project-local
instructions as native harness configuration. Enabling subscription login
mounts the native product's complete configuration directory writable so the
same extensions work in an isolated attempt and can refresh their own state.
Per-session environment values and additional volume mounts support alternate
configuration roots and extensions that live elsewhere. XpressClaw does not
translate those extensions into a second proprietary skill or agent layer.

ACP session setup carries the MCP servers enabled for that harness. ACP session
configuration options and legacy session modes are persisted as events and
rendered as controls in the harness settings, task composer, and workflow
editor. ACP available-command updates are also persisted; choosing a command
sends the ordinary `/command arguments` prompt defined by ACP. Controls chosen
in task chat are applied before the next turn. Workflow steps can set the same
opaque ACP option IDs, select a native command, request a fresh session, and
target different harnesses step by step.

ACP does not expose a client-side method that invokes an attached MCP tool.
For `mcp_server`, `mcp_tool`, and `mcp_arguments` workflow fields, XpressClaw
therefore constructs a tool-call request inside the native turn. The harness
remains responsible for permissions and execution and emits the normal ACP
tool-call activity; XpressClaw does not independently verify the call. A future deterministic control-plane MCP step
would belong to the workflow runtime rather than the ACP client.

## Consequences

### Positive

- Users can choose the native agent product that best fits each workflow step.
- The project differentiates on orchestration, isolation, devices, and UX
  instead of maintaining another agent framework.
- Session history remains coherent even though native execution contexts are
  disposable.
- Schedules, task dependencies, and workflows share one dispatch path and one
  audit model. Future connector sources must enter through that same path.
- The UI can be designed around intent, progress, evidence, and decisions
  rather than terminal multiplexing.

### Negative

- The selected native runner image must be built or supplied before work can execute.
- Mounting host OAuth state into a container is a meaningful trust boundary.
- The first implementation serializes attempts per session, so an interactive
  message can be queued behind current work even though the session UI remains
  responsive.
- Commands and control changes selected while a native turn is already running
  apply to the next turn because attempt containers and ACP connections are
  short-lived.
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
docker buildx build --load -f harnesses/native/codex/Dockerfile -t xpressclaw-runner-codex:latest -t localhost/xpressclaw-runner-codex:latest harnesses/native
docker buildx build --load -f harnesses/native/claude/Dockerfile -t xpressclaw-runner-claude:latest -t localhost/xpressclaw-runner-claude:latest harnesses/native
docker buildx build --load -f harnesses/native/opencode/Dockerfile -t xpressclaw-runner-opencode:latest -t localhost/xpressclaw-runner-opencode:latest harnesses/native
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
- ADR-022: Workflow execution retained; connector runtime deferred
- ADR-024: Android device capability work (designed to attach as a resource)
