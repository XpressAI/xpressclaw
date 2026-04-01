# ADR-020: Task Dependencies and Topological Scheduling

## Status
Proposed

## Context

The current task system is flat: tasks have priorities and parent/child hierarchies, but no dependencies between them. The dispatcher runs tasks FIFO per agent. This breaks down when:

1. **Task B needs Task A's output.** Without dependencies, B might run before A finishes, fail, and the agent wastes turns figuring out why.
2. **Small models can't plan.** A 9B model can break work into steps, but it can't reliably reason about execution order across multiple tasks. The system should handle scheduling — the agent just declares "B depends on A."
3. **Multi-agent workflows.** Agent X produces data, Agent Y consumes it. Without dependencies, there's no coordination — Y starts before X finishes.
4. **SOPs are sequential but brittle.** The current SOP system creates subtasks in order, but if step 3 fails and is retried, step 4 doesn't wait — it was already created and might be dispatched.

### What We Need

A dependency graph where:
- An agent (or user) says "Task B depends on Task A"
- The system only dispatches B after A completes
- Cycles are rejected
- The dispatcher does topological ordering automatically
- Blocked tasks show WHY they're blocked (which dependency)
- When a dependency completes, blocked tasks become ready

### Design Principles

**The system does the hard work, not the model.** A small model should only need to:
- Create tasks with `depends_on: ["task-id-1", "task-id-2"]`
- The system validates (no cycles), blocks the task, and auto-unblocks when dependencies complete

**Dependencies are between tasks, not agents.** Task A might be assigned to agent X, Task B to agent Y. When A completes (regardless of which agent did it), B becomes ready.

## Decision

### 1. Task Dependencies Table

```sql
CREATE TABLE task_dependencies (
    task_id TEXT NOT NULL,        -- the dependent task (blocked)
    depends_on TEXT NOT NULL,     -- the dependency (must complete first)
    PRIMARY KEY (task_id, depends_on),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (depends_on) REFERENCES tasks(id) ON DELETE CASCADE
);
```

Simple directed edges. Task A depends on Task B means: don't dispatch A until B is completed.

### 2. Dependency Validation (Cycle Detection)

When adding a dependency, run DFS from the dependency back to the task. If we find the task, it's a cycle — reject it.

```rust
fn would_create_cycle(task_id: &str, depends_on: &str) -> bool {
    // DFS from depends_on: can we reach task_id?
    let mut visited = HashSet::new();
    let mut stack = vec![depends_on];
    while let Some(current) = stack.pop() {
        if current == task_id {
            return true; // Cycle!
        }
        if visited.insert(current) {
            // Add all tasks that `current` depends on
            for dep in get_dependencies(current) {
                stack.push(dep);
            }
        }
    }
    false
}
```

### 3. Dispatcher Integration

The dispatcher already checks task status before dispatch. Add a dependency check:

```rust
fn is_ready(task: &Task) -> bool {
    if task.status != "pending" {
        return false;
    }
    // Check all dependencies are completed
    let deps = get_dependencies(&task.id);
    deps.iter().all(|dep_id| {
        get_task(dep_id)
            .map(|t| t.status == "completed")
            .unwrap_or(true) // deleted dependency = satisfied
    })
}
```

Tasks with unmet dependencies stay in `pending` status but are skipped by the dispatcher. When a dependency completes, the dependent task becomes eligible on the next dispatcher cycle.

### 4. Auto-Blocking and Auto-Unblocking

When a task is created with dependencies:
- If all dependencies are already completed → task stays `pending` (ready to dispatch)
- If any dependency is not completed → task stays `pending` but is skipped by the dispatcher

No separate "blocked" status needed for dependency tracking. The `blocked` status is reserved for manual blocking (agent says "I'm stuck"). Dependencies are a scheduling constraint, not a status.

When a task completes:
- The system checks: "which tasks depend on this one?"
- For each, check if ALL dependencies are now met
- If so, the task is now eligible for dispatch (still `pending`, just no longer skipped)
- If the dependent task has an agent assigned and is enqueued, it will be picked up on the next dispatcher cycle

### 5. Task Creation with Dependencies

The `create_task` API and MCP tool accept a `depends_on` field:

```json
{
    "title": "Deploy to staging",
    "agent_id": "atlas",
    "depends_on": ["task-id-for-build", "task-id-for-tests"]
}
```

The system:
1. Creates the task as `pending`
2. Validates no cycles
3. Inserts dependency edges
4. Enqueues the task (dispatcher will skip it until dependencies are met)

### 6. Topological Ordering for Display

The UI and API can return tasks in dependency order:

```rust
fn topological_sort(tasks: &[Task]) -> Vec<&Task> {
    // Kahn's algorithm
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for task in tasks {
        in_degree.entry(&task.id).or_insert(0);
        for dep in get_dependencies(&task.id) {
            adj.entry(&dep).or_default().push(&task.id);
            *in_degree.entry(&task.id).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut result = Vec::new();
    while let Some(id) = queue.pop_front() {
        result.push(id);
        if let Some(dependents) = adj.get(id) {
            for &dep in dependents {
                let deg = in_degree.get_mut(dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }
    // Return tasks in topological order
    result.iter().filter_map(|id| tasks.iter().find(|t| t.id == *id)).collect()
}
```

