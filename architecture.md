# AgentKeeper Architecture

## Overview

AgentKeeper is structured as a layered system where each layer can be developed and tested independently.

```
┌─────────────────────────────────────────────────────────────┐
│                         CLI / API                           │
├─────────────────────────────────────────────────────────────┤
│                      Agent Manager                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │   Agent 1   │  │   Agent 2   │  │  Meeseeks   │         │
│  │  (primary)  │  │  (primary)  │  │ (temporary) │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
├─────────────────────────────────────────────────────────────┤
│                     Backend Adapters                        │
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐   │
│  │Qwen3-8B│ │ Claude │ │ Codex  │ │ Gemini │ │ Xaibo  │   │
│  │ (local)│ │  Code  │ │        │ │  CLI   │ │        │   │
│  └────────┘ └────────┘ └────────┘ └────────┘ └────────┘   │
├─────────────────────────────────────────────────────────────┤
│                     Core Services                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │  Memory  │  │  Tools   │  │  Budget  │  │ Isolation│   │
│  │  System  │  │  System  │  │  System  │  │  System  │   │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
├─────────────────────────────────────────────────────────────┤
│                      Storage Layer                          │
│  ┌──────────────────┐  ┌──────────────────┐                │
│  │   SQLite (state) │  │  Vector DB       │                │
│  │                  │  │  (embeddings)    │                │
│  └──────────────────┘  └──────────────────┘                │
└─────────────────────────────────────────────────────────────┘
```

## Component Responsibilities

### CLI / API Layer

The user-facing interface. All operations go through here.

- `agentkeeper init` - Initialize a new workspace
- `agentkeeper up` - Start the daemon and agents
- `agentkeeper down` - Stop gracefully
- `agentkeeper status` - Show agent status, budget usage
- `agentkeeper logs` - Stream or view agent logs
- `agentkeeper memory` - Inspect/manage memory system
- `agentkeeper chat` - Interactive session with an agent

### Agent Manager

Orchestrates agent lifecycles.

- Starts/stops agents based on configuration
- Handles agent restarts on failure
- Manages Meeseeks (temporary agent) spawning and cleanup
- Routes messages between agents
- Enforces budget limits (delegates to Budget System)

### Backend Adapters

Normalize different agent implementations into a common interface.

Each adapter must implement:

```python
class BackendAdapter(Protocol):
    async def start(self, config: AgentConfig) -> None: ...
    async def stop(self) -> None: ...
    async def send_message(self, message: str) -> AsyncIterator[str]: ...
    async def invoke_tool(self, tool: str, args: dict) -> ToolResult: ...
    def get_status(self) -> AgentStatus: ...
```

**Priority backends:**
1. Qwen3-8B (local, default)
2. Claude Code (cloud, primary upgrade path)

**Future backends:**
- OpenAI Codex
- Gemini CLI
- Aider
- LangChain
- CrewAI
- Xaibo

### Memory System

Zettelkasten-based knowledge management with vector search.

- **Zettelkasten store**: Interconnected notes with bidirectional links
- **Vector index**: Embeddings for semantic search
- **Slot manager**: 8 near-term memory slots spliced into context
- **Eviction policy**: LRU or relevance-based eviction to zettelkasten

### Tool System

Sandboxed tool execution with permissions.

- **Built-in tools**: filesystem, shell, web browser, etc.
- **Permission model**: Explicit allow-list per agent
- **Tool registry**: Discover and load custom tools (MCP compatible)

### Budget System

Cost tracking and enforcement.

- Track token usage and API costs per agent
- Enforce daily/monthly/per-task limits
- Configurable actions: pause, alert, degrade, stop
- Support for local models (compute-time based) and API models (cost based)

### Isolation System

Contain blast radius when things go wrong.

- **Container mode**: Each agent runs in isolated container
- **VM mode**: Stronger isolation for untrusted workloads
- **None mode**: Direct execution (development only)

Manages:
- Filesystem isolation
- Network isolation
- Resource limits (CPU, memory)
- Capability restrictions

### Storage Layer

Persistent state management.

- **SQLite**: Agent state, task history, budget tracking, configuration
- **Vector DB**: Memory embeddings (default: embedded solution like sqlite-vec or hnswlib)

## Data Flow Examples

### Agent Startup

```
1. CLI: `agentkeeper up`
2. Agent Manager: Load config, validate
3. For each agent in config:
   a. Isolation System: Create sandbox
   b. Backend Adapter: Initialize agent runtime
   c. Memory System: Load relevant memories into slots
   d. Tool System: Register permitted tools
   e. Agent Manager: Mark agent as running
4. CLI: Show status, enter interactive mode or detach
```

### Agent Processing a Task

```
1. Agent receives task (from user, schedule, or event)
2. Memory System: Find relevant memories, splice into context
3. Agent reasons about task
4. Agent requests tool use
5. Budget System: Check if within limits
6. Tool System: Validate permissions, execute in sandbox
7. Agent receives tool result
8. Repeat 3-7 as needed
9. Memory System: Update memories, evict if slots full
10. Task complete
```

### Meeseeks Spawning

```
1. Primary agent decides to spawn specialist
2. Agent Manager: Create Meeseeks with:
   - Inherited relevant context from parent
   - Own private memory space
   - Task-scoped budget cap
   - Subset of parent's tool permissions
3. Meeseeks executes task
4. Meeseeks reports result to parent
5. Agent Manager: Clean up Meeseeks
   - Optionally merge useful memories to parent
   - Release resources
```

## Configuration Hierarchy

```
System defaults
  └── Workspace config (agentkeeper.yaml)
       └── Agent config (agents/*.yaml)
            └── Runtime overrides (CLI flags)
```

## File Layout

```
~/.agentkeeper/                    # Global config and cache
  config.yaml                      # Global defaults
  models/                          # Downloaded model weights
    qwen3-8b-q4_K_M.gguf
  
my-workspace/                      # User's workspace
  agentkeeper.yaml                 # Workspace config
  agents/
    main.yaml                      # Primary agent definition
    specialists/                   # Meeseeks templates
      researcher.yaml
      coder.yaml
  sops/                            # Standard operating procedures
    weekly-report.yaml
  memory/                          # Managed by Memory System
    zettelkasten.db
    vectors.db
  logs/                            # Agent logs
  .agentkeeper/                    # Runtime state (gitignored)
    state.db
    sockets/
```

## Technology Choices

| Component | Technology | Rationale |
|-----------|------------|-----------|
| Language | Rust | Performance, safety, single binary |
| Local LLM | llama.cpp | Best GGUF support, Qwen3 compatible |
| Vector DB | sqlite-vec or hnswlib | Embedded, no external deps |
| State DB | SQLite | Reliable, embedded, well-understood |
| Containers | Podman or Docker | Standard, widely available |
| IPC | Unix sockets | Fast, simple |

## Security Model

1. **Principle of least privilege**: Agents only get tools they need
2. **Defense in depth**: Multiple isolation layers
3. **Audit trail**: All actions logged with full context
4. **Budget caps**: Financial safety net
5. **User confirmation**: Configurable gates for sensitive actions
