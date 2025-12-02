# Getting Started with XpressAI

This guide will help you set up XpressAI and run your first autonomous agent.

## Prerequisites

- Python 3.11 or higher
- An API key for your chosen backend:
  - **Claude**: Set `ANTHROPIC_API_KEY` environment variable
  - **OpenAI**: Set `OPENAI_API_KEY` environment variable
  - **Local models**: No API key needed (uses Ollama or llama.cpp)
- Docker (optional, for container isolation)

## Installation

```bash
pip install xpressai
```

Or with uv (recommended):

```bash
uv pip install xpressai
```

## Quick Start

### 1. Initialize a Workspace

Navigate to your project directory and run:

```bash
xpressai init
```

This creates an `xpressai.yaml` configuration file with sensible defaults:
- One agent named "atlas" using Claude
- $20/day budget limit
- Basic filesystem and shell tools enabled

### 2. Start the Runtime

```bash
xpressai up
```

This starts your agent in the foreground. You'll see logs as the agent runs.

To run in the background:

```bash
xpressai up -d
```

### 3. Give Your Agent a Task

Open another terminal and add a task:

```bash
xpressai tasks atlas add "Create a hello.txt file with a friendly greeting"
```

Watch the logs to see your agent pick up and complete the task.

### 4. Check Status

```bash
xpressai status
```

This shows:
- Which agents are running
- Current budget usage
- Task counts (pending, in progress, completed)

### 5. Stop the Runtime

```bash
xpressai down
```

## Next Steps

### Schedule Recurring Tasks

Have your agent do something every day:

```bash
xpressai tasks atlas schedule "Check for security updates" --cron "0 9 * * *"
```

See [Scheduling](scheduling.md) for cron syntax and more examples.

### Configure Your Agent

Edit `xpressai.yaml` to customize:

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

### Set Budget Limits

Control spending with budget configuration:

```yaml
system:
  budget:
    daily: $10.00
    on_exceeded: pause  # Agent pauses when budget is hit
```

### Use the Dashboard

Launch a web dashboard to monitor your agents:

```bash
xpressai dashboard
```

Or use the terminal UI:

```bash
xpressai tui
```

## Example: Daily News Summary

Here's a complete example that summarizes Hacker News every morning:

```bash
# Initialize
xpressai init

# Configure the agent (edit xpressai.yaml)
cat > xpressai.yaml << 'EOF'
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
xpressai up -d

# Schedule daily summary at 8am
xpressai tasks newsbot schedule "Summarize top 10 HN stories into hn-{date}.md" --cron "0 8 * * *"

# Check it's scheduled
xpressai tasks newsbot schedules
```

## Troubleshooting

### Agent not picking up tasks

Make sure the runtime is running:

```bash
xpressai status
```

If it shows "not connected to daemon", start it:

```bash
xpressai up -d
```

### API key errors

Ensure your API key is set:

```bash
export ANTHROPIC_API_KEY="your-key-here"
```

Add it to your shell profile (`.bashrc`, `.zshrc`) to persist.

### Check logs

```bash
xpressai logs atlas
```

Or follow logs in real-time:

```bash
xpressai logs -f
```

## Learn More

- [Configuration Reference](configuration.md) - All configuration options
- [CLI Commands](commands.md) - Complete command reference
- [Scheduling](scheduling.md) - Set up recurring tasks
