# ADR-037: Project Collaboration and Concurrent Conversations

## Status

Accepted

## Context

The durable object historically called an Agent accumulated several unrelated
responsibilities: it was simultaneously a workspace, container, harness,
memory boundary, task queue, and navigation category. That model works for a
single coding harness, but it cannot naturally represent a Project in which
several specialized Agents participate in several ongoing Conversations.

Task timelines are also the wrong interaction surface for lightweight
coordination. A task occupies an Agent's serialized work lane and is optimized
for an auditable outcome. People still need to ask a quick question while that
task is running, let several Agents discuss shared context, turn a message into
durable work, and receive a result or file back in the place where the work was
requested.

ADR-019 anticipated background Conversations, but subsequent ACP and retained
runtime work replaced its direct-LLM implementation assumptions. ADR-030
intentionally deferred channels and kept the older Agent-first hierarchy.

## Decision

### Projects are the collaboration boundary

The top-level product object is a **Project**. A Project contains:

- zero or more **Agents**;
- zero or more **Conversations**; and
- zero or more **Tasks**.

Project memory belongs to the Project, not to an Agent session. An Agent belongs
to one Project and may participate in any number of that Project's
Conversations. A Conversation may contain any number of the Project's Agents.
Tasks retain one Project owner and may optionally link to the Conversation that
created them. Cross-Project Agent assignment is rejected.

An Agent remains the execution identity: it owns a harness configuration,
workspace, retained container, task lane, and durable ACP task sessions. A
Project does not imply one shared container or filesystem. This preserves
isolation between specialized Agents while giving them an explicit shared
context above those runtimes.

### Conversations have independent execution lanes

Each `(Conversation, Agent)` pair has a durable session record and queued turn
stream. Its ACP process runs in the Agent's retained container but is separate
from the Agent's task ACP process. Conversation responses therefore remain
available while the same Agent is executing a long task. Different
Conversations for one Agent also retain independent ACP sessions.

An unmentioned user message addresses every participating Agent. A message
with Agent mentions addresses only those Agents. Agent messages do not
automatically wake peers; an Agent must mention a peer explicitly. This avoids
unbounded reply loops while still permitting visible Agent-to-Agent
coordination. Multiple messages waiting for one Agent are coalesced into the
next turn, and messages arriving during a running turn queue a follow-up.

Conversation turns and session identifiers are persisted. Running turns return
to the queue after a control-plane restart. Removing a participant atomically
cancels that Agent's queued or running turns, and a late response from a
cancelled turn cannot be published. Message storage and addressed-Agent queue
updates share one database transaction, so a crash cannot strand a visible
message without the turns needed to deliver it.

### Messages connect coordination to durable work

Conversation messages may include up to ten attachments with a combined
decoded size of 20 MiB. Attachments are stored atomically with the message.
Agents receive tools to:

- publish a message and files to a Conversation;
- download a published attachment into their workspace; and
- create a linked Task for themselves from the Conversation.

People can use **Continue with task** to create a linked Task for a Project
Agent. Completing that Task publishes its result back to the Conversation.
The Task keeps its normal detailed timeline and remains linked from the
Conversation, so coordination stays readable without hiding execution detail.

### Workflows run in Project and Conversation context

Workflow definitions remain reusable. A run may bind to a Project and an
optional Conversation. Every Agent role in a Project-bound run must resolve to
an Agent in that Project. Tasks created by the run inherit both scopes, and
their results return to the bound Conversation. A Conversation can start a
workflow with its typed inputs from the same **Continue with task** surface.

### Existing installations migrate without discarding work

Migration initially creates one Project for each existing Agent, preserving
the previous visible grouping. Legacy Conversations that already connect
several Agents define connected components; their Agents, Tasks, Conversations,
Project memory, and vector index entries are consolidated into one Project.
Empty intermediate Projects are removed. Existing Agent IDs, task histories,
ACP sessions, workspaces, and retained containers remain valid.

The primary sidebar now presents Projects, their Conversations, and their
Agents as one hierarchy. Tasks remain a global operational view and are grouped
by Project. Project pages provide the local overview and creation paths.

## Consequences

- People can coordinate several Agents without turning every message into a
  serialized task or waiting for active task work to finish.
- Conversation history becomes the shared human-readable context; Tasks remain
  the durable unit of execution and audit.
- Project memory can be used consistently by every Agent in the Project.
- Each additional active Conversation/Agent pair may add an idle ACP process
  inside an already-retained container. It consumes some memory but no task
  queue lease and should consume negligible CPU while idle.
- Attachments increase the local SQLite database size. The per-message limits
  bound accidental growth; larger artifact storage can be introduced later
  without changing message semantics.
- Agents cannot currently span Projects. Sharing one execution identity across
  separate memory and security boundaries would require an explicit future
  identity model rather than an accidental many-to-many relationship.
- This ADR supersedes the Agent-first hierarchy in ADR-030 and the execution
  design in ADR-019. It retains ADR-033's per-Agent container and process
  lifecycle for task work.
