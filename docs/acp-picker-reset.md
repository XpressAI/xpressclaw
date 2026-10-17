# ACP picker reset — bounded replacement for PR #263

This replacement separates **observed ACP state**, **explicit task preferences**,
and **turn execution**. The old PR and its uncommitted work remain reference only.

## User-visible contract

- Opening an initialized task's picker requires no chat message. Its catalog
  comes from ACP, not a provider/model list maintained here.
- An idle connected edit uses `session/set_config_option` without prompting.
  Dependent reasoning choices follow the returned/notified schema. Late schema
  notifications are refreshed without requiring a message.
- Busy/disconnected edits are saved as explicit task preferences and visibly
  marked **pending**, including after page reload. Saving never queues work.
- An uninitialized context has an explicit unavailable state. Opening a picker
  does not launch containers, create a native session, or contact an LLM.
- Reassignment, runner changes, abandoned forks, and long activity histories
  cannot supply the wrong catalog.

## Observations are not instructions

`GET /api/tasks/{id}/config` is the frontend's only observed-state source. It
returns `context` (Agent, runner, native session), `availability` (`live`,
`cached`, `busy`, `unavailable`), `config_options`, `modes`, and the durable
`event_id` when applicable. It also reports the explicit preference revision,
`pending` edits, whether a different model is pending, and `has_preferences`.

The reusable-task-session selector is shared with dispatch. Idle reads use
completed/interrupted sessions; busy inspection can use an active attempt. An
active attempt without a native ID does not borrow a previous catalog. A failed
fork is not a fallback; updates on the same reusable native session can remain
valid after a failed turn. Config-options notifications persist the synchronized
complete control snapshot, retaining independent legacy mode catalogs while
updating or removing mirrored reasoning aliases. The connection retains a control
recorder for each native session; explicit edits bind it to the requesting task.
Replies and notifications are persisted in cache-update order, including idle
notifications after the configuration grace period. HTTP never republishes an
older response over a newer notification, and GET remains read-only. Only control
observations are retained, not a completed prompt's transcript or output state.
Activity history is not another schema source.

Displayed selections are presentation only: neither normal messages nor
visualization follow-ups send the displayed control map. Therefore loading a
cache cannot promote all of its model-dependent defaults to strict requirements.

## Explicit preference edits

`POST /api/tasks/{id}/config` accepts `context`, `revision`, `config_id`, and
`value`. It validates against the currently resolved ACP schema. Assignment,
context, or optimistic-revision conflicts return 409 without saving the edit;
the composer refreshes without losing its draft. Invalid choices return 400.

Only explicitly edited values are persisted under `tasks.context.acp_preferences`,
scoped to the assigned Agent and canonical runner. Existing task context is
preserved. A model edit clears previously explicit model-dependent preferences;
reasoning edits are unavailable until the selected model advertises its schema.
Independent task/workflow requirements and runner defaults remain separate.

The preference is saved before attempting live application. If a turn has become
active or the connection is unavailable, it stays pending. If the adapter rejects
a previously advertised live choice, the response explicitly says it was saved
but not applied; the user can choose another model or clear the preference.
Neither an RPC failure nor enqueueing a message counts as application. Matching
live state or a matching observation newer than the saved preference acknowledges
it. Preferences stay explicit/sticky for this task, not provider-wide defaults.

`POST` with `reset: true` clears the explicit task preference map. The UI exposes
**Clear task preferences**, including when a former catalog is unavailable.
This restores normal agent/workflow configuration on future turns; it does not
claim to reconstruct or reset an adapter's original remote-session defaults.

## Application boundary, without queue redesign

Dispatch reads one preference snapshot **before asynchronous repository
preparation**. An edit after that boundary applies to a later dispatch, never by
replacing the current turn's captured input. Automatic continuations naturally
use the same task-preference rule; there is no message ownership to infer for
these preferences and no need to modify queue creation or repair.

Preferences are applied after agent/task defaults, and only within their recorded
Agent/runner scope. Legacy explicit message configuration newer than a preference
can still override it. Older message snapshots cannot resurrect options the user
cleared or replaced. Legacy message delivery is not redesigned in this PR.

Model preferences override the runner's default model. Optional reasoning defaults
that became incompatible with that model yield even if the picker already applied
the model before the next turn. Explicit workflow/message requirements still
validate strictly.

## Validation and review scope

Regressions cover read-only discovery beyond capped history, scope/resumability,
explicit edit persistence/CAS, invalid/stale edits, pending-model dependency
suppression, acknowledgement, reset, frozen dispatch preferences, model-default
precedence, delayed schemas surviving disconnect (including task ownership and
read-only inspection), and browser workflows without message submission.

A reproducible violation of this contract blocks the PR. General automatic-turn
prompt fencing, orphaned-queue repair, and legacy message-delivery hardening stay
separate unless demonstrated to be dependencies of this picker path. After two
non-converging review rounds, stop and report scope tradeoffs rather than expanding
indefinitely. No merge, deployment, package rebuild, or new-session discovery is
included.
