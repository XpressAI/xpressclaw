# Getting Started with xpressclaw

This guide will help you set up xpressclaw and run your first autonomous agent.

## Prerequisites

- Rust toolchain (see [rustup.rs](https://rustup.rs/))
- An API key for your chosen backend:
  - **Claude**: Set `ANTHROPIC_API_KEY` environment variable
  - **OpenAI**: Set `OPENAI_API_KEY` environment variable
  - **Local models**: No API key needed (uses Ollama or llama.cpp)
- Docker (optional, for container isolation)

## Installation

### Download Binary

Grab the latest release from [GitHub Releases](https://github.com/XpressAI/xpressclaw/releases).

```bash
xpressclaw init
xpressclaw up
```

### Build from Source

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw
cargo build --release
```

## Quick Start

### 1. Initialize a Workspace

Navigate to your project directory and run:

```bash
xpressclaw init
```

This creates an `xpressclaw.yaml` configuration file with sensible defaults.

### 2. Start the Runtime

```bash
xpressclaw up
```

This starts your agents. You'll see logs as they run.

To run in the background:

```bash
xpressclaw up --detach
```

### 3. Give Your Agent a Task

Open another terminal and add a task:

```bash
xpressclaw tasks create "Refactor the auth module" --agent atlas
```

### 4. Check Status

```bash
xpressclaw status
```

### 5. Stop the Runtime

```bash
xpressclaw down
```

## Next Steps

### Schedule Recurring Tasks

Have your agent do something every day:

```bash
xpressclaw tasks schedule "Check for security updates" --agent atlas --cron "0 9 * * *"
```

### Configure Your Agent

Edit `xpressclaw.yaml` to customize:

```yaml
agents:
  - name: atlas
    backend: claude-code
    role: |
      You are a DevOps assistant. You help with:
      - Monitoring system health
      - Running maintenance scripts
      - Summarizing logs
```

See [Configuration](configuration.md) for all options.

### Use the Dashboard

The web UI is built-in. Just run:

```bash
xpressclaw up
```

And visit http://localhost:8935.

## Example: Daily News Summary

Here's a complete example that summarizes Hacker News every morning:

```bash
# Initialize
xpressclaw init

# Configure the agent (edit xpressclaw.yaml)
cat > xpressclaw.yaml << 'EOF'
system:
  budget:
    daily: $5.00
    on_exceeded: pause

agents:
  - name: newsbot
    backend: claude-code
    role: |
      You are a news summarizer. You browse Hacker News,
      identify the top stories, and create concise summaries.
EOF

# Start in background
xpressclaw up --detach

# Schedule daily summary at 8am
xpressclaw tasks schedule "Summarize top 10 HN stories" --agent newsbot --cron "0 8 * * *"

# Check status
xpressclaw status

# View logs
xpressclaw logs newsbot
```

## Learn More

- [Configuration Reference](configuration.md) - All configuration options
- [CLI Commands](commands.md) - Complete command reference
- [Scheduling](scheduling.md) - Set up recurring tasks
