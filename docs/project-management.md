# Project management with an Agent

An Agent can plan durable work for the other Agents in its Project, inspect
their results, and schedule workflows for later. These use the same Tasks and
Automations shown in the GUI.

For example:

> Discover the Agents in this Project and prepare a six-week delivery plan.
> Create the work in Backlog, assign each task to the appropriate Agent, and
> record dependencies and earliest start dates. Keep this planning task
> independent of the future work. Show me the plan before starting it.

After reviewing the plan:

> Release the approved first milestone. Keep the other milestones in Backlog.
> Schedule the project checkpoint workflow every Monday at 09:00, with a
> separate one-time readiness review on January 15 at 09:00 Japan time.

## Discover and inspect

Use `list_project_agents` to obtain the exact IDs, display titles, harnesses,
and status of the current Project's Agents. Display titles are not identifiers.
No manual copying of Agent URLs is needed.

`list_project_tasks` supports search, Agent and state filters, and pagination.
`get_project_task` returns a task's planning state, revision, dependencies, and
latest 20 messages so the manager can inspect worker results.

`list_project_workflows` lists shared templates and the Project's workflows.
Use `include_templates: false` to focus on the Project's own automations.
`get_project_workflow` returns its YAML definition and revision.

## Create a durable task plan

`create_tasks` creates up to 100 tasks in one transaction. Each task can have:

- `agent_id`: an ID obtained from the Project directory.
- `backlog: true`: hold the task for approval.
- `start_after`: its earliest start, as an RFC 3339 timestamp with a timezone.
- `ref`: a unique reference within this batch.
- `depends_on`: batch references or existing task IDs in the same Project.
- `priority` and `new_session`: execution preferences.

Every dependency must complete before the dependent task can start. Backlog
holds remain in effect after a start date passes. A future task does not reserve
an Agent's execution lane.

The complete batch is validated before it becomes visible. An invalid Agent,
date, reference, or dependency cycle rolls back both tasks and queue entries.
For larger plans, create multiple batches and use returned IDs for dependencies
between batches. Responses contain both the tasks and a `refs` map.

Omitting `parent_task_id` creates children of the current task. Required
children block that parent's completion. Set `parent_task_id: null` for
independent project work that will continue after the planning conversation.
The singular `create_task` supports the same dates, Backlog, and dependencies.

Use `update_task_planning` with the latest `expected_revision` to change
future work. Its actions are `schedule`, `backlog`, `queue`, `assign`, and
`priority`. `queue` releases a Backlog hold while preserving its date and
dependencies. `schedule` can also release it with `activate: true`.
An active turn retains its existing task lifecycle.

## Schedule workflows

`schedule_workflows` accepts up to 100 entries, each with a template
`workflow_id`, input values, an optional name, and exactly one of:

- `run_at`: a one-time RFC 3339 date and time, including timezone.
- `cron`: a recurring expression in the XpressClaw server's local timezone.

Each entry creates an independent, Project-owned copy. Scheduling the same
template for different milestones or input values leaves the source template
unchanged. Copies have distinct IDs; colliding names receive a numeric suffix.
All Agent bindings stay within the Project.

Use `enabled: false` to prepare workflow schedules for approval. After
approval, `update_project_workflow` can enable or pause a copy, or replace its
YAML to change its steps, inputs, or schedule. Supply its current revision.
Shared templates are read-only through these manager tools.

`create_workflows` can also create up to 100 Project-owned definitions from
YAML in one transaction. Definitions use the existing
[workflow format](workflows.md). For a one-time YAML schedule:

```yaml
schedule:
  run_at: "2027-01-15T09:00:00+09:00"
  inputs:
    goal: Review milestone readiness
    worker: "<Agent ID returned by list_project_agents>"
```

The schedule and its workflow instance are committed together. A one-time
schedule disables after starting its run; subsequent checks do not replay it.
Changing its run time prepares a new occurrence. Pausing a schedule affects
future starts, while already-started instances continue through their own
lifecycle.

Project workflows and one-time dates are visible in **Automations**. The
workflow editor's **Inputs & trigger** section offers **Run once** and
**Cron schedule** and preserves scheduled input values when switching modes.

## Current-turn plans

ACP plan items remain the Agent's checklist for its current response. Use
durable tasks or workflow schedules for a multi-week project, and finish the
current checklist when the planning work itself is done.
