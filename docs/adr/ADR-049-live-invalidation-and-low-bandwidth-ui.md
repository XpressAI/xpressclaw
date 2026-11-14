# ADR 049: Live invalidation stream and low-bandwidth UI transport

**Status:** Proposed for review with this implementation

## Context

The workspace UI relied on independent `setInterval` timers across the shell,
Agent views, and Task views without in-flight guards, visibility awareness, or
HTTP compression:

- `TaskView` gated `poll()` on its cached local `task.status`, so completed or
  blocked tasks reopened by one-shot scheduled wake-ups, workflow continuations,
  or GitHub review activity never updated until a full reload or user message,
  while `/api/tasks/{id}/config` still polled every 3 seconds.
- `SessionTab` polled `GET /api/sessions/{id}` every 2.5 seconds, serializing up
  to 50 full artifacts, 100 session events, and 70 attempts (multi-megabyte JSON
  responses on active Agents) when the view only needed session status, queued
  attempt count, and active task count.
- `WorkspaceShell` polled `/api/schedules` on every page every 3 seconds,
  transferring all historical completed one-shot wake-up prompts even when the
  Automations sidebar was closed.
- Eager static view imports loaded the full route graph on initial bootstrap,
  and responses were served uncompressed without `ETag` revalidation.

On high-latency or constrained cellular connections, overlapping timers queued
behind browser connection limits and transferred tens of megabytes within
seconds.

## Decision

1. **Database-driven domain revisions and SSE invalidation (`/api/ui/stream`, `/api/ui/revisions`):**
   - `Database` tracks monotonic `u64` revision counters per UI domain
     (`projects`, `agents`, `conversations`, `tasks`, `sessions`, `schedules`,
     `workflows`) via SQLite `update_hook`, `commit_hook`, and `rollback_hook`,
     plus explicit bumps for in-memory runtime changes (configuration reloads
     and live ACP elicitations).
   - `GET /api/ui/stream` streams coalesced `UiRevisions` snapshots over SSE
     behind the standard user session boundary, and `GET /api/ui/revisions`
     provides a tiny snapshot for fallback checks.
   - Protected `GET`/`HEAD` API responses include a compact `x-ui-revisions`
     header stamped from the pre-read `UiRevisions` snapshot before the handler
     runs. On fresh page loads, this establishes an exact revision baseline
     with zero extra requests when idle while reconciling any mutation that
     completes between an initial `GET` read and the first SSE frame (falling
     back to a one-time baseline reconciliation when headers are absent).
   - A single shared browser client maintains one stream per tab, triggers
     domain-scoped single-flight refreshes when revisions advance (including
     when a completed task or conversation receives a scheduled wake-up),
     suspends hidden tabs/offscreen panes until visible, and falls back to
     adaptive single-flight `setTimeout` checks when SSE is unavailable.

2. **Right-sized endpoints and schedule history filtering:**
   - `GET /api/sessions/{id}` returns lightweight session status, active/queued
     attempts, and `active_task_count` by default (`include_details=true` opts
     into full history/artifacts).
   - `GET /api/sessions/{id}/events` supports filtering by `event_type` so
     `TaskView` and `RunnerTab` fetch only the latest command/config metadata
     rather than 100 full session events.
   - `GET /api/tasks/{id}/messages` supports incremental `after_id` reads, and
     `/api/tasks/recent-by-agent` omits large task descriptions and contexts
     unused by sidebar navigation.
   - `GET /api/schedules` excludes completed disabled one-shot wake-ups by
     default unless `include_completed_once=true` is requested, and Automations
     views provide an opt-in toggle to inspect completed wake-ups.
   - Scheduled conversation wake-ups also notify `ConversationEventBus` so open
     conversation streams update immediately.

3. **Transport compression, asset caching, and lazy view loading:**
   - `xpressclaw-server` applies HTTP response compression (gzip and brotli)
     for compressible responses while explicitly excluding `text/event-stream`
     so SSE frames flush immediately.
   - Embedded frontend assets emit `ETag` headers and honor `If-None-Match`.
   - `WorkspaceContent`, `ProjectView`, and `SettingsView` lazy-load secondary
     views on demand so opening one page transfers only its active bundle.

## Validation

Verify scheduled task and conversation wake-ups update open `completed` views
without reload, idle pages issue zero background polls while SSE is connected,
slow/offline fallback never overlaps in-flight requests, completed one-shot
wake-ups are excluded by default and visible via opt-in toggle, SSE frames are
not buffered by HTTP compression, and Rust + Playwright suites pass.
