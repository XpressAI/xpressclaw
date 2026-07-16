# ADR-026: Agent Client Protocol as the Runner Boundary

## Status

Accepted

## Context

ADR-025 moved task execution out of Xpressclaw's bespoke agent layer and into
Codex, Claude Code, OpenCode, and custom products. Its first implementation
still invoked each CLI in a different non-interactive mode and parsed each
product's private JSON event format. Adding a backend therefore required a new
command builder, event decoder, session-ID extractor, completion heuristic,
permission convention, and set of UI exceptions.

That is the wrong extension boundary. The Agent Client Protocol (ACP) already
defines bidirectional JSON-RPC messages for initialization, session creation
and continuation, prompts, plans, tool activity, agent messages, permissions,
and completion. Several agent products either expose ACP directly or have a
maintained adapter in the ACP Registry.

## Decision

Xpressclaw is an ACP client. An agent runner is any container process that
speaks ACP over stdin/stdout.

The built-in images start these servers:

- Codex: the ACP Registry Codex adapter;
- Claude Code: the ACP Registry Claude adapter;
- OpenCode: its built-in `opencode acp` server.

A custom runner supplies an explicit image and argv. Its process has the same
protocol contract as a built-in runner; Xpressclaw does not add a
provider-specific event parser. The only command placeholder is `{workspace}`
because task text is delivered as an ACP `session/prompt`, not interpolated
into a shell command.

An optional per-project model preference is applied with
`session/set_config_option` after creating or resuming the ACP session. The
agent advertises the model selector and its valid values; Xpressclaw does not
maintain provider-specific model lists or translate the preference into a CLI
flag. An invalid value fails with the choices reported by that agent.

### Session lifecycle

Each work attempt starts a short-lived, attached container. Xpressclaw performs
the ACP handshake, then creates a session for fresh work or continues a stored
ACP session. It prefers `session/resume` when the agent advertises that
capability and otherwise uses `session/load`. The returned ACP session ID is
stored on the attempt and selected using the precedence from ADR-025:
dependency, earlier turn on the same task, explicit fresh conversation, then
the project's latest conversation.

The container remains disposable. Session durability belongs to the agent's
mounted data directory and the ACP session identifier. Custom images that need
durable state must declare an explicit volume.

### Events and tasks

ACP session updates are the source of user-visible activity:

- agent message chunks form the result and conversation reply;
- thought chunks are visible activity, not hidden provider logs;
- tool calls and updates become technical steps;
- ACP plans are synchronized into task subtasks;
- usage and mode changes become structured events;
- the complete protocol transcript is retained as a diagnostic artifact.

A task is complete after the prompt has returned and every extracted subtask
is complete. If the response asks for user input, the task waits and a later
reply resumes the same ACP session.

### Permissions

The initial client chooses an affirmative permission option when an ACP agent
requests approval. This preserves the previous autonomous-worker behavior and
is limited by the attempt container, workspace mounts, network policy, and
resource limits. Permission requests and the selected option are recorded.
A configurable interactive/autonomous permission policy is future work.

### Compatibility

Existing Rust type names and the `native_session_id` database column remain
temporarily for data compatibility. Their values now describe ACP-backed
projects and ACP session IDs. User-facing configuration and UI call the
boundary ACP and no longer expose native-CLI turn limits or prompt command
templates.

## Consequences

### Positive

- A new backend only needs an ACP server image and command.
- Plans, tool activity, permissions, session continuation, and results share
  one implementation across products.
- Provider CLI output changes no longer require Xpressclaw parser changes.
- The task UI can present semantic events without exposing terminals.
- Agent products retain ownership of reasoning, tools, subagents, and context.

### Negative

- Products without native ACP support require an adapter.
- The control plane must keep a bidirectional container attachment open for a
  turn instead of consuming one-way logs after process exit.
- Session continuation depends on the server implementing `session/resume` or
  `session/load` and retaining the referenced session state.
- Automatically approving permissions is suitable only for trusted images and
  deliberately scoped worker environments.

## Related ADRs

- ADR-003: Container isolation
- ADR-020: Task dependencies
- ADR-022: Connectors and workflows
- ADR-025: Event-driven sessions and task-multiplexer UX
