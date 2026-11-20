# ADR-031: Workflow Execution and Triggers

## Status

Accepted

## Context

The workflow engine could create and advance workflow instances, but its only
manual entry point was a little-discoverable API and untyped YAML variables.
The visual editor treated all triggers as unavailable connector automation.
Users could define a multi-agent workflow without a clear way to start it,
describe its required data, or make it run automatically.

Connector channels are intentionally deferred until their conversation model
returns. Workflow execution should not remain blocked on that separate feature.

## Decision

Manual execution is a first-class workflow capability and is always available.
Workflow definitions may declare typed `inputs` with descriptions, required
flags, and defaults. The control plane validates and resolves those inputs
before creating an instance; templates receive them as top-level variables and
through `trigger.payload`.

A workflow may also declare one recurring `schedule` with a standard five-field
cron expression and saved input overrides. Cron is evaluated in the server's
local timezone. The workflow's enabled flag controls automatic starts but does
not prevent a person from starting a manual run.

The control plane persists the last claimed occurrence, trigger count, and
latest error on the workflow record. It claims an occurrence atomically before
starting an instance, making automatic starts duplicate-safe across restarts or
overlapping scheduler loops. Editing the schedule resets its occurrence cursor
from the edit time without erasing its historical trigger count.

Legacy connector triggers and sinks remain compatibility-only and disabled.
They are not conflated with cron schedules.

## Consequences

- Workflows can be run from the Automations list or editor with validated data.
- Scheduled multi-agent work survives restarts and exposes failures in the UI.
- Existing workflow YAML without an input schema keeps accepting its legacy
  free-form payload.
- The schema can later add other trigger types without coupling workflow
  execution to channels.
- Only one cron schedule is supported per workflow for now; richer event and
  multi-trigger composition can build on the same persisted execution API.
