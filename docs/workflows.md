# Workflows

Workflows coordinate reusable steps across one or more Agents. Each run is a
durable workflow instance; task steps enter the normal Agent queue and their
declared outputs become available to later steps. A run can be independent or
bound to a Project Conversation, in which case its Tasks and results remain
linked to that shared context.

## Start from a template

Open **Automations → New workflow** to choose one of six working templates:

1. **Goal loop** makes one verified increment at a time until its Agent returns
   `complete`. The workflow cycle guard prevents an accidental infinite loop.
2. **Implementation + independent review** uses reusable `implementer` and
   `reviewer` roles, a draft pull request as the cross-context handoff, and a
   durable GitHub activity wait after the pull request is ready for a person.
3. **Scheduled repository caretaker** checks CI, dependencies, security
   signals, and documentation on a real cron schedule, then follows a healthy,
   changes, or blocked path. Automatic publishing is forbidden; optional edits
   stay local for a person to inspect.
4. **Periodic issue/backlog processor** asks an MCP-capable Agent to fetch and
   normalize a bounded batch from its configured provider, processes the items
   serially and durably, and writes back only when explicitly enabled and
   supported. It works with GitHub, Jira, Linear, or another source without
   depending on a disabled connector trigger.
5. **Requirements → detailed specification** gathers context, drafts a
   high-level design, gives an independent role a clean-session challenge, and
   produces acceptance criteria plus independently useful delivery slices. It
   works for product, policy, operations, research, and software work.
6. **Implement alternatives → choose best** defines a rubric, builds two independent
   implementations, runs common checks, and selects the strongest eligible result.
   It can refine a copy using ideas from the other implementation, keeping it only
   when the original rubric scores it higher and its checks pass. The worker adopts
   and re-verifies the chosen version, then publishes a comparison report.
   Saving a reusable decision to project memory is optional.

The compact **Start blank** action retains the original one-step starter for
people who want an executable empty canvas instead of a primary gallery card.
Every template uses typed inputs and reusable Agent roles rather than fixed
Agent IDs.

The two periodic templates ask for a cron expression and an Agent when they are
created. That selected Agent ID is stored only in `schedule.inputs` so automatic
runs are executable; manual runs can bind the same role to another Agent. Cron
uses server-local time, the new schedule starts enabled, and both its cadence
and binding remain editable in **Inputs & trigger**. For the backlog template,
configure the chosen Agent with the provider's MCP tools first—XpressClaw does
not present generic connector triggers or sinks as functional in this beta.

## Run a workflow

Open **Automations → Workflows** and select **Run**. Workflows may declare
typed inputs, which XpressClaw validates before creating the first task:

```yaml
name: release-report
inputs:
  goal:
    type: string
    description: What should the report investigate?
    required: true
  retries:
    type: number
    default: 2
  options:
    type: json
  worker:
    type: agent
    description: Agent that creates the report.
    required: true
    primary: true

flows:
  main:
    steps:
      - id: report
        agent: "@worker"
        prompt: |
          Investigate @goal. Use these options: @options
```

Input types are `string`, `number`, `boolean`, `json`, and `agent`. An input is
available directly as `@input_name` and in the original run payload as
`@trigger.payload.input_name`. Defaults are applied without changing the saved
workflow. The older `variables` field remains available for internal workflow
defaults and existing definitions.

An `agent` input is a reusable role. Task and wait blocks reference it as
`agent: "@role_name"`; each run binds that role to any configured XpressClaw
Agent. When a run is bound to a Project, every selected role must belong to
that Project. Mark one role `primary: true` to have it reuse New Work's current
Agent selection when switching modes. Every role, including an independent
reviewer, receives its own workflow input. A literal agent ID remains supported
for intentionally fixed workflows and appears in Workflow mode when its Agent
belongs to the selected Project.

Agent selectors are resolved and frozen when a run starts. A projectless run
may use a literal Agent, a typed Agent input, or a template that resolves from
its initial inputs, trigger payload, or variables. It cannot choose an Agent
from a future step's output because XpressClaw must record Project ownership
before inserting the durable run; bind the run to a Project or model that
choice as a typed Agent input instead.

### Start new work through a workflow

The **New Work** composer has separate **Agent** and **Workflow** modes. Agent
mode sends an ordinary task directly to the selected Agent. Workflow mode lets
you choose a manual workflow, fills the composer from its declared input
schema, and opens the first task created by the run.

Workflow mode supports the same string, number, boolean, JSON, and Agent inputs
as the full run form in **Automations**. Agent roles are limited to the Agents
in the selected Project, defaults prefill their fields, and required inputs
must be provided before the workflow can start. Workflows with automatic
triggers or connector sinks remain managed from **Automations**.

### Run a workflow in a Conversation

Open a Project Conversation and choose **Continue with task**, then select a
workflow. The same form collects its typed inputs and binds the workflow
instance to both the Project and Conversation. Each task step links back to the
Conversation, and Agent results are published there as they complete. The
workflow definition remains reusable: running it from another Conversation
creates a new instance with that Conversation's Project boundary.

### Run a workflow for every task

