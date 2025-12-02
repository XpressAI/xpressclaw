# XpressAI - Agent Runtime System

XpressAI is an "Operating System for AI agents" - a simple, opinionated runtime that makes it easy to run, manage, and observe AI agents. Think of it as an agent operating system that handles isolation, memory, tools, budgets, and observability so you can focus on what your agents do, not how they run.

## Project Vision

The goal is **radical simplicity** for getting agents running:

```bash
xpressai init
xpressai up
```

That's it. Zero configuration to start. Progressive disclosure for power users.

## Core Architecture

### Agent Backends
XpressAI is a runtime, not another agent framework. It runs agents built with:
- **Claude Agent SDK** (primary, especially for developer agent teams)
- OpenAI Codex
- Gemini CLI
- Aider
- LangChain
- CrewAI
- Xaibo (our framework)
- Local models via Qwen3-8B (default for zero-config start)

### Key Subsystems

1. **Container Isolation** - Each agent runs in a Docker container to minimize blast radius
2. **Memory System** - Zettelkasten-style notes with vector search (sqlite-vec), 8-slot near-term memory with eviction
3. **Tool System** - MCP (Model Context Protocol) as the universal standard for all tools
4. **Task System** - Kanban-style task board, SOPs that agents can create and follow
5. **Budget Controls** - Per-agent and global spending limits with configurable actions on exceed
6. **Observability** - What did my agent do at 3am?

### UI Layers
- **CLI** - Primary interface, `xpressai` command
- **TUI** - Rich terminal UI built with Textual for monitoring/interaction
- **Web UI** - HTMX-based, server-rendered, minimal JavaScript

## Tech Stack

- **Language**: Python 3.11+
- **Database**: SQLite + sqlite-vec for vector storage
- **Container Runtime**: Docker (via docker-py)
- **Web Framework**: FastAPI + HTMX (Jinja2 templates)
- **TUI Framework**: Textual
- **Agent SDK**: claude-agent-sdk (primary)
- **Local Model**: Qwen3-8B via llama.cpp or Ollama

## Project Structure

```
xpressai-cli/                # Current project directory
├── src/xpressai/
│   ├── __init__.py
│   ├── cli/                 # CLI commands (click or typer)
│   │   ├── __init__.py
│   │   ├── main.py
│   │   ├── init.py
│   │   ├── up.py
│   │   └── status.py
│   ├── core/                # Core runtime
│   │   ├── __init__.py
│   │   ├── config.py        # Configuration loading/defaults
│   │   ├── runtime.py       # Main runtime orchestrator
│   │   └── lifecycle.py     # Agent lifecycle management
│   ├── agents/              # Agent backend adapters
│   │   ├── __init__.py
│   │   ├── base.py          # Abstract agent interface
│   │   ├── claude.py        # Claude Agent SDK adapter
│   │   ├── local.py         # Local model (Qwen3-8B) adapter
│   │   └── registry.py      # Backend discovery/registration
│   ├── isolation/           # Container isolation
│   │   ├── __init__.py
│   │   ├── docker.py        # Docker container management
│   │   └── sandbox.py       # Filesystem/network sandboxing
│   ├── memory/              # Memory system
│   │   ├── __init__.py
│   │   ├── zettelkasten.py  # Note storage and linking
│   │   ├── vector.py        # Vector search (sqlite-vec)
│   │   ├── slots.py         # Near-term memory slots
│   │   └── eviction.py      # Memory eviction strategies
│   ├── tools/               # MCP tool system
│   │   ├── __init__.py
│   │   ├── registry.py      # Tool discovery and registration
│   │   ├── mcp.py           # MCP server/client handling
│   │   └── builtin/         # Built-in tools
│   │       ├── filesystem.py
│   │       ├── web.py
│   │       └── shell.py
│   ├── tasks/               # Task and SOP system
│   │   ├── __init__.py
│   │   ├── board.py         # Kanban task board
│   │   ├── sop.py           # Standard operating procedures
│   │   └── scheduler.py     # Task scheduling
│   ├── budget/              # Budget and rate limiting
│   │   ├── __init__.py
│   │   ├── tracker.py       # Cost tracking
│   │   ├── limits.py        # Budget enforcement
│   │   └── policies.py      # On-exceed policies
│   ├── web/                 # Web UI (HTMX)
│   │   ├── __init__.py
│   │   ├── app.py           # FastAPI app
│   │   ├── routes/
│   │   └── templates/       # Jinja2 + HTMX
│   └── tui/                 # Terminal UI (Textual)
│       ├── __init__.py
│       └── app.py
├── tests/
├── docs/
│   └── adr/                 # Architecture Decision Records
├── pyproject.toml
└── README.md
```

