# ⚡ XpressAI

**The Operating System for persistently running AI Agents**

XpressAI is an agent runtime that makes it trivially easy to run, manage, and observe AI agents. Think of it just like as an operating system - handling isolation, memory, tools, budgets, and observability so you can focus on what your agents do, not how they run.

## Quick Start

```bash
# Install
pip install xpressai

# Initialize a workspace
xpressai init

# Start your agents
xpressai up
```

That's it. Zero configuration required to start.

## Features

- **🐳 Container Isolation**: Each agent runs in Docker with configurable resource limits
- **🧠 Persistent Memory**: Zettelkasten-style notes with vector search (sqlite-vec)
- **🔧 MCP Tools**: Universal tool standard - filesystem, shell, web, and custom tools
- **💰 Budget Controls**: Per-agent and global spending limits with automatic enforcement
- **📋 Task Board**: Kanban-style task management with SOPs (Standard Operating Procedures)
- **🖥️ Multiple UIs**: CLI, rich TUI (Textual), and web dashboard (HTMX)

## Agent Backends

XpressAI is a runtime, not another framework. It runs agents built with:

- **Claude Agent SDK** (primary) - For developer agent teams
- OpenAI Codex CLI
- Gemini CLI  
- Aider
- LangChain
- CrewAI
- Local models (Qwen3-8B)

## Architecture

```
┌─────────────────────────────────────────────────┐
│                 XpressAI Runtime                │
│  (CLI, TUI, Web UI, Orchestrator)               │
└───────────────┬─────────────────────────────────┘
                │
    ┌───────────┼───────────┐
    │           │           │
┌───▼───┐   ┌───▼───┐   ┌───▼───┐
│Agent 1│   │Agent 2│   │Agent 3│
│Docker │   │Docker │   │Docker │
└───────┘   └───────┘   └───────┘
```

## Configuration

```yaml
# xpressai.yaml
system:
  budget:
    daily: $20.00
    on_exceeded: pause  # pause | alert | degrade | stop

agents:
  - name: atlas
    backend: claude-code
    role: |
      You are my executive assistant...
    autonomy: high

tools:
  builtin:
    filesystem: ~/agent-workspace
    shell:
      allowed_commands: [git, npm, python]
```

## Commands

```bash
xpressai init              # Initialize workspace
xpressai up                # Start agents
xpressai down              # Stop agents
xpressai status            # Show status
xpressai logs [agent]      # Stream logs
xpressai tui               # Launch terminal UI
xpressai dashboard         # Open web dashboard
```

## Development

```bash
# Clone the repo
git clone https://github.com/xpressai/xpressai
cd xpressai

# Install in development mode
pip install -e ".[dev]"

# Run tests
pytest

# Run linting
ruff check src/
mypy src/
```

## Documentation

See the [docs/adr/](docs/adr/) directory for Architecture Decision Records explaining the design.

## License

Apache 2.0
