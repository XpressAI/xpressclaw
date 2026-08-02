# Workflows

Workflows coordinate reusable steps across one or more agents. Each run is a
durable workflow instance; task steps enter the normal agent queue and their
declared outputs become available to later steps.

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
    description: Project context that creates the report.
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
agent/project context. Mark one role `primary: true` to connect it to New Work's
main Agent picker. Other roles (for example, an independent reviewer) receive
their own picker. A literal agent ID remains supported for intentionally fixed
workflows.

### Start new work through a workflow

The **New Work** composer has an optional workflow picker beside the agent
picker. Choosing **No workflow** sends an ordinary task directly to that
agent. Choosing a workflow sends the composer text as its `goal` input and
opens the first task created by the workflow.

A workflow appears in this picker when it declares a string `goal` input and
every other required input is either an `agent` role or has a default. The main
Agent picker binds the primary role, and additional required agent roles appear
beside the composer. Workflows with other required inputs remain available from
**Automations**, where the full typed run form collects them.

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
remain readable but disabled. They will return with channels, which need a
separate event and conversation model.
