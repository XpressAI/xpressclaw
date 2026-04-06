# ADR-022: Connectors and Workflows

## Status
Proposed

## Context

Xpressclaw agents are isolated — they can execute tasks and have conversations, but they can't interact with the outside world or orchestrate work across multiple agents in a structured way. Two capabilities are missing:

1. **External connectivity**: Agents need to receive messages from Telegram, respond to GitHub events, watch filesystems, and send notifications. Currently the only way to talk to an agent is through the web UI.

2. **Multi-agent orchestration**: There's no way to define "when X happens, agent A does Y, then agent B does Z, and if it fails, go back to A." The task dependency system (ADR-020) handles DAGs but not cycles, conditional branching, or event-triggered chains.

## Decision

### Connectors

Connectors are external integrations that provide **channels** — named endpoints for receiving events (sources) and sending messages (sinks).

#### Connector Types

| Type | Source | Sink | Implementation |
|------|--------|------|----------------|
| Webhook | HTTP POST received | HTTP POST to URL | Full |
| Telegram | Bot API long polling | sendMessage API | Full |
| File Watcher | Filesystem events via `notify` | Write file | Full |
| Email | IMAP polling | SMTP send | Stub (future) |
| GitHub | Webhook receiver | API calls | Stub (future) |
| Jira | Webhook receiver | API calls | Stub (future) |
| Slack | Bot events | Web API | Stub (future) |

#### Two Modes of Use

**Direct agent binding**: A channel is bound to an agent via `connector_channels.agent_id`. Incoming messages are injected into a conversation with that agent. Agent responses are sent back through the channel. This reuses the entire conversation system — no workflow needed.

```
Telegram message → Conversation → Agent Session → Response → Telegram reply
```

**Workflow trigger/sink**: Unbound channels emit events to the `connector_events` table. The workflow engine polls for unprocessed events and starts matching workflow instances. Sink nodes deliver messages through connectors.

```
Jira ticket → connector_events → Workflow Engine → Tasks → Sink → Telegram + Email
```

#### Connector Trait

```rust
#[async_trait]
pub trait Connector: Send + Sync {
    fn connector_type(&self) -> &str;
    async fn validate_config(&self, config: &Value) -> ValidationResult;
    async fn start(&mut self, config: &Value, channels: &[ChannelConfig],
                   event_tx: mpsc::Sender<ConnectorEvent>) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    async fn send(&self, message: &SinkMessage) -> Result<()>;
    async fn health(&self) -> bool;
}
```

The `ConnectorRegistry` holds live connector instances and manages their lifecycle. On server startup, it starts all enabled connectors. Events flow through an `mpsc` channel to a central handler that either injects into conversations (direct binding) or records to `connector_events` (workflow trigger).

### Workflows

Workflows are visual state machines that chain tasks across agents. Each node is a task template; each edge is a conditional transition.

#### Definition Format (YAML)

```yaml
name: code-review-pipeline
description: Jira ticket through PM, Dev, Test cycle
version: 1

trigger:
  connector: jira
  channel: my-project
  event: issue_created
  filter:
    type: Story

nodes:
  - id: spec
    label: "Write Specification"
    agent: pm-agent
    prompt: |
      Write a spec for: {{trigger.payload.summary}}
    position: { x: 250, y: 0 }

  - id: implement
    label: "Implement Feature"
    agent: dev-agent
    prompt: |
      Implement: {{nodes.spec.output}}
    position: { x: 250, y: 150 }

  - id: test
    label: "Run Tests"
    agent: tester-agent
    procedure: run-tests
    position: { x: 250, y: 300 }

edges:
  - from: spec
    to: implement
    condition: completed

  - from: test
    to: implement
    condition: output.verdict == "fail"
    label: "Tests Fail"
```

Workflows explicitly support cycles (test → implement above). The `attempt` column in `workflow_node_executions` tracks revisits, with a configurable max (default 10) to prevent infinite loops.

#### Condition Language

Simple expressions evaluated against task output:
- `completed` / `failed` — task status shorthand
- `output.field == "value"` — JSON path comparison
- `output.field contains "text"` — substring match
- `default` — catch-all fallback

Not a full expression language — deliberately minimal. Covers the stated requirements (approve/reject cycles, output-based branching).

#### Integration with Task System

Workflows do NOT replace the task dispatcher. Each workflow node creates a real task via `TaskBoard::create()` + `TaskQueue::enqueue()`. The existing dispatcher processes it normally. Agents don't need to know they're in a workflow.

After a task completes, the dispatcher calls the workflow engine:

```rust
// In evaluate(), after task completion:
if let Some(exec) = engine.find_execution_by_task(&task.id) {
    engine.on_task_completed(&exec.instance_id, &exec.node_id, &result);
}
```

The engine records the output, evaluates outgoing edge conditions, and creates the next task(s).

#### Context Passing

Each node's output (the last assistant message from its task) is stored in `workflow_node_executions.output`. When building the prompt for the next node, the engine renders `{{nodes.spec.output}}` templates by looking up completed node executions.

#### Crash Recovery

On startup, the engine loads all `workflow_instances` with `status = 'running'`. For each, it checks if the current node's task completed but wasn't advanced (server crashed between task completion and engine advancement). If so, it resumes by calling `on_task_completed()`.

### Visual Editor

The workflow editor at `/workflows/[id]` uses @xyflow/svelte (Svelte Flow) with custom node types:
- **TaskNode**: agent avatar, label, conditional output handles
- **TriggerNode**: connector icon, channel name
- **SinkNode**: sink connector icons

Definitions are saved as YAML — the graph is the source of truth, and node positions are persisted in the YAML for editor layout.

## Database Schema

Six new tables in migration V21:

- `connectors` — connector definitions (type, config, status)
- `connector_channels` — channel instances with optional `agent_id` for direct binding
- `connector_events` — event log for workflow matching
- `workflows` — workflow definitions (YAML content)
- `workflow_instances` — running instances with current state
- `workflow_node_executions` — per-node execution history with task linkage

## Consequences

### Positive
- Agents can interact with the outside world without custom code
- Multi-agent workflows with cycles handle real-world processes (approval loops)
- Direct agent binding provides zero-config Telegram/Slack bots
- YAML definitions are shareable and version-controllable
- Crash recovery ensures workflows survive server restarts
- Visual editor makes workflow creation accessible to non-developers

### Negative
- Connector stubs (email, GitHub, Jira, Slack) need future implementation
- Simple condition language may be limiting for complex branching logic
- Long-polling connectors (Telegram) consume a thread per connector
- Workflow YAML format is custom (not an industry standard like BPMN)

### Risks
- Telegram Bot API rate limits may throttle high-volume channels
- Filesystem watcher may miss events during brief disconnections
- Cyclic workflows with loose conditions could loop unexpectedly (mitigated by max_cycles)
