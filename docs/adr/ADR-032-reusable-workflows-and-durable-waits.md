# ADR-032: Reusable Workflows and Durable Event Waits

## Status

Accepted

## Context

The first executable workflow UI saved concrete agent IDs on every task step.
That made a definition a project-specific macro rather than a reusable
workflow. It also advanced only when an XpressClaw task completed. A realistic
code-review workflow must be able to publish a pull request, release its worker,
wait hours or days for human activity, and resume after a restart.

The editor also exposed asynchronous task steps inside foreach loops even
though the engine stopped after the first iteration.

## Decision

### Agent roles are typed inputs

The `agent` workflow input type represents a run-time role. Steps reference a
role with `agent: "@role"`; manual, New Work, and scheduled runs bind the role
to a configured agent ID. One role may be `primary`, connecting it to the main
Agent picker. Concrete IDs remain valid for deliberately fixed workflows.

Using the existing input/default/schedule system keeps validation and persisted
run payloads in one place. It also permits cross-project orchestration without
silently requiring every role to share a workspace.

For code review, a draft pull request is the handoff between the implementer
and reviewer. The roles can use separate workspaces, while both project
contexts remain scoped to the repository containing that pull request.

### Instances pin their definition

Each instance stores the workflow YAML it started with. Task completion,
restart recovery, and long-lived waits use that immutable snapshot rather than
the workflow's latest editable record. Existing pre-migration runs fall back to
the current record.

### Waits are persisted step executions

A `wait` step records an event name, rendered resource, bound repository
context, start cursor, and optional timeout in `workflow_step_executions`. The
instance enters a distinct `waiting` state while no task or container is
active. A background runner polls waits and atomically changes a matching
execution from `waiting` to `resuming`, persisting the event before advancing.
Startup recovery replays `resuming` executions and advances waits whose event
was committed immediately before a crash.

The first event provider is project-scoped GitHub pull-request activity:
formal reviews, conversation comments, and inline review comments. It reuses
the native worker's repository/credential discovery and rejects resources from
a different repository. This is an event provider for the generic wait step,
not a revival of the disabled connector/channel runtime.

Timeouts either fail the instance or follow an explicit goto target.

### Foreach cursors are durable

Loop state now stores the item collection, item index, nested step index, loop
variable, and parent execution ID. Every cursor is persisted before an agent
task is queued. Task completion advances the cursor until all items and nested
steps have run serially. Unsupported nested control-flow blocks are rejected
instead of being silently skipped.

## Consequences

- One workflow definition can be reused across any configured project agents.
- Editing a definition cannot redirect an already-running instance.
- Code review can span automated review, PR publication, human delay, feedback
  handling, and another review cycle without an agent polling in a container.
- Scheduled workflows must provide required agent roles through defaults or
  schedule inputs.
- GitHub polling is eventually consistent. It starts at a short interval and
  persists an exponential backoff capped at five minutes, avoiding needless
  API traffic during long human waits. It depends on the bound project having
  discoverable scoped GitHub access.
- Execution remains serial. Parallel fan-out and additional event providers
  require explicit future implementations rather than overloaded task steps.
