# CLI Command Reference

Complete reference for all `xpressclaw` commands.

## Core Commands

### `xpressclaw init`

Initialize a new xpressclaw workspace.

```bash
xpressclaw init
```

### `xpressclaw up`

Start the runtime and all configured agents.

```bash
xpressclaw up           # Run in foreground
xpressclaw up --detach  # Run in background
```

**Options:**
- `--detach` - Run in background mode
- `--port` - Port for web UI and API (default: 8935)

### `xpressclaw down`

Stop all running agents and the runtime.

```bash
xpressclaw down
```

### `xpressclaw status`

Show current status of agents and the runtime.

```bash
xpressclaw status
```

### `xpressclaw logs`

View agent logs.

```bash
xpressclaw logs              # View recent logs
xpressclaw logs --agent NAME # View specific agent logs
xpressclaw logs -f           # Follow logs in real-time
```

## Task Commands

### `xpressclaw tasks list`

List tasks.

```bash
xpressclaw tasks list
xpressclaw tasks list --agent NAME
```

### `xpressclaw tasks create`

Create a new task.

```bash
xpressclaw tasks create "Refactor the auth module" --agent atlas
xpressclaw tasks create "Fix login bug" --agent atlas --priority high
```

**Options:**
- `--priority` - `high`, `medium`, `low` (default: medium)
- `--agent` - Agent to assign task to

### `xpressclaw tasks schedule`

Create a recurring scheduled task.

```bash
xpressclaw tasks schedule "Daily standup summary" --agent atlas --cron "0 9 * * *"
```

**Options:**
- `--cron` - Cron expression (required)

### `xpressclaw tasks complete`

Mark a task as completed.

```bash
xpressclaw tasks complete TASK_ID
```

## Memory Commands

### `xpressclaw memory list`

List memories.

```bash
xpressclaw memory list
xpressclaw memory list --agent NAME
```

### `xpressclaw memory search`

Search memories.

```bash
xpressclaw memory search "project deadlines"
```

## Budget Commands

### `xpressclaw budget`

Show budget status.

```bash
xpressclaw budget
xpressclaw budget --agent NAME
```

## SOP Commands

### `xpressclaw sop list`

List available SOPs.

```bash
xpressclaw sop list
```

### `xpressclaw sop create`

Create a new SOP.

```bash
xpressclaw sop create deploy-process
```

### `xpressclaw sop run`

Execute an SOP.

```bash
xpressclaw sop run deploy-process --agent atlas
```

## Chat Commands

### `xpressclaw chat`

Interactive chat with an agent.

```bash
xpressclaw chat atlas
```

## Global Options

| Option | Description |
|--------|-------------|
| `--help` | Show help for any command |
| `--version` | Show xpressclaw version |

## Examples

**Full workflow:**

```bash
# Initialize and start
xpressclaw init
xpressclaw up

# Add a task
xpressclaw tasks create "Update dependencies" --agent atlas

# Schedule recurring work
xpressclaw tasks schedule "Check HN" --agent atlas --cron "0 9 * * 1-5"

# Monitor
xpressclaw status
xpressclaw logs -f

# When done
xpressclaw down
```
