<script lang="ts">
  import type { Task } from "$lib/api";
  import { dateLabel, taskLabel, taskLane, lanes } from "$lib/taskPlanning";
  let {
    task,
    project,
    agent,
    onactions,
    ondrag,
    ondrop,
    onreorder,
    selected = false,
    showDescription = false,
  }: {
    task: Task;
    project: string;
    agent: string;
    selected?: boolean;
    showDescription?: boolean;
    onactions: (task: Task) => void;
    ondrag: (event: DragEvent, task: Task) => void;
    ondrop?: (event: DragEvent, task: Task) => void;
    onreorder?: (task: Task, direction: number) => void;
  } = $props();
  const editable = $derived(!!task.planning && !task.planning.disabled_reason);
</script>

<!-- Dragging duplicates the Actions menu and Alt+Arrow keyboard controls. -->
<article
  data-planning-card={task.id}
  class:selected
  draggable={editable}
  aria-label={`${task.title}, ${taskLabel(task)}. Use Actions for planning controls.`}
  ondragstart={(event) => ondrag(event, task)}
  ondragover={(event) => {
    if (editable) event.preventDefault();
  }}
  ondrop={(event) => ondrop?.(event, task)}
>
  <div class="identity">
    <span title={project}>{project}</span><span title={agent}>{agent}</span>
  </div>
  <div class="title">
    <a href={`/tasks/${task.id}`}>{task.title}</a><button
      onkeydown={(event) => {
        if (
          event.altKey &&
          ["ArrowUp", "ArrowDown"].includes(event.key) &&
          editable
        ) {
          event.preventDefault();
          onreorder?.(task, event.key === "ArrowUp" ? -1 : 1);
        }
      }}
      type="button"
      aria-label={`Actions for ${task.title}`}
      onclick={() => onactions(task)}>•••</button
    >
  </div>
  {#if showDescription && task.description}<p class="description">{task.description}</p>{/if}
  <div class="meta">
    <span
      class="status"
      style:color={lanes.find((l) => l.id === taskLane(task))?.color}
      >{taskLabel(task)}</span
    ><span
      >{task.priority >= 10
        ? "Urgent"
        : task.priority >= 5
          ? "High"
          : task.priority < 0
            ? "Low"
            : "Normal"}</span
    >{#if task.parent_task_id}<span>Subtask</span>{/if}
  </div>
  {#if task.start_after}<p class="date">
      Earliest {dateLabel(task.start_after)}
    </p>{/if}
  {#if task.depends_on?.length}<p class="dependencies">
      ↳ {task.depends_on.length}
      {task.depends_on.length === 1 ? "dependency" : "dependencies"}
    </p>{/if}
</article>

<style>
  .description { display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; color: hsl(var(--muted-foreground)); font-size: 12px; margin: 6px 0; white-space: pre-wrap; }
  article {
    border: 1px solid hsl(var(--border));
    border-radius: 12px;
    background: hsl(var(--card));
    padding: 12px;
    box-shadow: 0 1px 2px #00000006;
    min-width: 0;
  }
  article[draggable="true"] {
    cursor: grab;
  }
  article:focus-visible,
  article.selected {
    outline: 2px solid hsl(var(--primary));
    outline-offset: 2px;
  }
  .identity {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 10px;
    color: hsl(var(--muted-foreground));
  }
  .identity span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 55%;
  }
  .title {
    display: flex;
    align-items: start;
    gap: 4px;
    margin-top: 6px;
  }
  .title a {
    flex: 1;
    font-size: 13px;
    font-weight: 550;
    overflow-wrap: anywhere;
    line-height: 1.45;
  }
  .title a:hover {
    text-decoration: underline;
  }
  button {
    min-width: 36px;
    min-height: 36px;
    margin: -5px -6px 0 0;
    border-radius: 7px;
    font-weight: 700;
  }
  button:hover {
    background: hsl(var(--accent));
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 10px;
    margin-top: 9px;
    color: hsl(var(--muted-foreground));
  }
  .status {
    font-weight: 650;
  }
  .date,
  .dependencies {
    margin-top: 6px;
    font-size: 10px;
    color: hsl(var(--muted-foreground));
    line-height: 1.5;
  }
  @media (pointer: coarse) {
    button {
      min-width: 44px;
      min-height: 44px;
    }
    article {
      padding: 14px;
    }
  }
</style>