## Development Guidelines

### Git Version Control
- Read ADR-000 and follow the instructions. We use git not just for source code versioning but also to store important notes about changes for later.

### Code Style
- Use type hints everywhere
- Prefer composition over inheritance
- Keep modules focused and small
- Write docstrings for public APIs

### Error Handling
- Use custom exception hierarchy rooted at `XpressAIError`
- Never swallow exceptions silently
- Log errors with context

### Configuration
- Environment variables for secrets (API keys)
- YAML for user configuration (`xpressai.yaml`)
- Sensible defaults for everything
- Progressive disclosure: start simple, add complexity as needed

### Testing
- pytest for all tests
- Use fixtures for database/container setup
- Mock external services (Docker, LLM APIs) in unit tests
- Integration tests with real containers

## Key Design Principles

1. **Zero Config Start** - `xpressai init && xpressai up` should just work
2. **Cloud Optional** - We default to the Claude Agent SDK but also allow local models (like Qwen3-8B by default) when needed for privacy/security.
3. **Safety by Default** - Containers, budgets, tool permissions
4. **Observable** - Always know what agents are doing
5. **Framework Agnostic** - Run any agent backend through a common interface
6. **MCP Native** - Tools speak MCP, period
7. **Reliable** - SOPs ensure Agents perform consistently and can learn to avoid mistakes over time.
8. **Agents Remember and Learn** - The plain-text zettelcasten memory system with SQLite-vec powered enable agents that can learn.

## Current Focus

We're building the MVP with this priority:
1. Core runtime and configuration
2. Claude Agent SDK integration (for developer agent teams)
3. Docker isolation
4. SQLite + sqlite-vec memory
5. Basic CLI (init, up, status, logs)
6. Simple HTMX dashboard
7. Textual TUI for interaction

## Commands Reference

```bash
xpressai init              # Initialize workspace with defaults
xpressai up                # Start the runtime and agents
xpressai down              # Stop all agents gracefully
xpressai status            # Show agent status, budget usage
xpressai logs [agent]      # Stream agent logs
xpressai memory [agent]    # Inspect memory state
xpressai tasks             # Show task board
xpressai sop create        # Create a new SOP
xpressai dashboard         # Open web dashboard
xpressai tui               # Launch terminal UI
```

## Configuration Example

```yaml
# xpressai.yaml - generated by `xpressai init`

system:
  budget:
    daily: $20.00
    on_exceeded: pause  # pause | alert | degrade | stop
  
  isolation: docker  # docker | none
  
agents:
  - name: atlas
    backend: claude-code  # or: local, openai-codex, etc.
    role: |
      You are my executive assistant...
    autonomy: high
    wake_on:
      - schedule: "every 30 minutes"
      - event: user.message

tools:
  builtin:
    filesystem: ~/agent-workspace
    web_browser: true
    shell:
      enabled: true
      allowed_commands: [git, npm, python]

memory:
  near_term_slots: 8
  eviction: least-recently-relevant
  # cleanup: none | delete_after:30d | summarize
```

## Reading the ADRs

Before making significant changes, read the relevant ADRs in `docs/adr/`:
- ADR-001: Project overview
- ADR-002: Agent backend abstraction
- ADR-003: Container isolation
- ADR-004: Memory system
- ADR-005: MCP tool system
- ADR-006: SQLite storage
- ADR-007: HTMX web UI
- ADR-008: Textual TUI
- ADR-009: Task/SOP system
- ADR-010: Budget controls
- ADR-011: Default local model