### 7. MCP Tool Changes

**`create_tasks`** — batch creation with inline dependencies:

A single tool call creates an entire task graph. This is critical for small models — one call instead of N sequential calls with dependency wiring.

```json
{
    "name": "create_tasks",
    "description": "Create one or more tasks with dependencies. Use local IDs (any string) to reference tasks within this batch. Dependencies can reference local IDs (for tasks in this batch) or existing task UUIDs.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Local ID for referencing within this batch (e.g. 'build', 'test'). The system assigns real UUIDs." },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "agent_id": { "type": "string" },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Local IDs or existing task UUIDs that must complete first"
                        }
                    },
                    "required": ["title"]
                }
            },
            "parent_task_id": {
                "type": "string",
                "description": "If set, all tasks in this batch become subtasks of this parent"
            }
        },
        "required": ["tasks"]
    }
}
```

Example — model creates a deploy pipeline in one call:
```json
{
    "tasks": [
        { "id": "build", "title": "Build frontend", "agent_id": "dev" },
        { "id": "test", "title": "Run unit tests", "agent_id": "dev", "depends_on": ["build"] },
        { "id": "lint", "title": "Run linter", "agent_id": "dev", "depends_on": ["build"] },
        { "id": "deploy", "title": "Deploy to staging", "agent_id": "ops", "depends_on": ["test", "lint"] }
    ]
}
```

The system:
1. Validates the graph (no cycles)
2. Maps local IDs → real UUIDs
3. Creates all tasks atomically
4. Inserts all dependency edges
5. Returns the created tasks with real IDs

The old `create_task` (singular) still works for simple cases — it's equivalent to `create_tasks` with a single-element array.

**`add_dependency`** — for adding dependencies to existing tasks:
```json
{
    "name": "add_dependency",
    "description": "Add a dependency: task cannot start until the dependency completes.",
    "inputSchema": {
        "properties": {
            "task_id": { "type": "string", "description": "Task that will be blocked" },
            "depends_on": { "type": "string", "description": "Task that must complete first" }
        }
    }
}
```

**`get_task`** — response includes dependencies:
```json
{
    "id": "task-123",
    "title": "Deploy to staging",
    "status": "pending",
    "depends_on": ["task-456", "task-789"],
    "dependents": ["task-012"],
    "ready": false,
    "blocked_by": ["task-789"]  // only incomplete dependencies
}
```

The `ready` and `blocked_by` fields help small models understand why a task isn't running without needing to reason about the dependency graph themselves.

### 8. Cascade Behavior

When a dependency is **completed**: dependents become eligible (no action needed — dispatcher checks automatically).

When a dependency is **cancelled**: dependents are also cancelled (cascade). A task that depends on cancelled work can't succeed.

When a dependency is **failed**: dependents are blocked. The user/agent can retry the failed task or cancel the dependents.

When a dependency is **deleted**: the edge is removed (ON DELETE CASCADE). The dependent task loses that constraint and may become eligible.

### 9. Helping Small Models

The system should actively help models that struggle with task planning:

**Auto-generated context in prompts:**
When the dispatcher builds the prompt for a task, it includes:
```
## Task Dependencies
This task was waiting for these tasks to complete:
- ✅ "Build frontend" (completed 2 min ago)
- ✅ "Run tests" (completed 30 sec ago)

Their results are available in the task history.
```

**Dependency output forwarding:**
When a dependency completes, its final output (last task message) is included as context in the dependent task's prompt. The model doesn't need to go find the results — they're right there.

**Validation errors as hints:**
If a model tries to create a cycle, the error message explains WHY:
```
Cannot add dependency: "Deploy" → "Build" → "Deploy" would create a cycle.
"Build" already depends on "Deploy" (directly or transitively).
```

## Consequences

### Positive
- Tasks execute in correct order automatically
- Small models just declare dependencies — system handles scheduling
- Multi-agent workflows become possible (agent X's output feeds agent Y)
- SOPs can express parallel and sequential steps
- Dependency output forwarding gives context without extra LLM calls

### Negative
- More complex dispatcher logic
- Cycle detection adds overhead on dependency creation (negligible for small graphs)
- Cancelled cascades could surprise users

### Migration
1. Add `task_dependencies` table (new migration)
2. Add `depends_on` to `create_task` API and MCP tool
3. Add `add_dependency` MCP tool
4. Update dispatcher to check dependencies before dispatch
5. Update task API responses to include dependency info
6. Update frontend task view to show dependency graph

## Related ADRs
- ADR-009: Task and SOP System (current task model)
- ADR-018: Desired-State Reconciliation (reconciler pattern)
- ADR-019: Background Conversations (async processing)
