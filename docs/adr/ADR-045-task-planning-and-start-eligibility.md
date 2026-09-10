# ADR 045: Task planning and start eligibility

**Status:** Proposed for review with this implementation

## Context

A multi-project workload needs a shared Board, Timeline, and List. The old
Kanban interface made tasks easy to scan, but its arbitrary status moves do
not fit retained ACP workers, elicitation, dependencies, and monitored reviews.
Ordinary tasks also need a durable earliest start without creating another task
when a timer fires.

## Decision

Store `start_after` (qualified UTC timestamp), `backlog`, and `position`
on the existing task. Reuse the SQLite queue and the existing dispatcher poll;
do not create a parallel timer or a schedule that creates another task.
Both claim paths apply the date and Backlog gates, dependency checks, terminal
state, and Project deletion state inside an immediate transaction. A future
queued attempt does not reserve an execution lane. Enqueue coalesces an existing
queued item; continuation, retry, and recovery therefore retain the same gate.
The Backlog flag defaults to false: New Work and agent `create_task` remain To do.
Creation can explicitly park a task in Backlog before any dispatch is possible.
Moving to To do preserves the date, and date editing alone keeps Backlog parked.
These planning properties travel with explicit Project sync; the local revision
does not.

Planning mutations have their own narrow API. A task revision detects stale
clients, and actual queue/attempt ownership is checked in the same transaction.
Claimed, running, waiting-for-input, review-monitored, terminal, and native plan
rows cannot be moved with planning controls. Queueing or parking in Backlog does not
claim execution, finish work, bypass dependencies, or interrupt a worker.
Assignment follows the Project boundary and moves queued attempt ownership.
Reordering adopts the neighbor's priority and persists order within that
priority, which the dispatcher uses. Explicit actions and keyboard shortcuts
invoke the same mutations as dragging.

All three views use the same filtered, paginated planning query. Derived lanes
separate queue, schedule, work, input, dependencies, Backlog, review, and terminal
work. The timeline uses earliest-start markers, observed start timestamps, and
completed task lifetimes; it does not estimate completion. Dates use the
browser's local timezone and the API requires qualified timestamps.

On narrow workspace panes, primary controls stay at the bottom, the board has
a lane selector, and the timeline becomes an agenda. Native modal dialogs provide
focus containment, Escape, and bottom sheets on phones. Dragging has explicit
Move, Assign, Priority, and Schedule alternatives.

## Consequences

The database migration is additive. Existing tasks stay immediately eligible
unless normal queue rules block them. No timer recovery pass or second task ID
is required. Planning a non-running task schedules its next queued turn; changing
an already executing or terminal task requires its existing detail lifecycle.
A single date gate applies to every later continuation until cleared, so a
Backlog task or future deadline also holds user-triggered continuations.

Board/Timeline pages contain up to 100 tasks and List pages 20, with server-side
scoped counts, search, and filters. Sorting across filtered cards also preserves
unseen tasks' relative order. Periodic refresh updates live lanes; edits keep
their original revision until submitted so polling cannot silently erase a
concurrency conflict.
