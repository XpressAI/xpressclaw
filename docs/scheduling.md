# Scheduling Work

Schedules create ordinary queued work at the configured time. Xpressclaw does
not add an agent loop: the selected session launches its native product worker
and records the result in the same timeline as human messages and tasks. Codex
or Claude may use their own subagents internally.

## Create a schedule

1. Open **Work → Schedules**.
2. Choose the destination session.
3. Enter the instruction to send.
4. Set a cron expression and timezone.
5. Save and enable the schedule.

The session must be runner-ready before scheduled attempts can execute. If it
is not, the queued attempt remains visible with its readiness error.

## Common cron expressions

| Expression | Runs |
|---|---|
| `0 9 * * *` | Every day at 09:00 |
| `0 9 * * 1-5` | Weekdays at 09:00 |
| `0 10 * * 1` | Mondays at 10:00 |
| `0 */6 * * *` | Every six hours |
| `0 17 * * 5` | Fridays at 17:00 |

## Choosing tasks versus workflows

Use a schedule with a single session for recurring work such as SEO analysis,
dependency checks, or weekly metrics review. Schedule a workflow when the job
needs multiple products—for example, Codex implementing changes and Claude
Code reviewing until approval.

## Operational behavior

- Schedules survive control-plane restarts.
- Every run records schedule provenance in the session timeline.
- Missed or failed work remains inspectable instead of disappearing into a log.
- Disabling a schedule prevents future dispatches without deleting history.
