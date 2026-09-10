# Plan tasks across projects

Open **Tasks** and choose **Board**, **Timeline / Gantt**, or **List**. The Tasks
sidebar also links to these views. Filters narrow the same task collection by
Project, Agent, and status. Search includes task text and saved conversation
history. Project and Agent names remain on every card. Durable subtasks appear
with a Subtask label; current-turn ACP checklist rows stay in their parent.

Filter choices take effect only with **Apply filters**. **Reset** clears the draft
choices; closing the sheet with × or Escape discards them. Applied filters remain
in place during live workspace updates.

## Board and queue order

The board distinguishes Backlog, To do, Scheduled, Working, Needs you, Blocked,
Review, and Done. To do is the default for New Work and agent `create_task` calls.
Use **Add to Backlog** or **Start in → Backlog** to collect work for later without
starting an agent. Backlog remains parked when you assign an agent, edit dates,
or send a continuation. A date remains visible when dependencies or Backlog also
prevent execution. Unassigned tasks and tasks without a queued turn are labeled.

Drag an editable card onto another card in its lane to order it before that
card. This also adopts the destination card's priority; the queue chooses higher
priorities first, then this order, subject to its normal execution rules.
**Actions → Move up / Move down** and **Alt + Arrow up / Arrow down** on the
Actions button do the same without dragging. Priority can also be set directly.

Supported lane moves are:

- **To do** leaves Backlog and queues one turn, preserving the earliest start and dependencies. A future date places it in Scheduled.
- **Backlog** parks work without stopping a running worker. New Backlog tasks do not allocate a queued turn.
- **Scheduled** opens the date/time picker. Choose a date and **Move task** to schedule and release it from Backlog. **Save schedule** edits only the date.

Work that is running, asking for input, blocked on an execution problem, awaiting
review, or finished must use its task details for the relevant lifecycle action.
Dragging cannot start an agent, complete a task, accept a review, or interrupt
execution. Dependencies remain enforced. Required durable child tasks must also
complete before a queued parent turn can run, including after a restart. ACP
checklist rows do not block parent dispatch. Assignment is available within the
current task's Project and transfers its queued work to the selected Agent.

## Start no earlier than

Set **Start no earlier than** when creating a task, from its Actions sheet, or
through **Plan / schedule** in task details. The local timezone is named beside
the picker; the server stores an unambiguous UTC timestamp. Clearing the date
removes only the date constraint; it does not clear Backlog.

The task cannot acquire an execution slot before this threshold. At the
threshold it becomes eligible, subject to dependencies, Agent availability,
Project state, and normal queue rules. Other eligible tasks can pass it in the
queue. An overdue date is eligible immediately; it does not replay missed runs.
The same task and queued attempt survive a restart without a second task firing.

Edits to a claimed/running, terminal, input-waiting, or review task are rejected.
For an idle task, a new schedule applies to its next turn. The threshold and Backlog
also apply to subsequent continuations and retries. Cancellation retires queued
work through the existing cancellation flow; a past schedule never reopens it.
Reopening a task explicitly retains its date and Backlog until changed.

The timeline shows a diamond at the earliest start and actual start timestamps.
A span is shown only for an observed start and recorded task completion; that
lifetime includes waiting periods. There are no invented duration or completion
estimates. Tasks without a planned start remain visible as Unscheduled. Dependency
links open their existing task details. Group by Project or Agent and navigate
by two-week windows, or enter a date. Dragging a marker preserves its local time
on the chosen day; the picker gives precise control.

## Phone and narrow panes

Board, Timeline, List, Filters, and New Task controls sit at the bottom of the
pane. The board's lane selector and the timeline's agenda avoid precise dragging.
Every editable card has an Actions button with Move, Assign, Priority, and Schedule
controls. Sheets respect safe areas and the visible keyboard viewport, scroll
independently, and support screen readers and Escape. Closing a task sheet returns
focus to its card when the card remains in view.

Views refresh every five seconds while visible. Changes are confirmed by the
server. If another client or an Agent changed the task, the edit is rejected,
the latest state is loaded, and the error stays visible for review.
