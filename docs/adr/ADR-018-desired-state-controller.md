# ADR-018: Desired-State Reconciliation

## Status
Proposed

## Context

The current runtime uses an imperative model: "start agent" creates a container, "stop agent" removes it, and a single `status` field in the DB tracks what happened. This breaks in several ways:

1. **Stale status on crash/kill.** If the process dies while agents are "running", the DB still says "running" on restart. The UI lies.
2. **No image management.** Containers fail with 404 if the image hasn't been pulled. There's no mechanism to ensure images are up to date.
3. **No restart on failure.** If a container crashes or Docker restarts, agents stay dead. A 9B parameter model with tool access cannot be left running unsupervised with no recovery.
4. **Temporary errors are permanent.** A network blip during image pull or container start marks the agent as "error" forever.
5. **No reconciliation.** The system has no way to detect drift between what should be running and what is running.

These problems aren't specific to agents. Conversations, tasks, and future workflows all have the same shape: something should be in a certain state, and the system needs to make it so. The current code has ad-hoc state handling scattered across every feature with no shared pattern.

### What Kubernetes Gets Right

Kubernetes solves this with a reconciliation loop: you declare desired state ("I want 3 replicas of this pod"), a controller continuously compares desired vs observed state, and takes actions to converge. We need the same pattern, adapted for our single-machine context and generalized beyond containers.

## Decision

Introduce a **general desired-state reconciliation pattern** used across all stateful resources in the system: agents, conversations, tasks, images, and future workflows.

### Core Pattern: State Machine + Reconciler

Every managed resource follows the same shape:

```
┌──────────────┐        ┌──────────────┐        ┌──────────────┐
│   Desired    │        │  Reconciler  │        │  Observed    │
│   State      │───────▶│  (periodic)  │◀───────│  State       │
│  (DB)        │        │              │        │  (live query)│
└──────────────┘        └──────────────┘        └──────────────┘
       ▲                       │
       │                       │ actions
   user/system                 ▼
   actions              start, stop, pull,
                        re-queue, notify...
```

**Desired state** is persisted in the DB. It represents intent — what the user or system wants to be true.

**Observed state** is never persisted. It's derived live from the actual source of truth (Docker API, filesystem, process table, etc.) at the moment it's needed.

**The reconciler** runs periodically, compares the two, and takes the minimum action to converge.

### State Machines

Each resource type defines a state machine with explicit valid transitions. Invalid transitions are rejected, not silently ignored.

```rust
/// A state machine definition for any managed resource.
trait StateMachine {
    type State: Clone + Eq;

    /// Valid transitions from a given state.
    fn transitions(from: &Self::State) -> &[Self::State];

    /// Can this transition happen?
    fn can_transition(from: &Self::State, to: &Self::State) -> bool {
        Self::transitions(from).contains(to)
    }
}
```

#### Agent States

```
stopped ──▶ running
running ──▶ stopped
```

Two states. The user sets desired. The reconciler converges. Error and restart context lives alongside (not as states themselves) — an agent in error is still desired "running", the reconciler is just backing off.

#### Conversation States

```
active ──▶ waiting_for_agent ──▶ active
active ──▶ waiting_for_user ──▶ active
active ──▶ handed_off ──▶ active    (agent-to-agent handoff)
active ──▶ closed
```

A conversation's desired state is "active with agent X responding." The reconciler ensures the agent is running and the message has been delivered. If the agent crashes mid-response, the conversation transitions to "waiting_for_agent" and the reconciler retries when the agent is back.

#### Task States

```
pending ──▶ in_progress ──▶ completed
pending ──▶ in_progress ──▶ failed ──▶ pending  (retry)
pending ──▶ cancelled
in_progress ──▶ blocked ──▶ in_progress
in_progress ──▶ waiting_for_input ──▶ in_progress
```

A task's desired state is "completed." The reconciler assigns it to an agent, monitors progress, and re-queues if the agent dies. Tasks stuck in `in_progress` with no running agent get moved back to `pending`.

#### Workflow States (future)

