# ADR-035: Managed pull-request review lifecycle

## Status

Accepted

## Context

Generic GitHub publishing guidance often defaults to draft pull requests. That
default is useful while an agent is still assembling a change, but it is wrong
at the end of an ordinary XpressClaw task: automated review does not start and
the task can appear complete before a person has had a chance to respond.

Review also outlives one model turn. Keeping a worker or container busy while
waiting wastes resources, while completing the task lets the next queued task
change the same workspace before review feedback is addressed.

## Decision

The bundled, project-scoped GitHub MCP manages pull requests created by
ordinary tasks:

- `pr create` normalizes every supported long, assigned, and bundled shorthand
  form of an inherited draft flag and publishes a ready pull request, and
  managed tasks reject `pr ready --undo` rather than allowing a
  ready pull request to become draft again. Managed dry-run creation is also
  rejected because no real pull request exists to register. Before `pr create`
  or `pr ready` runs, the MCP durably arms a
  fail-closed completion gate; successful registration atomically replaces it
  with the real PR. When `pr ready` names a pull request or branch explicitly,
  that same target is used to resolve its URL. A control-plane timeout therefore
  cannot leave a published PR unmonitored or register the current branch's PR
  by mistake.
- Registration is accepted only for the task's assigned agent and configured
  repository. Hidden tasks and workflow-owned tasks are excluded. Validation,
  task activation, and sentinel or pull-request persistence share one
  transaction, so cancellation or reassignment cannot be overwritten by a
  registration request that started earlier.
- XpressClaw polls GitHub durably, starting at 15 seconds and backing off to
  five minutes. The worker process and container do not need to remain busy.
- New external reviews and comments enqueue one continuation in the same task
  conversation. The prompt requires inspection of the whole PR, all unresolved
  threads, and CI; addressed threads are replied to and resolved, and an
  explicit re-review is requested when the configured reviewer requires one.
  Feedback insertion, the terminal-status check, task activation, and queue
  creation are one transaction, so feedback fetched before a user cancellation
  cannot reopen the cancelled task. Moving a closed or expired review to
  waiting-for-input likewise checks the current task status and writes the
  attention state and explanatory message atomically; a task cancelled during
  GitHub I/O stays cancelled. Cancellation also transitions a live attempt,
  marks its task cancelled, and retires every waiting or attention review
  monitor before asynchronous container cleanup, all in one transaction.
  Approval/merge
  finalization rechecks the task, records the terminal monitor result,
  completes native-plan children, appends the result message, and completes
  the task in one transaction, so stale GitHub responses cannot complete a
  cancelled task or mutate its plan.
- Every page of unresolved threads is rechecked and exposed through the
  project-scoped `pr thread list` command, and can trigger an hourly reminder
  after an agent turn, rather than later pages being silently abandoned.
- The task and that agent's queue lane remain active until every registered PR
  is merged or approved. Reassigning the task atomically transfers its review
  monitor, queued continuations, work attempts, and queue-lane reservation to
  the new agent only after verifying that agent is bound to the same GitHub
  repository; reassignment is rejected when the repository cannot be verified,
  differs, a turn is actively running, or the destination agent's lane is
  already reserved by another active review task. Review feedback rereads the
  task's assignment and creates its queue item, attempt, and session event in
  one transaction, so a reassignment during the GitHub request cannot recreate
  stale work for the former agent.
  Approval means a formal approved review, an
  unambiguous `+1`, `LGTM`, or `approved` review/comment, or a thumbs-up
  reaction on the PR summary, in every case from someone other than the PR
  author. Submitted-review text follows each reviewer's latest review state, so
  an earlier `LGTM` cannot override a later change request; standalone comments
  remain independent signals.
- Monitoring expires after 14 days. A timeout or a PR closed without merge
  moves the task to waiting for input; it does not count as completion.

Workflow-owned tasks retain their explicit semantics. A reusable workflow may
intentionally create a draft PR, bind multiple agents, and use its own wait and
branch steps. The implicit ordinary-task lifecycle must not rewrite that flow.

## Consequences

Ready pull requests immediately enter GitHub's configured review automation,
and ordinary tasks no longer report completion while review is outstanding.
Feedback handling survives control-plane restarts without consuming model or
container time. Because the queue lane is reserved, a long review can delay
other tasks assigned to the same agent; users can explicitly cancel the task
when they need to override that ordering.

The GitHub MCP source is embedded in the XpressClaw binary at runtime. An app
upgrade therefore applies lifecycle behavior even when an otherwise compatible
runner image is cached.
