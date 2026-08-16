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
6. **UI regression tester** runs scoped browser flows, records concrete
   evidence, classifies findings, and can optionally make one narrow local fix
   and retest pass. It never publishes that fix automatically.

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
- **Evidence-based UI regression:** keep the browser, optional local fix, and
  retest in one Agent role so the workspace and captured evidence remain
  coherent; branch around the fix when `allow_fix` is false.
- **Failure handling:** an `on_error` flow catches failed agent tasks; explicit
  timeout flows handle external systems that never respond.

Workflow execution is deliberately sequential today. Parallel fan-out and
arbitrary webhook events are not represented as if they already work; they can
be added on top of the persisted role, cursor, and wait model.

Each run snapshots its workflow definition when it starts. Editing the reusable
definition affects future runs without changing a run that is already waiting,
looping, or executing a task.

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
