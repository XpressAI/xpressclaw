# ADR-030: Agent and Automation UX

## Status

Accepted

## Context

XpressClaw historically called each durable work context a project and called
its replaceable ACP product an agent. This made the interface harder to explain:
the durable object owns task history, conversation state, memory, workspace,
and environment, while Codex, Claude, or OpenCode can be swapped without
replacing that context.

Schedules and multi-step workflows were also separate top-level destinations,
even though both are ways to arrange future or repeated agent work.

## Decision

The user interface calls the durable work context an **Agent**. An Agent has a
user-chosen name and displays its selected **Harness** and workspace folder as
secondary metadata. Codex, Claude, OpenCode, and other ACP implementations are
harnesses. Existing API paths, database fields, configuration keys, and other
internal `project` names remain compatible; this decision changes the product
language and information architecture, not persisted identities.

The primary navigation is Agents, Tasks, Automations, and Settings.
**Automations** is the umbrella for:

- workflows, which coordinate steps, decisions, and loops across agents; and
- schedules, which start or resume agent work at a future or recurring time.

Legacy `/workflows` and `/schedules` entry points continue to render the
combined Automations workspace so bookmarks remain useful.

A goal-loop template is a normal workflow: one agent makes a verified
increment, reports `complete` or `continue`, and a decision step may return to
the work step. The workflow engine's existing ten-cycle limit bounds this
template and prevents an accidental infinite loop.

## Consequences

Users can identify a durable agent independently from its current harness and
folder. Harness configuration has a clear home, and future harness changes do
not imply replacing task history or memory. Workflows and schedules share one
navigational model while keeping their existing execution APIs.

Channels for observing and joining multi-agent communication are complementary
to Automations but require a separate interaction and event model. They are
intentionally deferred to a later decision and pull request.