Turn on **Run for every task** in the workflow editor to attach that workflow
once to each ordinary Agent task. XpressClaw waits for the task and any already
queued response to finish before starting the workflow's `main` flow. Hidden
tasks, idle work, native plan items, and tasks created by another workflow are
excluded. A durable `(workflow, task)` attachment prevents retries or restarts
from invoking the default workflow twice.

Default workflows cannot open an input form. A primary `agent` input is bound
to the task's Agent automatically; every other required input must have a
default. Because the policy applies across Projects, task and wait blocks must
use that primary Agent role rather than a fixed Agent. The setting is local
execution policy, so it is stored with the local workflow record instead of in
the portable YAML definition.

Use a `continue` step when a policy or final check should go back to the same
task instead of creating a child task:

```yaml
flows:
  main:
    steps:
      - id: final_ui_check
        type: continue
        prompt: |
          Ensure that there are no messages in the UI which are unnecessary
          to the end user.
```

The fixed prompt is appended as a user message to the source task and queues
one more response from that task's Agent. The prompt, workflow execution, and
queue entry share one database transaction, so the step sends its prompt once
per workflow run even if completion is observed more than once. A `continue`
step requires a source task and is therefore intended for default workflows.

## Wait for an external event

A `wait` block suspends the workflow durably. It does not keep an agent turn or
container alive, and it survives a control-plane restart. GitHub pull-request
waits currently support formal reviews, conversation comments, inline review
comments, or all three:

```yaml
      - id: mark_ready
        type: step
        agent: "@implementer"
        prompt: Push the branch and mark the pull request ready for review.
        outputs:
          pull_request_url: { type: string }

      - id: wait_for_review
        type: wait
        agent: "@implementer"
        event: github.pull_request.activity
        resource: "@mark_ready.pull_request_url"
        timeout: 14d
        on_timeout: flow timed_out

      - id: address_review
        type: step
        agent: "@implementer"
        prompt: |
          Review activity arrived: @wait_for_review
          Inspect the complete PR, address actionable feedback, and report the outcome.
```

The wait uses the bound agent's workspace to discover the repository and its
existing project-scoped GitHub credential. The resource must belong to that
repository. Durations use `s`, `m`, `h`, `d`, or `w`. With no `on_timeout`, a
timeout fails the workflow; otherwise it follows the named step or flow.

## Useful workflow shapes

- **Goal loop:** one agent step emits `complete` or `continue`; a `when` block
  jumps back until the goal is done. The cycle guard caps accidental loops.
- **Implementation and independent review:** bind `implementer` and `reviewer`
  roles, start each review in a fresh conversation, use a draft PR as the
  cross-context handoff, loop requested changes back to implementation, then
  mark the PR ready.
- **PR lifecycle:** publish, durably wait for GitHub activity, triage it, and
  either finish, address changes and re-review, or wait again.
- **Scheduled maintenance/reporting:** put agent bindings and other values in
  `schedule.inputs`, so the same generic workflow runs unattended.
- **Batch work:** a `loop` walks an array serially and durably resumes each
  nested agent task, including after restart.
- **Independent specification challenge:** give the challenger a reusable Agent
  role and `new_session: true`, then pass its structured gaps and recommendations
  to the final drafting step.
- **Implement alternatives → choose best:** define the rubric before building,
  evaluate independent implementations with shared checks, then adopt and verify
  the selected result.
- **Failure handling:** an `on_error` flow catches failed agent tasks; explicit
  timeout flows handle external systems that never respond.

Steps run in order, with explicit `parallel` groups for independent work. A
parallel group waits for all its branches before advancing. GitHub events use
`wait` blocks; arbitrary webhook triggers are not supported.

Each run snapshots its workflow definition when it starts. Editing the reusable
definition affects future runs without changing a run that is already waiting,
looping, or executing a task.

## Parallel alternatives and executable verification

The built-in **Implement alternatives → choose best** workflow completes a task
through a common baseline, a predefined rubric, two independent implementations,
shared verification, selection, adoption, and a final check.

The worker inspects the destination and relevant project memory, then prepares a
reproducible starting point. The coordinator defines mandatory requirements,
weighted quality criteria, expected evidence, and a tie-break rule before either
candidate starts. It creates the shared verification script, incorporating any
existing command the user supplies. Both candidates receive the same baseline,
brief, rubric, and checks. Each builds a complete alternative in its own uniquely
named directory or Git worktree. Different Agents can run concurrently and use
different configured models; two roles bound to one Agent run serially.

After both implementations are ready, command steps run the saved verification
script in each candidate's exact directory. The evaluator compares actual outputs
and source changes against the original rubric, producing a scorecard and rationale.
The engine checks the selected candidate's machine-derived `passed` flag before
allowing adoption. A higher quality score cannot override failed required checks.
A failed or timed-out candidate remains ineligible while the other can still win.
If no eligible result is selected, the workflow publishes the comparison and fails
without applying a candidate to the destination.

