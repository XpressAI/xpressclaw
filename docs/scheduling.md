# Scheduling Tasks

XpressAI supports cron-style scheduling for recurring tasks. Agents automatically pick up scheduled tasks when they trigger.

## Quick Start

```bash
# Schedule a task to run every day at 9am
xpressai tasks atlas schedule "Check for updates" --cron "0 9 * * *"

# List schedules
xpressai tasks atlas schedules

# Remove a schedule
xpressai tasks atlas unschedule <schedule-id>
```

## Cron Syntax

XpressAI uses standard 5-field cron syntax:

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

Use placeholders in task titles that get replaced when the task is created:

| Variable | Description | Example Output |
|----------|-------------|----------------|
| `{date}` | Current date | `2025-01-15` |
| `{time}` | Current time | `09:00` |

**Example:**

```bash
xpressai tasks atlas schedule "Daily report for {date}" --cron "0 17 * * *"
```

Creates tasks like:
- "Daily report for 2025-01-15"
- "Daily report for 2025-01-16"

## Named Schedules

Give schedules memorable names:

```bash
xpressai tasks atlas schedule "Summarize HN" --cron "0 9 * * *" --name daily-hn
```

The name appears in `schedules` output and can make management easier.

## Managing Schedules

### List Schedules

```bash
xpressai tasks atlas schedules
```

Output:
```
Scheduled tasks for @atlas

[enabled] daily-hn
    ID: 9af0bf1b
    Cron: 0 9 * * *
    Task: Summarize top 10 HN stories
    Next run: 2025-01-16 09:00
    Run count: 5
```

### Remove a Schedule

Use the schedule ID (or prefix):

```bash
xpressai tasks atlas unschedule 9af0bf1b
# or just the prefix
xpressai tasks atlas unschedule 9af0
```

## How Scheduling Works

1. **Daemon required** - Schedules only trigger when the daemon is running (`xpressai up -d`)
2. **Persistence** - Schedules are saved to the database and survive restarts
3. **Task creation** - When a schedule triggers, it creates a task on the task board
4. **Agent execution** - The agent picks up the task like any other task
5. **Budget applies** - Scheduled tasks count against your budget

## Examples

### Daily Code Review

```bash
xpressai tasks coder schedule "Review open PRs and leave comments" --cron "0 10 * * 1-5"
```

### Weekly Summary

```bash
xpressai tasks atlas schedule "Create weekly progress report for {date}" --cron "0 17 * * 5" --name weekly-report
```

### Hourly Monitoring

```bash
xpressai tasks monitor schedule "Check system health and alert on issues" --cron "0 * * * *"
```

### Monthly Cleanup

```bash
xpressai tasks atlas schedule "Archive old logs and clean temp files" --cron "0 2 1 * *"
```

## Timezone

Schedules use the system timezone where the daemon is running. To check:

```bash
date +%Z
```

## Troubleshooting

### Schedule not triggering

1. Make sure the daemon is running:
   ```bash
   xpressai status
   ```

2. Check the schedule is enabled:
   ```bash
   xpressai tasks atlas schedules
   ```

3. Verify the cron expression:
   ```bash
   # Use an online cron expression tester to verify
   ```

### Missed schedules

If the daemon was stopped when a schedule was supposed to trigger, that run is skipped. The schedule will trigger at the next scheduled time.

### Check logs

```bash
xpressai logs -f | grep -i schedule
```
