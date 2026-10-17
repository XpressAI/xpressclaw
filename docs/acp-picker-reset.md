# ACP picker reset — bounded replacement for PR #263

Status: **draft, backend foundation only**. This is not yet the end-to-end picker
replacement. The existing frontend is deliberately unchanged in this first slice.

## User-visible contract

- Opening an initialized task's picker does not require a chat message.
- Model/reasoning choices come from ACP, not a provider catalog maintained here.
- Changing a model on an idle, connected session refreshes dependent controls.
- Busy/disconnected selections must be labeled pending, not applied. This behavior
  must not be silently removed to simplify the implementation.
- A task with no initialized session has an explicit unavailable state. Opening
  the picker does not launch containers, create sessions, or contact an LLM.
- Reassignment, runner changes, and failed forks cannot contribute another
  context's controls. Long activity histories cannot hide the picker.

## State ownership

There are three different things; they must not be conflated:

1. **Observed schema and values**: a backend snapshot of a specific native
   conversation. Durable events are an observation cache, never user preferences.
2. **Explicit user edits**: only the settings the user actually changed, with a
   clear owner and pending/applied result. Do not serialize the entire displayed
   schema into every chat message.
3. **Turn execution**: consumes explicit intent at a defined boundary. Existing
   workflow requirements and external callers remain distinct from UI defaults.

`GET /api/tasks/{id}/config` is the sole intended frontend source for observed
controls. It returns `context` (Agent, runner, native session), `availability`
(`live`, `cached`, `busy`, `unavailable`), `config_options`, `modes`, and a durable
`event_id` where available. Event IDs are comparable within a context, not across
native-session or runner changes. Activity events should only trigger a refresh.

The selector for reusable task sessions is shared with the dispatcher. Idle
reads use completed/interrupted sessions; busy inspection can use an active
attempt. An active attempt without a native ID does not borrow an old catalog.
An abandoned fork is not a fallback; updates on the same reusable native session
can remain valid after a failed turn. No new database table or queue mechanism is
required for these read-only operations.

## This first slice

- Reuse the tested ACP configuration transport from #263: prompt-free inspection
  and mutation, response/notification reconciliation, model-before-dependent
  ordering, and keeping healthy connections available after request failures.
- Add the canonical, read-only snapshot endpoint independent of recent history.
- Share task-session selection with dispatch and reject an obsolete retained
  runner. Add no new HTTP mutation yet.
- Keep the old PR and its uncommitted changes untouched for reference.

## Remaining before this draft can be ready

- Implement the explicit-intent mutation contract and wire the picker to the
  snapshot endpoint. Separate observed state, pending edits, and confirmed edits.
- Specify persistence and the application boundary for pending edits. Preserve
  busy/disconnected use without copying every observed option to messages.
- Add browser regressions for the actual user workflow, including slow refreshes
  and recovery. Do not claim this backend slice fixes the UI on its own.
- Run focused integration checks and one bounded review pass on the complete
  contract before requesting approval.

## Review and scope boundaries

A reproducible violation of the contract above blocks this replacement. A
pre-existing general message-delivery bug belongs in a separate follow-up unless
it is a demonstrated dependency of the new picker path. In particular, do not
copy #263's automatic-continuation fences or orphaned-queue repair changes merely
because they were previously reviewed there. Do not silently drop correctness
requirements to keep a diff small either.

After two non-converging review rounds, stop and report the remaining issues and
scope tradeoffs. Automated review is evidence, not permission for an unbounded
rewrite. No merge, deployment, package rebuild, or new-session discovery is
included in this work.
