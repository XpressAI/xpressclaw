# ADR 048: Delegated results and coordination turns

**Status:** Proposed for review with this implementation

## Context

The task queue used required children as both a completion gate and a dispatch
gate. A manager's scheduled wake-up was saved but could not run while its
children remained open. Separately, a worker's completed response could mark
its assignment completed even when its output described unfinished work.

## Decision

Keep task completion separate from response execution. Explicit user messages,
scheduled checks, and current delegated results can resume a parent while
required children remain open. Bare continuations and workflow-owned prompts
retain their dependency gates. Existing task priority chooses among eligible
responses, with one active response per Agent.

Dispatched required delegations acquire a durable parent-review gate. A
finished worker response is a result to review, including when it reports a
blocker. The parent explicitly accepts the current attempt, requests revisions
on the same child, or records a wait. Acceptance cannot bypass nested required
work or pull-request review. Independent tasks, native plan rows, platform
work, and workflow-owned steps keep their existing ownership policies.

Reuse the delegation notification journal and queue. Results are deduplicated
by parent, parent Agent, child, attempt, and outcome; pending notifications
coalesce. Reviewed waits do not cause polling turns, but a new outcome or
attempt can notify again. Obsolete notification-only turns are retired without
discarding user guidance or scheduled checks.

Migration 56 retains notification receipts and adds the completion-review gate.
Open native delegations adopt it; historical completed work is preserved.
Deleting a parent retains its surviving native children as independent,
unfinished work rather than leaving an approval gate with no owner.
Cancelling a parent likewise detaches children waiting on its review, without
completing or retrying them. Their own nested reviews and other completion
requirements remain in place; accepted historical children retain their links.

## Validation

Exercise incomplete worker responses, parent-only acceptance, stale attempts,
revisions, waits, queued wake-ups, unchanged scheduling constraints, upgrade
receipts, and parent deletion/cancellation. Verify API capability boundaries,
MCP transport, and UI distinction between a queued coordination response and a
result awaiting parent review.
