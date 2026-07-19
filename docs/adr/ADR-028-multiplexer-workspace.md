# ADR-028: Multiplexer Workspace

## Status

Accepted

## Context

XpressClaw needs to supervise several long-running native agent sessions without
requiring the user to watch terminal panes. The earlier navigation replaced the
project list whenever a task, workflow, or settings page opened. That hid the
status of other projects at precisely the moment a user needed to notice an
agent waiting for input or failing.

Tasks also displayed chat separately from technical activity. That made an ACP
turn difficult to reconstruct and encouraged automatic scrolling that pulled a
reader away from older context.

## Decision

The desktop and responsive web interface use a persistent workspace:

- the project sidebar remains visible and prioritizes waiting, failed, and
  working sessions;
- resources open as tabs, and wide viewports may split them into multiple
  panes;
- projects, tasks, workflows, and settings share the same tab model;
- task messages and ACP activity form one chronological transcript;
- technical events are quiet, single-line rows that expand on demand;
- new activity follows the viewport only while the reader is already at the
  bottom; otherwise the UI offers a jump-to-latest control;
- long transcripts page backward without losing the reader's scroll position;
- mobile uses the same information architecture with one active pane.

The workspace stores only navigation layout locally. Tasks, attempts, messages,
and events remain durable server-side state.

## Consequences

The interface behaves like an outcome-oriented multiplexer rather than a set of
unrelated administration pages. A user can monitor many sessions while staying
inside the work that currently needs attention. Browser regression tests must
cover tab restoration, splitting, mobile navigation, transcript ordering, and
scroll-follow behavior.