```
pending ──▶ running ──▶ step_complete ──▶ running  (next step)
running ──▶ waiting_for_handoff ──▶ running        (different agent)
running ──▶ completed
running ──▶ failed ──▶ pending                     (retry)
```

Workflows are sequences of tasks with agent handoffs. The reconciler drives the workflow forward: when step N completes, it creates the task for step N+1 and assigns it to the appropriate agent.

### Resource-Specific Reconcilers

Each resource type has its own reconciler that runs in the shared loop:

```rust
/// The main reconciliation loop. Runs every N seconds.
async fn reconcile_all(ctx: &ReconcileContext) {
    // Order matters: images before agents, agents before tasks
    reconcile_images(ctx).await;
    reconcile_agents(ctx).await;
    reconcile_tasks(ctx).await;
    reconcile_conversations(ctx).await;
}
```

#### Agent Reconciler

```rust
async fn reconcile_agents(ctx: &ReconcileContext) {
    let docker = match &ctx.docker {
        Some(d) => d,
        None => return, // No Docker, nothing to reconcile
    };

    for agent_config in &ctx.config.agents {
        let desired = ctx.registry.get_desired_status(&agent_config.name);
        let container_name = format!("xpressclaw-{}", agent_config.name);
        let is_running = docker.is_container_running(&container_name).await;

        match (desired.as_str(), is_running) {
            ("running", true) => {
                // Stable — reset restart count if up > 5 min
                let uptime = docker.container_uptime(&container_name).await;
                if uptime > Duration::from_secs(300) {
                    ctx.registry.reset_restart_count(&agent_config.name);
                }
            }
            ("running", false) => {
                // Needs to be started (with backoff)
                let agent = ctx.registry.get(&agent_config.name);
                let backoff = exponential_backoff(agent.restart_count);
                if agent.time_since_last_attempt() >= backoff {
                    match start_agent(agent_config, docker, &ctx.config).await {
                        Ok(_) => {
                            ctx.registry.clear_last_error(&agent_config.name);
                            info!(agent = agent_config.name, "started");
                        }
                        Err(e) => {
                            ctx.registry.set_last_error(&agent_config.name, &e.to_string());
                            ctx.registry.increment_restart_count(&agent_config.name);
                            warn!(agent = agent_config.name, error = %e, "start failed, will retry");
                        }
                    }
                }
            }
            ("stopped", true) => {
                let _ = docker.stop(&container_name).await;
            }
            ("stopped", false) => {} // converged
        }
    }
}
```

#### Image Reconciler

```rust
async fn reconcile_images(ctx: &ReconcileContext) {
    let docker = match &ctx.docker {
        Some(d) => d,
        None => return,
    };

    for agent in &ctx.config.agents {
        let image = image_for_backend(&agent.backend);
        // Pull latest — Docker no-ops if already up to date.
        // Failures are non-fatal: use local image if available.
        match docker.pull_image(image).await {
            Ok(_) => debug!(image, "image up to date"),
            Err(e) => warn!(image, error = %e, "pull failed, using local if available"),
        }
    }
}
```

#### Task Reconciler

```rust
async fn reconcile_tasks(ctx: &ReconcileContext) {
    let board = TaskBoard::new(ctx.db.clone());
    let queue = TaskQueue::new(ctx.db.clone());

    // Re-queue tasks stuck in_progress whose agent isn't running
    for task in board.list_by_status("in_progress") {
        if let Some(agent_id) = &task.agent_id {
            let container = format!("xpressclaw-{agent_id}");
            let agent_running = ctx.docker
                .as_ref()
                .map(|d| d.is_container_running(&container))
                .unwrap_or(false)
                .await;

            if !agent_running {
                board.update_status(&task.id, "pending", None);
                queue.enqueue(&task.id, agent_id);
                info!(task_id = task.id, agent_id, "re-queued orphaned task");
            }
        }
    }
}
```

### User Actions

User actions only change desired state. The reconciler handles the rest:

