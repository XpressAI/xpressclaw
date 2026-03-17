# Scheduling Tasks

xpressclaw supports cron-style scheduling for recurring tasks. Agents automatically pick up scheduled tasks when they trigger.

## Quick Start

```bash
# Schedule a task to run every day at 9am
xpressclaw tasks create --agent atlas --description "Check for updates"

# Use cron for recurring tasks
xpressclaw tasks schedule "Check for updates" --agent atlas --cron "0 9 * * *"

# List schedules
xpressclaw tasks list --agent atlas

# Remove a schedule (via xpressclaw tasks complete or edit)
```

## Cron Syntax

xpressclaw uses standard 5-field cron syntax:

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-6, 0=Sunday)
│ │ │ │ │
* * * * *
```

### Common Patterns

| Pattern | Description |
|---------|-------------|
| `0 9 * * *` | Every day at 9:00 AM |
| `0 9 * * 1-5` | Weekdays at 9:00 AM |
| `0 */2 * * *` | Every 2 hours |
| `30 8 * * 1` | Every Monday at 8:30 AM |
| `0 0 1 * *` | First day of every month at midnight |
| `0 17 * * 5` | Every Friday at 5:00 PM |
| `*/15 * * * *` | Every 15 minutes |
| `0 9,12,17 * * *` | At 9am, 12pm, and 5pm daily |

### Special Characters

| Character | Meaning | Example |
|-----------|---------|---------|
| `*` | Any value | `* * * * *` (every minute) |
| `,` | List | `0 9,17 * * *` (9am and 5pm) |
| `-` | Range | `0 9 * * 1-5` (Mon-Fri) |
| `/` | Step | `*/15 * * * *` (every 15 min) |

## Template Variables

Use placeholders in task descriptions that get replaced when the task is created:

| Variable | Description | Example Output |
|----------|-------------|----------------|
| `{date}` | Current date | `2025-01-15` |
| `{time}` | Current time | `09:00` |

**Example:**

```bash
xpressclaw tasks schedule "Daily report for {date}" --agent atlas --cron "0 17 * * *"
```

Creates tasks like:
- "Daily report for 2025-01-15"
- "Daily report for 2025-01-16"

## Named Schedules

Give schedules memorable names:

```bash
xpressclaw tasks schedule "Summarize HN" --agent atlas --cron "0 9 * * *" --name daily-hn
```

## How Scheduling Works

1. **Run xpressclaw** - Schedules trigger while xpressclaw is running
2. **Persistence** - Schedules are saved and survive restarts
3. **Task creation** - When a schedule triggers, it creates a task
4. **Agent execution** - The agent picks up the task like any other
5. **Budget applies** - Scheduled tasks count against your budget

## Examples

### Daily Code Review

```bash
xpressclaw tasks schedule "Review open PRs" --agent coder --cron "0 10 * * 1-5"
```

### Weekly Summary

```bash
xpressclaw tasks schedule "Create weekly report for {date}" --agent atlas --cron "0 17 * * 5"
```

### Hourly Monitoring

```bash
xpressclaw tasks schedule "Check system health" --agent monitor --cron "0 * * * *"
```

### Monthly Cleanup

```bash
xpressclaw tasks schedule "Archive old logs" --agent atlas --cron "0 2 1 * *"
```

## Timezone

Schedules use the system timezone. To check:

```bash
date +%Z
```

## Troubleshooting

### Schedule not triggering

1. Make sure xpressclaw is running:
   ```bash
   xpressclaw status
   ```

2. Check the task exists:
   ```bash
   xpressclaw tasks list --agent atlas
   ```
