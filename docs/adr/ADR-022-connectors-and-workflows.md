# ADR-022: Connectors and Workflows

## Status
Accepted

## Context

Xpressclaw agents are isolated — they can execute tasks and have conversations, but they can't interact with the outside world or orchestrate work across multiple agents in a structured way. Two capabilities are needed:

1. **External connectivity**: Agents need to receive messages from Telegram, respond to events, watch filesystems, and send notifications.
2. **Multi-agent orchestration**: There's no way to define "when X happens, agent A does Y, then agent B does Z, and if it fails, go back to A."

## Decision

### Connectors

Connectors are external integrations that provide **channels** — named endpoints for receiving events (sources) and sending messages (sinks).

#### Two Modes of Use

**Direct agent binding**: A channel is bound to an agent via `connector_channels.agent_id`. Incoming messages are injected into a conversation with that agent. Agent responses are sent back through the channel. No workflow needed.

**Workflow trigger/sink**: Unbound channels emit events to the `connector_events` table. The workflow engine matches events against workflow triggers and starts instances. Sink steps deliver messages through connectors.

#### Connector Types

| Type | Source | Sink | Status |
|------|--------|------|--------|
| Webhook | HTTP POST received | HTTP POST to URL | Full |
| Telegram | Bot API long polling | sendMessage API | Full |
| File Watcher | Filesystem events (notify) | Write file | Full |
| Email | IMAP | SMTP | Stub |
| GitHub | Webhook receiver | API calls | Stub |
| Jira | Webhook receiver | API calls | Stub |
| Slack | Bot events | Web API | Stub |

### Workflows

Workflows are a **task templating system** — each step creates a task assigned to an agent with either an ad-hoc prompt or a procedure (SOP). The definition is a YAML file that can be shared and version-controlled.

#### Block-Based Editor

The editor uses a linear block-based approach (not a node graph). Blocks stack vertically like steps in a recipe. This was chosen over a free-form canvas because:
- Reads naturally top-to-bottom
- Simpler mental model
- Works on all platforms (no WebKit DnD issues)
- Components are reusable in the procedure editor

#### Sub-workflows

A workflow consists of multiple **flows** (sub-workflows), each with its own tab:
- `main` (green) — primary flow, starts with a trigger
- `on_error` (red) — error handling, entered automatically on failure
- Custom flows (e.g. `on_rejected`) — user-defined, entered via jumps

Each flow is a linear sequence of steps. Flows share a common variable namespace.

#### Step Types

| Type | Description |
|------|-------------|
| **step** | Task: agent executes a prompt or procedure, produces typed outputs |
| **when** | Conditional: switch on a variable, match arms with goto/continue |
| **loop** | For-each: iterate over an array variable, execute nested steps per item |
| **sink** | Notification: deliver messages through connectors |
| **jump** | Control flow: jump to another flow, step, or workflow |

#### Variables and Outputs

Each task step declares **output variables** with types and descriptions. When a task completes, the agent's response is parsed as JSON and the declared fields are extracted into the workflow's **variable store**. Downstream steps reference variables via `@step_id.field` syntax.

Global workflow variables (declared in the YAML) enable accumulator patterns across loops and sub-workflows.

The `@` mention popup in the editor shows available variables from preceding steps, the trigger payload, global variables, and loop variables.

#### YAML Format

```yaml
name: support-intake
version: 1

trigger:
  connector: telegram
  channel: support-intake
  event: message_received

variables:
  escalation_count: 0

flows:
  main:
    color: "#22c55e"
    steps:
      - id: classify
        type: step
        label: "Classify Intent"
        agent: router-sm
        prompt: "Classify: @trigger.payload.text"
        outputs:
          intent: { type: string, description: "Classified intent" }
          entities: { type: array, description: "Extracted entities" }

      - id: process
        type: loop
        label: "Process Entities"
        over: "@classify.entities"
        as: entity
        steps:
          - id: enrich
            type: step
            label: "Enrich"
            agent: enricher
            prompt: "Enrich: @entity"

      - id: route
        type: when
        label: "Route by Intent"
        switch: "@classify.intent"
        arms:
          - match: "complaint"
            goto: flow on_rejected
          - match: default
            continue: true

      - id: notify
        type: sink
        label: "Notify"
        sinks:
          - connector: telegram
            channel: ops
            template: "Done: @classify.intent"

  on_error:
    color: "#ef4444"
    steps:
      - id: log_err
        type: step
        label: "Log Error"
        agent: logger
        prompt: "Log: @error"

  on_rejected:
    color: "#f97316"
    steps:
      - id: escalate
        type: step
        label: "Escalate"
        agent: escalator
        prompt: "Escalate case"
      - id: back
        type: jump
        label: "Back to main"
        target: flow main step notify
```

#### Execution Model

The workflow engine walks `flows[flow].steps[index]` sequentially:

1. **step**: Render prompt (append output schema), create task, enqueue for dispatcher. Wait for completion. Extract JSON outputs into variable store.
2. **when**: Resolve switch variable, match arms. Execute goto (step/flow/workflow) or continue to next step.
3. **loop**: Resolve variable to array, iterate, execute nested steps per item.
4. **sink**: Deliver messages through connectors, advance.
5. **jump**: Switch to target flow/step or start a new workflow instance.

If a step fails and `on_error` flow exists, execution jumps there automatically.

#### Crash Recovery

On server startup, the engine loads running instances and checks if the current step's task has completed. If so, it resumes execution.

### Database Schema

Six tables (connectors unchanged from initial design):

- `connectors` — connector definitions
- `connector_channels` — channels with optional agent binding
- `connector_events` — event log for workflow trigger matching
- `conversation_channel_bindings` — channel → conversation mapping for direct binding
- `workflows` — workflow definitions (YAML content)
- `workflow_instances` — running instances with current flow/step/variables
- `workflow_step_executions` — per-step execution history

### MCP Tools

Agents can create and manage workflows programmatically via MCP tools:
- `create_workflow`, `update_workflow`, `list_workflows`, `run_workflow`
- `create_procedure` — procedures share editing concepts with workflows

### Component Reuse

The block editor components (StepBlock, WhenBlock, LoopBlock) are designed to be reusable in the procedure editor, since procedures are a similar sequential step model within a single task.

## Consequences

### Positive
- Block editor is intuitive — reads like a recipe
- Sub-workflows handle complex error/rejection flows cleanly
- Variables with `@` mention provide good DX for prompt authoring
- Reusable components reduce future work on procedure editor
- Agents can create workflows, enabling meta-automation
- YAML files are portable and version-controllable

### Negative
- Sequential model can't express arbitrary parallel execution
- Loop execution is synchronous (one item at a time)
- Stub connectors (email, GitHub, Jira, Slack) need future implementation

### Risks
- LLM output parsing for structured variables is best-effort
- Complex goto patterns could create confusing execution flows
