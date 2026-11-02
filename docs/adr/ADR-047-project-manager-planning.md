# ADR 047: Project manager planning through MCP

**Status:** Proposed for review with this implementation

## Context

Native Agents can delegate a task but cannot discover their teammates, create
approval-held or future-dated work, or schedule workflows. The task planner
and workflow engine already implement most of the required lifecycle behavior.
Display titles can differ from Agent IDs, so guessing names fails assignment.

## Decision

Expose a narrow Project planning surface on the authenticated runner callback
router. The calling Agent determines the Project; request parameters cannot
select another Project. Discovery returns IDs and display context, not Agent
credentials or runner configuration.

Reuse TaskBoard and TaskPlanner. Task batches contain at most 100 entries and
commit creation, references, dependency edges and queue entries atomically.
Validate completion cycles including blocking parent-child relationships.
Planning mutations retain revision checks and cannot interrupt active work.
Explicit null parents support independent long-term work; omitted parents
retain existing delegation semantics.

Expose shared workflow templates for discovery and copy them when scheduling.
Manager-created definitions carry Project ownership. Only workflows owned by
the calling Project can be edited through MCP. Workflow runs inherit that
ownership and revalidate Agent membership when they start and execute.
Existing global workflows remain shared, and Project-owned defaults apply only
to tasks in their Project.

Extend the existing workflow schedule with an exclusive one-time `run_at`
alternative to cron. Claim the occurrence and insert its instance in one
transaction, guarded by the workflow revision and prior trigger timestamp.
One-time schedules disable after firing. Keep the existing timer, workflow
engine, persisted instance recovery, and GUI rather than introducing a second
workflow scheduler.

Schema version 55 adds nullable Project ownership and a revision to workflows.
Existing definitions retain their current global ownership and cron behavior.
New Project-owned definitions are removed by cascading Project deletion.

Expose the resulting work through the existing Tasks and Automations views,
including editable one-time workflow dates. Backlog tasks and disabled
workflows represent plans awaiting approval.

## Validation

Exercise multi-Agent batches, reference resolution, rollback, parent completion
cycles, future starts, Backlog release, stale revisions, and Project isolation.
Test one-time and recurring schedules, pause/resume, concurrent occurrence
claims, ownership changes, MCP transport, callback authorization, and visual
editor round-tripping in Chromium and WebKit.