```rust
// POST /api/agents/{id}/start
async fn start_agent_handler(id: &str) {
    registry.set_desired_status(id, "running");
    // Optionally trigger an immediate reconcile cycle
    reconcile_trigger.notify_one();
}

// POST /api/agents/{id}/stop
async fn stop_agent_handler(id: &str) {
    registry.set_desired_status(id, "stopped");
    reconcile_trigger.notify_one();
}
```

### API: Live Observed State

The `GET /api/agents` endpoint queries Docker live and merges with desired state from the DB:

```rust
async fn list_agents(db: &Database, docker: Option<&DockerManager>) -> Vec<AgentView> {
    let registry = AgentRegistry::new(db);
    registry.list().iter().map(|agent| {
        let container_name = format!("xpressclaw-{}", agent.id);
        let container = docker.and_then(|d| d.inspect(&container_name).await.ok());
        AgentView {
            id: agent.id,
            desired_status: agent.desired_status,
            observed_status: match (docker, &container) {
                (None, _) => "docker_unavailable",
                (Some(_), None) => "not_running",
                (Some(_), Some(c)) if c.is_running() => "running",
                (Some(_), Some(_)) => "exited",
            },
            last_error: agent.last_error,
            restart_count: agent.restart_count,
            container_id: container.as_ref().map(|c| &c.id),
            uptime: container.as_ref().and_then(|c| c.started_at()),
        }
    }).collect()
}
```

The UI derives the badge from the combination:

- **desired = running, container running** → Green "Running"
- **desired = running, container not found** → Yellow "Starting..." (reconciler will fix it)
- **desired = running, last_error set** → Red "Error (restarting...)" with message
- **desired = stopped, container not found** → Gray "Stopped"
- **Docker unavailable** → "Docker required"

### Restart Policy

Agents that crash get restarted with exponential backoff:

| Restart count | Backoff delay |
|---|---|
| 0 | Immediate |
| 1 | 5 seconds |
| 2 | 15 seconds |
| 3 | 30 seconds |
| 4 | 60 seconds |
| 5+ | 5 minutes (max) |

The restart count resets when the agent has been running stably for more than 5 minutes.

### Startup Sequence

On server startup:

1. Start the reconciliation loop (every 10 seconds)
2. First cycle pulls images, starts agents with `desired_status = 'running'`, re-queues orphaned tasks

That's it. No special startup code. The controller discovers actual Docker state and converges. If Docker isn't available yet, the controller logs a warning and retries next cycle.

### Docker Not Available

If Docker is not installed or not running:

- All agents show `observed_status = "docker_unavailable"` (derived, not stored)
- UI shows: "Docker is required to run agents."
- Controller keeps trying to connect each cycle
- When Docker becomes available, normal reconciliation begins

No fake "running" status. No silent failures.

## Consequences

### Positive
- **General pattern** — agents, tasks, conversations, and workflows all use the same state machine + reconciler shape. New resource types get resilience for free.
- **Self-healing** — crashes, Docker restarts, network blips are all recovered automatically
- **Truthful UI** — observed state is always live, never stale
- **No special startup code** — the reconciler handles cold start the same as any other drift
- **Workflow foundation** — agent-to-agent handoffs, multi-step workflows, and SOP execution can all be built on the same reconciliation infrastructure

### Negative
- More complex than direct start/stop
- 10-second reconciliation interval means actions aren't instant (mitigated by triggering immediate reconcile on user action)
- More Docker API calls (inspect on every API request + every reconcile cycle)

### Migration

1. Rename `status` column to `desired_status` in agents table
2. Drop `container_id`, `started_at`, `stopped_at`, `error_message` columns (all observable from Docker)
3. Add `last_error`, `restart_count`, `last_attempt_at` columns
4. Replace `start_agent`/`stop_agent` handlers to only set `desired_status`
5. Replace `list_agents`/`get_agent` handlers to query Docker live
6. Add the reconciliation loop to server startup
7. Update frontend to derive badge from desired + observed

## Related ADRs
- ADR-003: Container-based Agent Isolation (container spec, Docker management)
- ADR-006: SQLite Storage Layer (agents table schema)
- ADR-009: Task and SOP System (task lifecycle)
