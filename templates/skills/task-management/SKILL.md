---
name: task-management
description: Manage tasks, subtasks, and scheduled recurring work. Use when the user asks to track work, create reminders, schedule recurring actions, or break down complex goals into steps.
---

# Task Management

You can create, track, and manage tasks on a kanban board. Tasks can have subtasks, be assigned to agents, and trigger scheduled recurring work.

## Tools Available

- `create_task(title, description, agent_id, priority, parent_task_id, conversation_id)` — Create a task. Priority: 0=low, 1=normal, 2=high, 3=urgent.
- `list_tasks(status, agent_id)` — List tasks, optionally filtered by status or agent.
- `get_task(task_id)` — Get full task details.
- `update_task(task_id, status, title, description)` — Update a task's fields.
- `complete_task(task_id, result)` — Mark a task as completed with a result summary.
- `list_subtasks(parent_task_id)` — List subtasks of a parent task.

## When to Create Tasks

- The user asks you to do something complex → break it into subtasks
- The user says "remind me to..." or "every day at..." → create a one-off or recurring schedule
- Work must pause for an external job and continue later → arm `schedule_wakeup`; do not sleep or poll
- You need to track progress on multi-step work
- You want to hand work off to another agent

## Subtasks

Break complex work into subtasks by setting `parent_task_id`:

```
1. create_task(title="Build user dashboard", description="Full dashboard with charts")
2. create_task(title="Design layout", parent_task_id="<parent_id>")
3. create_task(title="Implement API endpoints", parent_task_id="<parent_id>")
4. create_task(title="Add charts", parent_task_id="<parent_id>")
```

## Scheduling

Create recurring tasks with cron schedules:

- `create_schedule(name, cron_expression, task_title, task_description, agent_id)` — Create a recurring schedule.
- `schedule_wakeup(message, delay_seconds | run_at, name)` — Start exactly one future turn in the current project's existing conversation.
- `list_schedules()` — List all schedules.
- `delete_schedule(schedule_id)` — Remove a schedule.

### One-off wake-ups

Use `schedule_wakeup` when the current turn needs to end and work should resume
after a delay. Provide exactly one of:

- `delay_seconds` — resolved against the Xpressclaw control-plane clock.
- `run_at` — an RFC 3339 timestamp including a timezone offset.

After the tool confirms the wake-up is armed, end the current turn. A shell
timer or sentinel cannot initiate a later model turn.

### Cron Format

Standard 5-field cron: `minute hour day-of-month month day-of-week`

Examples:
- `0 9 * * *` — Every day at 9:00 AM
- `0 9 * * 1-5` — Weekdays at 9:00 AM
- `*/30 * * * *` — Every 30 minutes
- `0 0 1 * *` — First day of every month at midnight

### Example: Daily Status Report

```
User: "Send me a daily status update at 9am"

1. create_schedule(
     name="daily-status",
     cron_expression="0 9 * * *",
     task_title="Generate daily status report",
     task_description="Summarize yesterday's completed tasks, today's priorities, and any blockers.",
     agent_id="<your_agent_id>"
   )
```

## Linking Tasks to Conversations

Pass `conversation_id` when creating a task to get status updates posted back to the conversation as the task progresses.

## Task Status Flow

```
pending → in_progress → completed
                      → failed
          blocked (waiting on something)
```
