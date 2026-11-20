<script lang="ts">
  import { planningViewport } from "$lib/taskPlanning";
  import { onMount } from "svelte";
  import type { Agent, PlanningAction, Task } from "$lib/api";
  import {
    dateLabel,
    localInput,
    scheduleTimestamp,
    timezone,
    taskLabel,
  } from "$lib/taskPlanning";
  let {
    task,
    agentList,
    scheduleFirst = false,
    activateSchedule = false,
    previous,
    next,
    onchange,
    onclose,
  }: {
    task: Task;
    agentList: Agent[];
    scheduleFirst?: boolean;
    activateSchedule?: boolean;
    previous?: Task;
    next?: Task;
    onchange: (task: Task, change: PlanningAction) => Promise<Task>;
    onclose: () => void;
  } = $props();
  let dialog: HTMLDialogElement;
  let scheduleSection: HTMLElement;
  let busy = $state(false);
  let error = $state("");
  let date = $state("");
  let agentId = $state("");
  let priority = $state(0);
  let move = $state("queue");
  const reason = $derived(
    task.planning?.disabled_reason ??
      (!task.planning
        ? "Planning information is unavailable. Refresh this task."
        : null),
  );
  onMount(() => {
    date = localInput(task.start_after);
    agentId = task.agent_id ?? "";
    priority = task.priority;
    dialog.showModal();
    if (scheduleFirst) {
      move = "scheduled";
      scheduleSection.scrollIntoView({ block: "start" });
    }
  });
  async function apply(action: PlanningAction) {
    busy = true;
    error = "";
    try {
      const updated = await onchange(task, action);
      date = localInput(updated.start_after);
      agentId = updated.agent_id ?? "";
      priority = updated.priority;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
  function schedule(activate = activateSchedule) {
    try {
      if (activate && !date) throw new Error("Choose the earliest start date and time.");
      void apply({ action: "schedule", start_after: scheduleTimestamp(date), activate });
    } catch (e) {
      error = String(e instanceof Error ? e.message : e);
    }
  }
  function moveTask() {
    if (move === "scheduled") {
      if (!date) {
        error = "Choose the earliest start below, then save the schedule.";
        return;
      }
      schedule(true);
    } else void apply({ action: move === "backlog" ? "backlog" : "queue" });
  }
</script>

<dialog
  use:planningViewport
  bind:this={dialog}
  {onclose}
  class="planning-sheet"
  aria-label={`Plan ${task.title}`}
>
  <header>
    <div>
      <small>{taskLabel(task)}</small>
      <h2>{task.title}</h2>
    </div>
    <button
      class="close"
      aria-label="Close task actions"
      onclick={() => dialog.close()}>×</button
    >
  </header>
  <div class="sheet-content">
    <a class="details" href={`/tasks/${task.id}`}>Open task details →</a>
    {#if reason}<p role="status" class="notice">{reason}</p>{/if}
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    <fieldset disabled={busy || !!reason}>
      <section>
        <h3>Move</h3>
        <div class="control-row">
          <select aria-label="Move task to" bind:value={move}
            ><option value="queue">To do (when eligible)</option><option
              value="backlog">Backlog</option
            ><option value="scheduled">Scheduled</option><option disabled
              >Working — controlled by the agent</option
            ><option disabled>Review / Done — use task details</option></select
          ><button onclick={moveTask}>Move task</button>
        </div>
        <p>Backlog keeps work for later. To do preserves its date and dependencies.</p>
        <div class="control-row">
          <button
            disabled={!previous}
            onclick={() =>
              previous && apply({ action: "reorder", before_id: previous.id })}
            >↑ Move up</button
          ><button
            disabled={!next}
            onclick={() =>
              next && apply({ action: "reorder", after_id: next.id })}
            >↓ Move down</button
          >
        </div>
      </section>
      <section>
        <h3>Assign</h3>
        <div class="control-row">
          <select aria-label="Assigned agent" bind:value={agentId}
            >{#each agentList.filter((a) => !task.project_id || a.project_id === task.project_id) as agent}<option
                value={agent.id}>{agent.title || agent.name}</option
              >{/each}</select
          ><button
            disabled={!agentId || agentId === task.agent_id}
            onclick={() => apply({ action: "assign", agent_id: agentId })}
            >Assign</button
          >
        </div>
        <p>Assignment stays within the task’s project.</p>
      </section>
      <section>
        <h3>Priority</h3>
        <div class="control-row">
          <select aria-label="Task priority" bind:value={priority}
            ><option value={-5}>Low</option><option value={0}>Normal</option
            ><option value={5}>High</option><option value={10}>Urgent</option
            >{#if ![-5, 0, 5, 10].includes(task.priority)}<option
                value={task.priority}>{task.priority}</option
              >{/if}</select
          ><button onclick={() => apply({ action: "priority", priority })}
            >Set priority</button
          >
        </div>
      </section>
      <section bind:this={scheduleSection}>
        <h3>Schedule</h3>
        <label
          >Start no earlier than<input
            type="datetime-local"
            bind:value={date}
          /></label
        >
        <p>
          {timezone()}{date ? ` · ${dateLabel(new Date(date).getTime())}` : ""}
        </p>
        <div class="control-row">
          <button class="primary" onclick={() => schedule()}>{activateSchedule ? "Schedule and move to To do" : "Save schedule"}</button
          ><button
            disabled={!task.start_after}
            onclick={() => apply({ action: "schedule", start_after: null })}
            >Clear schedule</button
          >
        </div>
        <p>
          To do becomes eligible after this date. Backlog stays parked until
          moved to To do or Scheduled. Completion time is not predicted.
        </p>
      </section>
    </fieldset>
  </div>
  <footer><button onclick={() => dialog.close()}>Done</button></footer>
</dialog>

<style>
  .planning-sheet {
    width: min(520px, calc(100vw - 24px));
    max-height: calc(100dvh - 40px);
    margin: auto;
    padding: 0;
    border: 1px solid hsl(var(--border));
    border-radius: 18px;
    background: hsl(var(--background));
    color: hsl(var(--foreground));
    box-shadow: 0 22px 80px #0003;
  }
  .planning-sheet[open] {
    display: flex;
    flex-direction: column;
  }
  .planning-sheet::backdrop {
    background: #0006;
  }
  header,
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 16px 20px;
    flex-shrink: 0;
  }
  header {
    border-bottom: 1px solid hsl(var(--border));
  }
  h2 {
    font-size: 16px;
    font-weight: 650;
    overflow-wrap: anywhere;
  }
  small {
    color: hsl(var(--muted-foreground));
  }
  .sheet-content {
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 4px 20px 16px;
  }
  section {
    padding: 16px 0;
    border-bottom: 1px solid hsl(var(--border));
  }
  h3 {
    font-size: 13px;
    font-weight: 650;
    margin-bottom: 9px;
  }
  .control-row {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  select {
    flex: 1;
    min-width: 0;
  }
  label {
    font-size: 12px;
    display: block;
  }
  input {
    display: block;
    width: 100%;
    margin-top: 8px;
  }
  button,
  select,
  input {
    min-height: 44px;
    border: 1px solid hsl(var(--border));
    border-radius: 8px;
    padding: 8px 12px;
    background: hsl(var(--card));
    font-size: 12px;
  }
  button:hover {
    background: hsl(var(--accent));
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  fieldset { min-width: 0; margin: 0; padding: 0; }
  fieldset:disabled {
    opacity: 0.7;
  }
  p {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    margin-top: 7px;
    line-height: 1.5;
  }
  .details {
    display: inline-block;
    padding: 12px 0;
    color: hsl(var(--primary));
    font-size: 12px;
  }
  .error {
    color: hsl(var(--destructive));
  }
  .notice {
    padding: 12px;
    border: 1px solid hsl(var(--border));
    border-radius: 8px;
  }
  .primary {
    background: hsl(var(--primary));
    color: hsl(var(--primary-foreground));
  }
  footer {
    border-top: 1px solid hsl(var(--border));
    padding-bottom: max(12px, env(safe-area-inset-bottom));
  }
  footer button {
    width: 100%;
  }
  .close {
    font-size: 24px;
    min-width: 44px;
    border: 0;
  }
  @media (max-width: 640px) {
    input, select { font-size: 16px; }
    .planning-sheet {
      width: 100%;
      max-width: 100vw;
      top: calc(var(--planning-viewport-top, 0px) + var(--planning-viewport-height, 100dvh));
      bottom: auto;
      transform: translateY(-100%);
      max-height: calc(var(--planning-viewport-height, 100dvh) - 12px);
      margin: 0;
      border-radius: 18px 18px 0 0;
    }
  }
</style>
