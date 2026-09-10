<script lang="ts">
  import { planningViewport } from "$lib/taskPlanning";
  import { onMount, onDestroy, tick, untrack } from "svelte";
  import {
    agents,
    projects,
    tasks,
    type Agent,
    type Project,
    type Task,
    type PlanningAction,
  } from "$lib/api";
  import {
    lanes,
    taskLane,
    timezone,
    localInput,
    scheduleTimestamp,
  } from "$lib/taskPlanning";
  import TaskCard from "$lib/components/planning/TaskCard.svelte";
  import TaskActions from "$lib/components/planning/TaskActions.svelte";
  import TaskTimeline from "$lib/components/planning/TaskTimeline.svelte";
  import CreateTask from "$lib/components/planning/CreateTask.svelte";
  let { route = "/tasks" }: { route?: string } = $props();
  let mounted = $state(false);
  $effect(() => {
    const next = route;
    if (mounted)
      untrack(() => {
        const url = new URL(next, location.origin);
        const mode = url.searchParams.get("view");
        if (mode && ["board", "timeline", "list"].includes(mode))
          view = mode as View;
        projectId = url.searchParams.get("project") ?? "";
        reset();
      });
  });
  function closeActions() {
    const id = selected?.id;
    selected = null;
    void tick().then(() => {
      const button = id
        ? root.querySelector<HTMLButtonElement>(
            `[data-planning-card="${CSS.escape(id)}"] button`,
          )
        : null;
      (button ?? root).focus();
    });
  }
  type View = "board" | "timeline" | "list";
  let root: HTMLDivElement;
  let scroller: HTMLDivElement;
  let filters: HTMLDialogElement;
  let view = $state<View>("list");
  let projectId = $state("");
  let agentId = $state("");
  let status = $state("active");
  let searchText = $state("");
  let search = $state("");
  let composing = false;
  let timer: ReturnType<typeof setTimeout>;
  let taskList = $state<Task[]>([]);
  let agentList = $state<Agent[]>([]);
  let projectList = $state<Project[]>([]);
  let counts = $state<Record<string, number>>({});
  let total = $state(0);
  let loading = $state(true);
  let error = $state("");
  let notice = $state("");
  let page = $state(0);
  let selected = $state<Task | null>(null);
  let scheduleFirst = $state(false);
  let activateSchedule = $state(false);
  let actionRequest = 0;
  let showCreate = $state(false);
  let createBacklog = $state(false);
  let narrow = $state(false);
  let dragging = $state<Task | null>(null);
  let saving = $state(false);
  let generation = 0;
  const pageSize = $derived(view === "list" ? 20 : 100);
  const pages = $derived(Math.max(1, Math.ceil(total / pageSize)));
  const columns = $derived(
    lanes.filter(
      (l) =>
        status === "all" ||
        (status === "active" && l.id !== "done") ||
        (status === "attention" &&
          ["input", "blocked", "review"].includes(l.id)) ||
        status === l.id,
    ),
  );
  const selectedNeighbors = $derived.by(() => {
    if (!selected) return {};
    const lane = taskList.filter(
      (t) =>
        taskLane(t) === taskLane(selected!) && !t.planning?.disabled_reason,
    );
    const index = lane.findIndex((t) => t.id === selected!.id);
    return { previous: lane[index - 1], next: lane[index + 1] };
  });
  onMount(() => {
    const url = new URL(route, location.origin);
    const saved =
      url.searchParams.get("view") ??
      localStorage.getItem("xpressclaw.tasks.view");
    if (["board", "timeline", "list"].includes(saved ?? ""))
      view = saved as View;
    projectId = url.searchParams.get("project") ?? "";
    void Promise.all([agents.list(), projects.list()])
      .then(([a, p]) => {
        agentList = a;
        projectList = p;
      })
      .catch((e) => (error = String(e)));
    const resize = new ResizeObserver((entries) => {
      root.style.setProperty(
        "--planner-height",
        `${entries[0].contentRect.height}px`,
      );
      const next = entries[0].contentRect.width <= 640;
      if (next !== narrow) {
        narrow = next;
        if (
          next &&
          view === "board" &&
          ["all", "active", "attention"].includes(status)
        ) {
          status = "queue";
          page = 0;
          void load();
        }
      }
    });
    resize.observe(root);
    mounted = true;
    const poll = setInterval(() => {
      if (!document.hidden && !dragging && !saving) void load(true);
    }, 5000);
    return () => {
      resize.disconnect();
      clearInterval(poll);
    };
  });
  onDestroy(() => clearTimeout(timer));
  async function load(quiet = false) {
    const request = ++generation;
    if (!quiet) loading = true;
    try {
      const result = await tasks.planning({
        project_id: projectId,
        agent_id: agentId,
        status,
        search,
        limit: pageSize,
        offset: page * pageSize,
      });
      if (request !== generation) return;
      taskList = result.tasks;
      counts = result.counts;
      total = result.total;
      if (page > 0 && page * pageSize >= total) {
        page = 0;
        void load();
      }
    } catch (e) {
      if (request === generation)
        error = e instanceof Error ? e.message : String(e);
    } finally {
      if (request === generation) loading = false;
    }
  }
  function projectName(task: Task) {
    return (
      projectList.find((p) => p.id === task.project_id)?.name ?? "No project"
    );
  }
  function agentName(task: Task) {
    const agent = agentList.find((a) => a.id === task.agent_id);
    return agent?.title || agent?.name || task.agent_id || "Unassigned";
  }
  function reset() {
    page = 0;
    error = "";
    void load();
  }
  function setView(next: View) {
    view = next;
    localStorage.setItem("xpressclaw.tasks.view", view);
    if (
      narrow &&
      next === "board" &&
      ["all", "active", "attention"].includes(status)
    )
      status = "queue";
    reset();
  }
  function filterCount(id: string) {
    return Object.entries(counts)
      .filter(
        ([lane]) =>
          id === "all" ||
          (id === "active" && lane !== "done") ||
          (id === "attention" &&
            ["input", "blocked", "review"].includes(lane)) ||
          id === lane,
      )
      .reduce((sum, [, n]) => sum + n, 0);
  }
  function setStatus(next: string) {
    status = next;
    reset();
  }
  function searchSoon() {
    if (composing) return;
    clearTimeout(timer);
    if (!composing)
      timer = setTimeout(() => {
        search = searchText.trim();
        reset();
      }, 250);
  }
  async function openActions(task: Task, schedule = false, activate = false) {
    const request = ++actionRequest;
    try {
      const latest = await tasks.planningTask(task.id);
      if (request === actionRequest) {
        scheduleFirst = schedule;
        activateSchedule = activate;
        selected = latest;
      }
    } catch (e) {
      if (request === actionRequest) error = String(e);
    }
  }
  async function mutate(task: Task, action: PlanningAction): Promise<Task> {
    saving = true;
    error = "";
    try {
      const updated = await tasks.plan(task, action);
      taskList = taskList.map((t) => (t.id === updated.id ? updated : t));
      if (selected?.id === updated.id) selected = updated;
      notice = "Planning saved. Queue and dependency rules still apply.";
      await load(true);
      return updated;
    } catch (e) {
      await load(true);
      if (selected?.id === task.id) {
        try {
          selected = await tasks.planningTask(task.id);
        } catch {}
      }
      throw e;
    } finally {
      saving = false;
    }
  }
  async function tryMove(task: Task, action: PlanningAction) {
    try {
      await mutate(task, action);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }
  function startDrag(event: DragEvent, task: Task) {
    if (!task.planning || task.planning.disabled_reason) {
      event.preventDefault();
      return;
    }
    dragging = task;
    event.dataTransfer?.setData("text/x-xpressclaw-task", task.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }
  function dragged(event: DragEvent) {
    event.preventDefault();
    event.stopPropagation();
    const task = dragging;
    if (
      !task ||
      event.dataTransfer?.getData("text/x-xpressclaw-task") !== task.id
    )
      return null;
    dragging = null;
    return task;
  }
  function dropCard(event: DragEvent, target: Task) {
    const task = dragged(event);
    if (!task || task.id === target.id) return;
    if (taskLane(task) !== taskLane(target)) moveToLane(task, taskLane(target));
    else void tryMove(task, { action: "reorder", before_id: target.id });
  }
  function dropLane(event: DragEvent, lane: string) {
    const task = dragged(event);
    if (task) moveToLane(task, lane);
  }
  function moveToLane(task: Task, lane: string) {
    if (lane === "queue") void tryMove(task, { action: "queue" });
    else if (lane === "backlog") void tryMove(task, { action: "backlog" });
    else if (lane === "scheduled") void openActions(task, true, true);
    else
      error =
        "This column follows agent activity. Use task details to respond, retry, or review.";
  }
  function reorder(task: Task, direction: number) {
    const lane = taskList.filter(
      (t) => taskLane(t) === taskLane(task) && !t.planning?.disabled_reason,
    );
    const next = lane[lane.findIndex((t) => t.id === task.id) + direction];
    if (next)
      void tryMove(
        task,
        direction < 0
          ? { action: "reorder", before_id: next.id }
          : { action: "reorder", after_id: next.id },
      );
  }
  function dropDate(event: DragEvent, date: string) {
    const task = dragged(event);
    if (!task) return;
    const clock = localInput(task.start_after).slice(11) || "09:00";
    try {
      void tryMove(task, {
        action: "schedule",
        start_after: scheduleTimestamp(`${date}T${clock}`),
      });
    } catch (e) {
      error = String(e);
    }
  }
  function go(next: number) {
    page = next;
    void load();
    scroller.scrollTo({ top: 0 });
  }
</script>

<svelte:window ondragend={() => (dragging = null)} />
<div
  bind:this={root}
  class="task-planner"
  tabindex="-1"
  aria-busy={saving || loading}
>
  <header class="planner-header">
    <div>
      <h1>Tasks</h1>
      <p>
        {total}
        {search ? "matching " : ""}{total === 1 ? "task" : "tasks"} · {projectId
          ? projectList.find((p) => p.id === projectId)?.name
          : "All projects"}
      </p>
    </div>
    <button
      class="new-desktop primary"
      onclick={() => (showCreate = true)}
      disabled={!agentList.length}>New Task</button
    >
  </header>
  <div class="top-controls">
    <nav aria-label="Task views">
      {#each [["board", "Board"], ["timeline", "Timeline / Gantt"], ["list", "List"]] as [id, label]}<button
          aria-pressed={view === id}
          onclick={() => setView(id as View)}>{label}</button
        >{/each}
    </nav>
    <button onclick={() => filters.showModal()}
      >Filters{projectId || agentId ? " · active" : ""}</button
    >
  </div>
  <div class="search">
    <input
      type="search"
      aria-label="Search tasks"
      placeholder="Search tasks and conversations…"
      value={searchText}
      maxlength="200"
      oninput={(e) => {
        searchText = e.currentTarget.value;
        if (!(e instanceof InputEvent && e.isComposing)) searchSoon();
      }}
      oncompositionstart={() => {
        composing = true;
        clearTimeout(timer);
      }}
      oncompositionend={(event) => {
        const input = event.currentTarget;
        clearTimeout(timer);
        timer = setTimeout(() => {
          composing = false;
          searchText = input.value;
          searchSoon();
        }, 0);
      }}
      onkeydown={(e) => {
        if (
          e.key === "Enter" &&
          !e.isComposing &&
          !composing &&
          e.keyCode !== 229
        ) {
          clearTimeout(timer);
          search = searchText.trim();
          reset();
        }
      }}
    />{#if searchText}<button
        aria-label="Clear task search"
        onclick={() => {
          clearTimeout(timer);
          searchText = "";
          search = "";
          reset();
        }}>×</button
      >{/if}
  </div>
  <div class="status-controls">
    {#each [["attention", "Needs you"], ["active", "Active"], ["all", "All"], ["done", "Done"]] as [id, label]}<button
        aria-pressed={status === id}
        onclick={() => setStatus(id)}>{label} {filterCount(id)}</button
      >{/each}<label class="lane-filter"
      >Status<select
        aria-label={narrow && view === "board" ? "Board lane" : "Task status"}
        value={status}
        onchange={(e) => setStatus(e.currentTarget.value)}
        ><option value="active">All active lanes</option><option value="all"
          >All lanes</option
        ><option value="attention">Needs attention</option
        >{#each lanes as lane}<option value={lane.id}
            >{lane.label} ({counts[lane.id] ?? 0})</option
          >{/each}</select
      ></label
    >
  </div>
  {#if projectId || agentId}<div class="filter-summary">
      {projectList.find((p) => p.id === projectId)?.name ?? "All projects"} · {(agentList.find(
        (a) => a.id === agentId,
      )?.title ??
        agentId) ||
        "All agents"}<button
        onclick={() => {
          projectId = "";
          agentId = "";
          reset();
        }}>Clear filters</button
      >
    </div>{/if}
  {#if error}<div class="error" role="alert">
      {error}<button
        onclick={() => {
          error = "";
          void load();
        }}>Refresh</button
      >
    </div>{/if}
  <span class="sr-only" role="status">{notice}</span>
  <div
    bind:this={scroller}
    class="planner-scroll"
    data-tasks-scroll
    aria-busy={loading}
  >
    {#if loading && !taskList.length}<p class="empty">
        Loading tasks…
      </p>{:else if !taskList.length}<p class="empty">
        {search ? `No tasks match “${search}”.` : "No tasks in this view."}
      </p>{/if}
    {#if view === "board"}<p class="hint">
        Drag to reorder, move between Backlog and To do, or schedule. Use Actions or Alt + ↑ / ↓
        from a card for keyboard controls.
      </p>
      <div class="board" data-task-board>
        {#each columns as lane}<section
            class="lane"
            class:unavailable={!!dragging &&
              !["queue", "backlog", "scheduled"].includes(lane.id) &&
              taskLane(dragging) !== lane.id}
            data-board-lane={lane.id}
            aria-label={lane.label}
            ondragover={(e) => e.preventDefault()}
            ondrop={(e) => dropLane(e, lane.id)}
          >
            <h2>
              <span style:background={lane.color}></span>{lane.label}<small
                >{counts[lane.id] ?? 0}</small
              >
            </h2>
            {#if dragging && !["queue", "backlog", "scheduled"].includes(lane.id) && taskLane(dragging) !== lane.id}<p
                class="hint"
              >
                Updated by the task lifecycle
              </p>{/if}
            {#if lane.id === "backlog"}<button class="backlog-add" onclick={() => { createBacklog = true; showCreate = true; }}>+ Add to Backlog</button>{/if}
            <div class="cards">
              {#each taskList.filter((t) => taskLane(t) === lane.id) as task (task.id)}<TaskCard
                  {task}
                  project={projectName(task)}
                  agent={agentName(task)}
                  onactions={openActions}
                  ondrag={startDrag}
                  ondrop={dropCard}
                  onreorder={reorder}
                />{/each}
            </div>
            {#if !taskList.some((t) => taskLane(t) === lane.id)}<p
                class="lane-empty"
              >
                No tasks on this page
              </p>{/if}
          </section>{/each}
      </div>{:else if view === "timeline"}<p class="hint">
        Dates shown in {timezone()}. Drag a start marker to change its date; use
        Schedule / actions for precise times.
      </p>
      <TaskTimeline
        {taskList}
        {projectName}
        {agentName}
        onactions={(task) => openActions(task, true)}
        ondrag={startDrag}
        ondate={dropDate}
      />{:else}<div class="task-list" data-task-list>
        {#each taskList as task (task.id)}<div
            data-task-row
            data-task-activity-status={task.activity_status ?? task.status}
          >
            <TaskCard
              {task}
              showDescription
              project={projectName(task)}
              agent={agentName(task)}
              onactions={openActions}
              ondrag={startDrag}
              ondrop={dropCard}
              onreorder={reorder}
            />
          </div>{/each}
      </div>{/if}
    {#if pages > 1}<nav class="pagination" aria-label="Task pages">
        <button disabled={page === 0 || loading} onclick={() => go(page - 1)}
          >Previous</button
        ><span
          >Page {page + 1} of {pages} · {page * pageSize + 1}–{Math.min(
            (page + 1) * pageSize,
            total,
          )} of {total}</span
        ><button
          disabled={page + 1 >= pages || loading}
          onclick={() => go(page + 1)}>Next</button
        >
      </nav>{/if}
  </div>
  <footer role="group" class="mobile-controls" aria-label="Task controls">
    {#each [["board", "Board"], ["timeline", "Timeline"], ["list", "List"]] as [id, label]}<button
        aria-pressed={view === id}
        onclick={() => setView(id as View)}>{label}</button
      >{/each}<button onclick={() => filters.showModal()}>Filters</button
    ><button
      class="primary"
      onclick={() => (showCreate = true)}
      disabled={!agentList.length}>+ Task</button
    >
  </footer>
</div>
<dialog bind:this={filters} class="filters-sheet" aria-label="Task filters">
  <header>
    <h2>Filter tasks</h2>
    <button aria-label="Close filters" onclick={() => filters.close()}>×</button
    >
  </header>
  <div>
    <label
      >Project<select
        aria-label="Filter project"
        bind:value={projectId}
        onchange={() => (agentId = "")}
        ><option value="">All projects</option
        >{#each projectList as project}<option value={project.id}
            >{project.name}</option
          >{/each}</select
      ></label
    ><label
      >Agent<select aria-label="Filter agent" bind:value={agentId}
        ><option value="">All agents</option
        >{#each agentList.filter((a) => !projectId || a.project_id === projectId) as agent}<option
            value={agent.id}>{agent.title || agent.name}</option
          >{/each}</select
      ></label
    ><label
      >Status<select aria-label="Filter status" bind:value={status}
        ><option value="active">Active</option><option value="all">All</option
        ><option value="attention">Needs attention</option
        >{#each lanes as lane}<option value={lane.id}>{lane.label}</option
          >{/each}</select
      ></label
    >
  </div>
  <footer>
    <button
      onclick={() => {
        projectId = "";
        agentId = "";
        status = "active";
        reset();
        filters.close();
      }}>Reset</button
    ><button
      class="primary"
      onclick={() => {
        reset();
        filters.close();
      }}>Apply filters</button
    >
  </footer>
</dialog>
{#if selected}<TaskActions
    {scheduleFirst}
    {activateSchedule}
    task={selected}
    {agentList}
    {...selectedNeighbors}
    onchange={mutate}
    onclose={closeActions}
  />{/if}
{#if showCreate}<CreateTask
    {agentList}
    {projectList}
    initialAgent={agentId ||
      agentList.find((a) => a.project_id === projectId)?.id ||
      ""}
    initialBacklog={createBacklog}
    onclose={() => { showCreate = false; createBacklog = false; }}
    oncreated={() => {
      notice = "Task created.";
      reset();
    }}
  />{/if}

<style>
  .task-planner {
    container: task-planner / inline-size;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    min-width: 0;
    background: hsl(var(--background));
  }
  .planner-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 22px 24px 14px;
    flex-shrink: 0;
  }
  h1 {
    font-size: 24px;
    font-weight: 700;
  }
  .planner-header p {
    font-size: 12px;
    color: hsl(var(--muted-foreground));
    margin-top: 4px;
  }
  button,
  select,
  input {
    min-height: 40px;
    border: 1px solid hsl(var(--border));
    background: hsl(var(--card));
    border-radius: 8px;
    padding: 8px 12px;
    font-size: 12px;
  }
  button:hover {
    background: hsl(var(--accent));
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  button[aria-pressed="true"] {
    background: hsl(var(--primary) / 0.1);
    color: hsl(var(--primary));
    border-color: hsl(var(--primary) / 0.3);
    font-weight: 600;
  }
  .primary {
    background: hsl(var(--primary));
    color: hsl(var(--primary-foreground));
  }
  .primary:hover {
    background: hsl(var(--primary) / 0.9);
  }
  .top-controls {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin: 0 24px 12px;
    gap: 12px;
  }
  .top-controls nav {
    display: flex;
    gap: 6px;
  }
  .search {
    position: relative;
    margin: 0 24px 12px;
  }
  .search input {
    width: 100%;
    padding-right: 44px;
  }
  .search button {
    position: absolute;
    right: 0;
    top: 0;
    border: 0;
    background: transparent;
    font-size: 20px;
  }
  .status-controls {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 24px 12px;
    flex-wrap: wrap;
  }
  .status-controls > button {
    min-height: 32px;
    padding: 6px 10px;
    font-size: 11px;
    border-radius: 20px;
  }
  .lane-filter {
    margin-left: auto;
    font-size: 11px;
    color: hsl(var(--muted-foreground));
  }
  .lane-filter select {
    margin-left: 8px;
    min-height: 34px;
  }
  .planner-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    overscroll-behavior: contain;
    padding: 0 24px 20px;
  }
  .hint {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    line-height: 1.5;
    margin-bottom: 12px;
  }
  .board {
    height: max(250px, calc(var(--planner-height, 100dvh) - 320px));
    overflow-y: hidden;
    display: flex;
    align-items: stretch;
    gap: 12px;
    overflow-x: auto;
    padding: 2px 2px 16px;
    min-height: 340px;
  }
  .lane {
    display: flex;
    flex-direction: column;
    flex: 1 0 250px;
    min-width: 250px;
    max-width: 340px;
    border-radius: 12px;
    background: hsl(var(--muted) / 0.5);
    border: 1px solid hsl(var(--border) / 0.7);
    padding: 12px;
  }
  .lane h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    font-weight: 650;
    margin: 0 0 14px;
  }
  .lane h2 > span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .lane h2 > small {
    margin-left: auto;
    color: hsl(var(--muted-foreground));
  }
  .lane.unavailable {
    opacity: 0.6;
  }
  .cards {
    overflow-y: auto;
    min-height: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .lane-empty {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    padding: 18px 4px;
  }
  .task-list {
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .task-list :global(article) {
    padding: 14px 16px;
  }
  .task-list :global(.identity) {
    justify-content: flex-start;
  }
  .task-list :global(.title a) {
    font-size: 14px;
  }
  .empty {
    padding: 48px 16px;
    text-align: center;
    color: hsl(var(--muted-foreground));
    font-size: 13px;
  }
  .pagination {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 20px;
  }
  .pagination span {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
  }
  .filter-summary {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    padding: 0 24px 10px;
  }
  .filter-summary button {
    border: 0;
    min-height: 28px;
    padding: 4px 8px;
    background: transparent;
    color: hsl(var(--primary));
  }
  .error {
    margin: 0 24px 12px;
    padding: 12px;
    border: 1px solid hsl(var(--destructive) / 0.4);
    border-radius: 8px;
    color: hsl(var(--destructive));
    font-size: 12px;
  }
  .error button {
    margin-left: 10px;
  }
  .mobile-controls {
    display: none;
  }
  .filters-sheet {
    width: min(440px, calc(100vw - 24px));
    margin: auto;
    padding: 0;
    border: 1px solid hsl(var(--border));
    border-radius: 18px;
    background: hsl(var(--background));
    color: hsl(var(--foreground));
    max-height: calc(100dvh - 24px);
    overflow: auto;
  }
  .filters-sheet::backdrop {
    background: #0006;
  }
  .filters-sheet header,
  .filters-sheet footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 16px 20px;
    gap: 12px;
  }
  .filters-sheet h2 {
    font-weight: 650;
  }
  .filters-sheet > div {
    padding: 0 20px;
  }
  .filters-sheet label {
    display: block;
    font-size: 12px;
    margin-bottom: 16px;
  }
  .filters-sheet select {
    width: 100%;
    display: block;
    margin-top: 8px;
  }
  .filters-sheet footer {
    padding-bottom: max(16px, env(safe-area-inset-bottom));
  }
  .filters-sheet .primary {
    flex: 1;
  }
  @container task-planner (max-width:640px) {
    .planner-header {
      padding: 14px 14px 10px;
    }
    h1 {
      font-size: 20px;
    }
    .new-desktop,
    .top-controls {
      display: none;
    }
    .search {
      margin: 0 14px 8px;
    }
    .status-controls {
      padding: 0 14px 10px;
      gap: 4px;
    }
    .status-controls > button {
      display: none;
    }
    .lane-filter {
      margin: 0;
      display: flex;
      width: 100%;
      align-items: center;
    }
    .lane-filter select {
      flex: 1;
      min-height: 44px;
    }
    .planner-scroll {
      padding: 0 14px 14px;
    }
    .cards {
      overflow: visible;
    }
    .board {
      height: auto;
      overflow: visible;
      display: block;
    }
    .lane {
      max-width: none;
      min-width: 0;
      margin-bottom: 12px;
    }
    .hint {
      display: none;
      font-size: 10px;
    }
    .mobile-controls {
      display: flex;
      flex-shrink: 0;
      gap: 4px;
      padding: 8px 10px max(8px, env(safe-area-inset-bottom));
      border-top: 1px solid hsl(var(--border));
      background: hsl(var(--card));
    }
    .mobile-controls button {
      flex: 1;
      min-width: 0;
      min-height: 48px;
      padding: 8px 4px;
      font-size: 11px;
    }
    .error,
    .filter-summary {
      margin-left: 14px;
      margin-right: 14px;
    }
    .pagination span {
      font-size: 10px;
    }
  }
  @media (max-width: 640px) {
    input, select { font-size: 16px; }
    .filters-sheet {
      width: 100%;
      max-width: 100vw;
      top: calc(var(--planning-viewport-top, 0px) + var(--planning-viewport-height, 100dvh));
      bottom: auto;
      transform: translateY(-100%);
      margin: 0;
      border-radius: 18px 18px 0 0;
    }
  }
</style>
