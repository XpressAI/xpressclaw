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

flows:
  main:
    steps:
      - id: report
        agent: release-agent
        prompt: |
          Investigate @goal. Use these options: @options
```

Input types are `string`, `number`, `boolean`, and `json`. An input is
available directly as `@input_name` and in the original run payload as
`@trigger.payload.input_name`. Defaults are applied without changing the saved
workflow. The older `variables` field remains available for internal workflow
defaults and existing definitions.

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
