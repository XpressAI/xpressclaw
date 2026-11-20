<script lang="ts">
  import type { Task } from "$lib/api";
  import { dateLabel, localInput, taskLabel } from "$lib/taskPlanning";
  import { serverTimestampMs } from "$lib/serverTime";
  let {
    taskList,
    projectName,
    agentName,
    onactions,
    ondrag,
    ondate,
  }: {
    taskList: Task[];
    projectName: (task: Task) => string;
    agentName: (task: Task) => string;
    onactions: (task: Task) => void;
    ondrag: (event: DragEvent, task: Task) => void;
    ondate: (event: DragEvent, date: string) => void;
  } = $props();
  let groupBy = $state("project");
  let start = $state(localInput(Date.now()).slice(0, 10));
  const days = $derived(
    Array.from({ length: 14 }, (_, i) => {
      const d = new Date(`${start}T12:00`);
      d.setDate(d.getDate() + i);
      return {
        date: localInput(d.getTime()).slice(0, 10),
        label: d.toLocaleDateString(undefined, {
          month: "short",
          day: "numeric",
        }),
        weekend: [0, 6].includes(d.getDay()),
      };
    }),
  );
  const groups = $derived.by(() => {
    const groups = new Map<string, { name: string; tasks: Task[] }>();
    for (const task of taskList) {
      const key = groupBy === "project" ? task.project_id ?? "unassigned" : groupBy === "agent" ? task.agent_id ?? "unassigned" : "all";
      const name =
        groupBy === "project"
          ? projectName(task)
          : groupBy === "agent"
            ? `${projectName(task)} · ${agentName(task)}`
            : "All tasks";
      const group = groups.get(key) ?? { name, tasks: [] };
      group.tasks.push(task);
      groups.set(key, group);
    }
    return [...groups]
      .map(([, group]) => group)
      .sort((a, b) => a.name.localeCompare(b.name))
      .map(({ name, tasks }) => ({
        name,
        tasks: tasks.sort(
          (a, b) =>
            (serverTimestampMs(
              a.start_after ?? a.planning?.actual_started_at,
            ) ?? Infinity) -
            (serverTimestampMs(
              b.start_after ?? b.planning?.actual_started_at,
            ) ?? Infinity),
        ),
      }));
  });
  function shift(days: number) {
    const date = new Date(`${start}T12:00`);
    date.setDate(date.getDate() + days);
    start = localInput(date.getTime()).slice(0, 10);
  }
  function index(value: string | null | undefined) {
    const day = localInput(value).slice(0, 10);
    return day ? days.findIndex((d) => d.date === day) : -1;
  }
  function actual(task: Task) {
    const first = serverTimestampMs(task.planning?.actual_started_at);
    if (first === null) return null;
    const left = new Date(`${start}T00:00`).getTime();
    const end = new Date(`${days[13].date}T23:59:59.999`).getTime();
    const last = serverTimestampMs(task.completed_at) ?? first;
    if (last < left || first > end) return null;
    return {
      left: Math.max(0, ((first - left) / (end - left)) * 100),
      width: Math.max(
        0.8,
        ((Math.min(last, end) - Math.max(first, left)) / (end - left)) * 100,
      ),
    };
  }
</script>

<div class="timeline-controls">
  <label
    >Group by<select bind:value={groupBy}
      ><option value="project">Project</option><option value="agent"
        >Agent</option
      ><option value="none">None</option></select
    ></label
  >
  <div class="range">
    <button aria-label="Previous two weeks" onclick={() => shift(-14)}>←</button
    ><button onclick={() => (start = localInput(Date.now()).slice(0, 10))}
      >Today</button
    ><label class="sr-only" for="timeline-start">Timeline start</label><input
      id="timeline-start"
      type="date"
      bind:value={start}
      onblur={() => {
        if (!start) start = localInput(Date.now()).slice(0, 10);
      }}
    /><button aria-label="Next two weeks" onclick={() => shift(14)}>→</button>
  </div>
