# XpressAI CLI

**XpressAI on your machine.** Build continuous AI agents that work autonomously, remember what they've learned, and stay on budget while you sleep.

## Quick Start

```bash
pip install xpressai
xpressai init
xpressai up
```

That's it. Your first agent is now running.

## What Can It Do?

**Schedule recurring tasks:**
```bash
# Summarize Hacker News every morning at 9am
xpressai tasks atlas schedule "Summarize top 10 HN stories" --cron "0 9 * * *"
```

**Assign work and let agents handle it:**
```bash
xpressai tasks atlas add "Refactor the auth module to use JWT"
xpressai up -d  # Run in background
# Come back later to see results
```

**Review what happened while you were away:**
```bash
xpressai logs atlas
xpressai status
```

## Features

- **Autonomous Agents** - Agents pick up tasks from a queue and work through them
- **Scheduled Tasks** - Cron-style scheduling for recurring work
- **Memory** - Agents remember context across sessions using a zettelkasten-style note system
- **Budget Controls** - Set daily spending limits so agents don't run up your API bill
- **Multiple Backends** - Claude, OpenAI, local models (Qwen3-8B), and more
- **Container Isolation** - Run agents in Docker containers for safety
- **Observability** - Logs, status, and dashboards to see what agents are doing

## Installation

```bash
pip install xpressai
```

Or with uv:
```bash
uv pip install xpressai
```

### Requirements

- Python 3.11+
- Docker (optional, for container isolation)
- An API key for your chosen backend (Claude, OpenAI, etc.) or a local model

## Commands

```bash
xpressai init                  # Initialize workspace
xpressai up                    # Start agents (foreground)
xpressai up -d                 # Start agents (background daemon)
xpressai down                  # Stop all agents
xpressai status                # Show agent status and budget

# Tasks
xpressai tasks <agent> add "Do something"           # Add a task
xpressai tasks <agent> list                         # List tasks
xpressai tasks <agent> schedule "Task" --cron "..."  # Schedule recurring task
xpressai tasks <agent> schedules                    # List schedules

# Observability
xpressai logs [agent]          # View logs
xpressai logs -f               # Follow logs
xpressai budget                # Show budget usage
xpressai dashboard             # Open web dashboard
xpressai tui                   # Terminal UI
```

## Configuration

`xpressai init` creates an `xpressai.yaml` in your project:

```yaml
system:
  budget:
    daily: $20.00
    on_exceeded: pause  # pause | alert | stop

  isolation: docker  # docker | none

agents:
  - name: atlas
    backend: claude-code
    role: |
      You are a helpful coding assistant.

tools:
  builtin:
    filesystem: ~/agent-workspace
    shell:
      enabled: true
      allowed_commands: [git, npm, python]

memory:
  near_term_slots: 8
  eviction: least-recently-relevant
```

See [docs/configuration.md](docs/configuration.md) for the full reference.

## Documentation

- [Getting Started](docs/getting-started.md) - First steps with XpressAI
- [Configuration](docs/configuration.md) - Full configuration reference
- [Commands](docs/commands.md) - CLI command reference
- [Scheduling](docs/scheduling.md) - Set up recurring tasks

## How It Works

1. **You define agents** in `xpressai.yaml` with a role and backend
2. **Agents run continuously**, polling a task board for work
3. **You add tasks** via CLI, schedules, or programmatically
4. **Agents execute tasks** using their configured tools and memory
5. **Everything is logged** so you can review what happened

## License

Apache-2.0
