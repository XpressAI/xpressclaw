# Scheduling Work

Schedules create ordinary queued work at the configured time. They can be
recurring cron jobs or one-off follow-ups. Xpressclaw does not add an agent
loop: the selected agent launches its ACP harness and records standard
activity and the result in the same timeline as human messages and tasks.
Codex or Claude may use their own subagents internally.

## Create a schedule

1. Open **Automations → Schedules**.
2. Choose the destination agent.
3. Enter the instruction to send.
4. Set a cron expression and timezone.
5. Save and enable the schedule.

The agent must be runner-ready before scheduled attempts can execute. If it
is not, the queued attempt remains visible with its readiness error.

## Resume work after a delay

Use a one-off follow-up when an agent needs to check a long-running external
job later. Set the target agent, an absolute local date and time, and the
instruction to deliver. At the deadline, Xpressclaw queues one new turn and
then disables the schedule.

Built-in Codex, Claude Code, and OpenCode runners also receive a constrained
`schedule_wakeup` tool. An agent can provide either `delay_seconds` (for
example, `18000` for five hours) or an RFC 3339 `run_at` timestamp with a
timezone offset. The tool is fixed to the current agent, so the future work
resumes that agent's existing ACP conversation. Xpressclaw also binds the
wake-up to the task that armed it: when the deadline arrives, the instruction,
status events, tool calls, and final response are appended to that task's
existing timeline rather than being buried in a separate scheduled task.

One-shot schedules created outside an active agent task and recurring schedules
retain the standalone behavior of creating a new task for each run.

This is the appropriate replacement for `sleep`, a sentinel file, or a goal
loop that keeps taking immediate turns: those mechanisms can delay a process,
but cannot originate a later model turn by themselves. Once `schedule_wakeup`
returns `armed`, the current turn can end.

## Common cron expressions

| Expression | Runs |
|---|---|
| `0 9 * * *` | Every day at 09:00 |
| `0 9 * * 1-5` | Weekdays at 09:00 |
| `0 10 * * 1` | Mondays at 10:00 |
| `0 */6 * * *` | Every six hours |
| `0 17 * * 5` | Fridays at 17:00 |

## Choosing tasks versus workflows

Use a schedule with a single agent for recurring work such as SEO analysis,
dependency checks, or weekly metrics review. Schedule a workflow when the job
needs multiple products—for example, Codex implementing changes and Claude
Code reviewing until approval. Workflow schedules and their input values are
configured in the workflow's **Inputs & trigger** block; see
[Workflows](workflows.md).

## Operational behavior

- Schedules survive control-plane restarts.
- An overdue one-off follow-up runs on the next scheduler check after the
  control plane starts.
- A one-off follow-up fires at most once and cannot be re-enabled after it has
  completed.
- Scheduler checks run once per minute, so a follow-up can begin up to about a
  minute after its deadline.
- If the agent already has an active attempt, the follow-up remains queued
  and starts when that conversation is available.
- Every run records schedule provenance in the Agent task timeline.
- Missed or failed work remains inspectable instead of disappearing into a log.
- Disabling a schedule prevents future dispatches without deleting history.
