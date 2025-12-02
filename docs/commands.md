# CLI Command Reference

Complete reference for all `xpressai` commands.

## Core Commands

### `xpressai init`

Initialize a new XpressAI workspace.

```bash
xpressai init
```

Creates an `xpressai.yaml` configuration file with sensible defaults.

**Options:**
- `--force` - Overwrite existing configuration

---

### `xpressai up`

Start the runtime and all configured agents.

```bash
xpressai up        # Run in foreground
xpressai up -d     # Run as background daemon
```

**Options:**
- `-d, --daemon` - Run in background mode

When running in daemon mode, use `xpressai down` to stop.

---

### `xpressai down`

Stop all running agents and the runtime.

```bash
xpressai down
```

---

### `xpressai status`

Show current status of agents and the runtime.

```bash
xpressai status
```

**Output includes:**
- Agent status (running/stopped)
- Budget usage
- Task counts

---

### `xpressai logs`

View agent logs.

```bash
xpressai logs              # View all logs
xpressai logs atlas        # View logs for specific agent
xpressai logs -f           # Follow logs in real-time
xpressai logs atlas -f     # Follow specific agent
```

**Options:**
- `-f, --follow` - Stream logs continuously

---

## Task Commands

All task commands require an agent name.

### `xpressai tasks <agent> list`

List tasks for an agent.

```bash
xpressai tasks atlas list
```

Shows tasks grouped by status (pending, in progress, completed).

---

### `xpressai tasks <agent> add`

Add a new task for an agent.

```bash
xpressai tasks atlas add "Refactor the auth module"
xpressai tasks atlas add "Fix bug in login" --priority high
```

**Options:**
- `--priority` - Task priority: `high`, `medium`, `low` (default: medium)

---

### `xpressai tasks <agent> complete`

Mark a task as completed.

```bash
xpressai tasks atlas complete abc123
```

Use the task ID prefix (shown in `list`).

---

### `xpressai tasks <agent> delete`

Delete a task.

```bash
xpressai tasks atlas delete abc123
```

---

### `xpressai tasks <agent> schedule`

Create a recurring scheduled task.

```bash
xpressai tasks atlas schedule "Daily standup summary" --cron "0 9 * * *"
xpressai tasks atlas schedule "Weekly report" --cron "0 17 * * 5" --name weekly-report
```

**Options:**
- `--cron` - Cron expression (required)
- `--name` - Optional name for the schedule

See [Scheduling](scheduling.md) for cron syntax.

---

### `xpressai tasks <agent> schedules`

List scheduled tasks for an agent.

```bash
xpressai tasks atlas schedules
```

Shows schedule ID, cron expression, next run time, and run count.

---

### `xpressai tasks <agent> unschedule`

Remove a scheduled task.

```bash
xpressai tasks atlas unschedule abc123
```

Use the schedule ID prefix (shown in `schedules`).

---

## Budget Commands

### `xpressai budget`

Show budget status.

```bash
xpressai budget
```

Shows daily spending, limits, and usage by agent.

---

## UI Commands

### `xpressai dashboard`

Launch the web dashboard.

```bash
xpressai dashboard
```

Opens a browser to the HTMX-based dashboard at `http://localhost:8935`.

---

### `xpressai tui`

Launch the terminal UI.

```bash
xpressai tui
```

Interactive terminal interface built with Textual.

**Keyboard shortcuts:**
- `q` - Quit
- `l` - View logs
- `t` - View tasks
- `s` - View status

---

## SOP Commands

Standard Operating Procedures for agents.

### `xpressai sop list`

List available SOPs.

```bash
xpressai sop list
```

---

### `xpressai sop create`

Create a new SOP.

```bash
xpressai sop create deploy-process
```

Opens an editor to define the SOP steps.

---

### `xpressai sop show`

Show details of an SOP.

```bash
xpressai sop show deploy-process
```

---

### `xpressai sop delete`

Delete an SOP.

```bash
xpressai sop delete deploy-process
```

---

## Global Options

These options work with any command:

| Option | Description |
|--------|-------------|
| `--help` | Show help for any command |
| `--version` | Show XpressAI version |

## Examples

**Full workflow:**

```bash
# Initialize and start
xpressai init
xpressai up -d

# Add some tasks
xpressai tasks atlas add "Set up CI/CD pipeline"
xpressai tasks atlas add "Write unit tests" --priority high

# Schedule recurring work
xpressai tasks atlas schedule "Check for security updates" --cron "0 9 * * 1"

# Monitor
xpressai status
xpressai logs -f

# When done
xpressai down
```