`refine_winner` is enabled by default. An independent review looks for specific
ideas from the other implementation that could improve the winner. When there is
a concrete opportunity, the worker creates an unchanged copy of the winner and a
separate experimental refinement. Both are checked in the worker's environment
with the same saved script and rescored against the original rubric. The engine
requires passing checks, confirmation that mandatory requirements still hold,
and a strictly higher numeric score before choosing the refinement. Failed checks,
ties, and lower scores retain the original winner. If the unchanged copy cannot
pass its recheck, the run stops before applying either version. The original
candidate workspaces and change sets remain available throughout.

The worker applies the selected change set, preserving unrelated user changes,
then runs the same verification script against the adopted result. A successful
run ends with a human-readable answer and a downloadable report published through
`publish_task_files`, including the rubric, scorecard, selection rationale,
delivered files, test evidence, and any before/after refinement comparison. Results
also reach a linked Project Conversation.
The optional `remember_decision` input is off by default; when enabled, a separate
step preserves reusable project knowledge only after final verification succeeds.

Agent roles exchange source state and change sets through persisted step outputs,
so candidates can use separate workspaces. Git worktrees belong under `.worktrees/`.
Absolute command directories keep verification tied to the intended candidate even
when an Agent adopts a different active repository between turns.

A `parallel` block contains 1–16 Agent or command steps:

```yaml
      - id: proposals
        type: parallel
        steps:
          - id: direct
            agent: "@ideator_a"
            new_session: true
            prompt: "Find the most direct approach to @problem"
            outputs:
              proposal: { type: string }
          - id: alternative
            agent: "@ideator_b"
            new_session: true
            prompt: "Propose an alternative technique for @problem"
            outputs:
              proposal: { type: string }
      - id: solve
        agent: "@worker"
        prompt: "Compare @direct.proposal and @alternative.proposal, then implement."
```

Branches receive the same input snapshot. Their outputs become available to later
steps after the join. Choose different Agents for simultaneous execution; branches
bound to one Agent use that Agent's serial task queue. The normal worker concurrency
limit still applies. Branch failures are collected until every branch finishes,
then the group enters `on_error` or fails the run. Cancellation retires the group's
queued and running tasks. Branches and their join survive control-plane restarts.
Nested parallel groups and control-flow branches are rejected by validation.

A `command` block runs a shell script directly in the selected Agent's retained
Linux container:

```yaml
      - id: test
        type: command
        agent: "@worker"
        command: "@verification_command"
        cwd: .
        timeout: 5m
        allow_failure: true
      # An Agent can review @test and repair the solution here.
      - id: verify
        type: command
        agent: "@worker"
        command: "@verification_command"
        timeout: 5m
```

Relative working directories start at the Agent's active repository. Absolute
container paths are also accepted, and the runner resolves symlinks to check that
the directory stays inside the Agent's configured workspace. Timeouts default to five minutes
and support the same duration syntax as waits, up to 24 hours. A command exposes
`passed`, `exit_code`, `stdout`, `stderr`, `signal`, `timed_out`, `stdout_truncated`, and
`stderr_truncated`; each output stream is capped at 1 MiB. These results also appear
in the command task and its attempt artifacts. `allow_failure: true` captures
nonzero exits, signals, and timeouts so later steps can compare outcomes. Launch
and transport errors fail the step. The derived `passed` flag is true only for
exit code zero with no signal or timeout; strict command steps require it to be
true. Use this flag for eligibility gates.

Commands use the run's saved definition and execution result. Updating task text
or reporting success cannot override the test gate. A saved command result is
reused after restart. If execution was interrupted before its result was saved,
the command fails with an interruption message; a new workflow run executes it
again. A command's process group is terminated on timeout, disconnect, or exit.

Numeric `when` switches can compare step outputs or a numeric literal with
`>`, `>=`, `<`, or `<=`. For example:

```yaml
      - id: improvement_gate
        type: when
        switch: "@assessment.after_score > @assessment.before_score"
        arms:
          - match: "true"
            goto: flow adopt_refined
          - match: default
            goto: flow adopt_original
```

The comparison runs in the engine using finite numbers. Missing or nonnumeric
operands fail the step instead of accepting a claimed improvement.

The `command` field on a normal Agent `step` still invokes a harness slash command.
Use `type: command` for direct shell execution. Both the visual editor and YAML
support parallel groups and command steps; serial loops also accept commands.

## Trigger a workflow automatically

Choose **Cron schedule** in the workflow's **Inputs & trigger** block, or add a
schedule to YAML:

```yaml
schedule:
  cron: "0 9 * * 1"
  inputs:
    goal: Weekly release-readiness report
    options:
      include_ci: true
    worker: release-agent
```

Cron uses the server's local time and the standard five-field format. A
scheduled workflow must provide every required input that has no default.
The enable switch pauses only automatic runs; users can still run the workflow
manually.

Automatic trigger state is persisted before a run starts. This prevents a
control-plane restart or overlapping scheduler check from starting the same
occurrence twice. The Automations page shows the latest trigger error so a bad
agent or workspace configuration does not fail silently.

Connector-backed triggers and notification sinks from legacy workflow files
remain readable but disabled. Project Conversations now provide the durable
human/Agent coordination surface; future connector events and sinks can target
that model without changing workflow-instance ownership.