</div>
<p class="legend">
  <span>◇ Earliest start</span><span
    >━ Actual start → completion (includes waits)</span
  ><span>Completion times are not predicted.</span>
</p>
<div class="chart" data-planning-timeline>
  <div class="chart-inner">
    <div class="date-row">
      <div class="row-label">Task / dependencies</div>
      {#each days as day}<div
          class:today={day.date === localInput(Date.now()).slice(0, 10)}
        >
          {day.label}
        </div>{/each}
    </div>
    {#each groups as group}<div class="group-label">{group.name}</div>
      {#each group.tasks as task (task.id)}
        {@const span = actual(task)}
        <div class="timeline-row" data-timeline-task={task.id}>
          <div class="row-label">
            <a href={`/tasks/${task.id}`}>{task.title}</a><small
              >{projectName(task)} · {agentName(task)} · {taskLabel(
                task,
              )}</small
            >
            {#if task.depends_on?.length}<small
                >Depends on {#each task.depends_on as id, i}{#if i},
                  {/if}<a href={`/tasks/${id}`}
                    >{taskList.find((t) => t.id === id)?.title ??
                      id.slice(0, 8)}</a
                  >{/each}</small
              >{/if}
            <small
              >{task.start_after
                ? `Earliest ${dateLabel(task.start_after)}`
                : "Unscheduled"}</small
            >{#if task.planning?.actual_started_at}<small
                >Actual start {dateLabel(
                  task.planning.actual_started_at,
                )}</small
              >{/if}
            <button
              class="edit"
              aria-label={`Schedule ${task.title}`}
              onclick={() => onactions(task)}>Schedule / actions</button
            >
          </div>
          <div class="date-cells">
            {#each days as day}<div
                class="day-cell"
                class:weekend={day.weekend}
                data-schedule-day={day.date}
                role="group"
                aria-label={`Schedule on ${day.label}`}
                ondragover={(e) => e.preventDefault()}
                ondrop={(e) => ondate(e, day.date)}
              ></div>{/each}
            {#if span}<div
                class="actual-span"
                class:finished={task.status === "completed"}
                style:left={`${span.left}%`}
                style:width={`${span.width}%`}
                title={`Actual start ${dateLabel(task.planning?.actual_started_at)}${task.completed_at ? ` · Completed ${dateLabel(task.completed_at)}` : " · No completion recorded"}`}
              ></div>{/if}
            {#if index(task.start_after) >= 0}<button
                class="marker"
                style:left={`calc(${(index(task.start_after) / 14) * 100}% + 8px)`}
                draggable={!task.planning?.disabled_reason}
                ondragstart={(e) => ondrag(e, task)}
                onclick={() => onactions(task)}
                aria-label={`Earliest start for ${task.title}: ${dateLabel(task.start_after)}`}
                title="Drag to a date or open the schedule picker"
                >◇<span>→</span></button
              >{/if}
            {#if !task.start_after && !span}<button
                class="unscheduled"
                onclick={() => onactions(task)}
                >Unscheduled · choose a date</button
              >{/if}
          </div>
        </div>{/each}{/each}
  </div>
</div>
<div class="agenda" data-planning-agenda>
  {#each groups as group}<h2>{group.name}</h2>
    {#each group.tasks as task (task.id)}<article>
        <div class="agenda-date">
          {task.start_after ? dateLabel(task.start_after) : "Unscheduled"}
        </div>
        <a href={`/tasks/${task.id}`}>{task.title}</a><small
          >{projectName(task)} · {agentName(task)} · {taskLabel(task)}</small
        >{#if task.planning?.actual_started_at}<small
            >Actual start {dateLabel(task.planning.actual_started_at)}</small
          >{/if}{#if task.depends_on?.length}<small
            >Depends on {#each task.depends_on as id, i}{#if i},
              {/if}<a href={`/tasks/${id}`}
                >{taskList.find((t) => t.id === id)?.title ?? id.slice(0, 8)}</a
              >{/each}</small
          >{/if}<button
          aria-label={`Actions for ${task.title}`}
          onclick={() => onactions(task)}>Schedule / actions</button
        >
      </article>{/each}{/each}
</div>

<style>
  .timeline-controls,
  .range {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
  .timeline-controls {
    justify-content: space-between;
    padding: 0 0 12px;
  }
  label {
    font-size: 12px;
  }
  button,
  input,
  select {
    border: 1px solid hsl(var(--border));
    background: hsl(var(--card));
    border-radius: 8px;
    padding: 8px;
    min-height: 40px;
    font-size: 12px;
  }
  select {
    margin-left: 8px;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    margin: 0 0 12px;
  }
  .chart {
    max-height: calc(100dvh - 370px);
    min-height: 240px;
    overflow: auto;
    border: 1px solid hsl(var(--border));
    border-radius: 12px;
    max-width: 100%;
  }
  .chart-inner {
    min-width: 1240px;
  }
  .date-row {
    position: sticky;
    top: 0;
    z-index: 4;
    display: grid;
    grid-template-columns: 260px repeat(14, minmax(60px, 1fr));
    background: hsl(var(--muted));
    align-items: stretch;
    font-size: 11px;
  }
  .date-row > div {
    padding: 12px 8px;
  }
  .today {
    color: hsl(var(--primary));
    font-weight: 700;
  }
  .group-label {
    position: sticky;
    left: 0;
    width: 260px;
    padding: 12px;
    font-size: 12px;
    font-weight: 650;
  }
  .timeline-row {
    display: grid;
    grid-template-columns: 260px 1fr;
    border-top: 1px solid hsl(var(--border));
    min-height: 118px;
  }
  .row-label {
    position: sticky;
    left: 0;
    background: hsl(var(--card));
    padding: 12px;
    z-index: 2;
    border-right: 1px solid hsl(var(--border));
    font-size: 12px;
  }
  .row-label > a {
    font-weight: 600;
  }
  a:hover {
    text-decoration: underline;
  }
  small {
    display: block;
    font-size: 10px;
    color: hsl(var(--muted-foreground));
    margin-top: 4px;
  }
  .edit {
    border: 0;
    padding: 0;
    min-height: 30px;
    font-size: 10px;
    color: hsl(var(--primary));
  }
  .date-cells {
    position: relative;
    display: grid;
    grid-template-columns: repeat(14, 1fr);
  }
  .day-cell {
    border-right: 1px solid hsl(var(--border));
  }
  .day-cell:hover {
    background: hsl(var(--primary) / 0.05);
  }
  .weekend {
    background: hsl(var(--muted) / 0.4);
  }
  .marker {
    position: absolute;
    top: 16px;
    border: 0;
    color: hsl(var(--primary));
    background: hsl(var(--card));
    font-size: 24px;
    line-height: 24px;
    padding: 0 6px;
    min-height: 36px;
    cursor: grab;
  }
  .marker span {
    font-size: 16px;
    opacity: 0.4;
  }
  .actual-span {
    position: absolute;
    top: 66px;
    height: 10px;
    background: #3b82f6;
    border-radius: 4px;
    pointer-events: none;
  }
  .actual-span.finished {
    background: #10b981;
  }
  .unscheduled {
    position: absolute;
    left: 14px;
    top: 34px;
    color: hsl(var(--muted-foreground));
    border-style: dashed;
  }
  .agenda {
    display: none;
  }
  .agenda h2 {
    font-size: 12px;
    font-weight: 650;
    padding: 16px 0 8px;
  }
  .agenda article {
    border: 1px solid hsl(var(--border));
    padding: 14px;
    border-radius: 12px;
    margin-bottom: 10px;
    background: hsl(var(--card));
  }
  .agenda-date {
    color: hsl(var(--primary));
    font-size: 11px;
    margin-bottom: 6px;
  }
  .agenda article > a {
    font-size: 13px;
    font-weight: 600;
  }
  .agenda article > button {
    margin-top: 12px;
    width: 100%;
    min-height: 44px;
  }
  @container task-planner (max-width:640px) {
    .legend {
      display: none;
    }
    .chart,
    .range {
      display: none;
    }
    .agenda {
      display: block;
    }
  }
</style>
