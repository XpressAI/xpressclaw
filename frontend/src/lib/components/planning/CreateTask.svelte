<script lang="ts">
  import { planningViewport } from "$lib/taskPlanning";
  import { onMount } from "svelte";
  import { tasks, type Agent, type Project, type Task } from "$lib/api";
  import { scheduleTimestamp, timezone } from "$lib/taskPlanning";
  let {
    agentList,
    projectList,
    initialAgent = "",
    initialBacklog = false,
    onclose,
    oncreated,
  }: {
    agentList: Agent[];
    projectList: Project[];
    initialAgent?: string;
    initialBacklog?: boolean;
    onclose: () => void;
    oncreated: () => void;
  } = $props();
  let dialog: HTMLDialogElement;
  let title = $state("");
  let description = $state("");
  let agentId = $state("");
  let priority = $state(0);
  let date = $state("");
  let newSession = $state(false);
  let backlog = $state(false);
  let dependencies = $state<string[]>([]);
  let choices = $state<Task[]>([]);
  let busy = $state(false);
  let error = $state("");
  let search = $state("");
  let request = 0;
  onMount(() => {
    agentId = initialAgent || agentList[0]?.id || "";
    backlog = initialBacklog;
    dialog.showModal();
    void loadDependencies();
  });
  async function loadDependencies() {
    const generation = ++request;
    try {
      const result = await tasks.planning({
        status: "active",
        search,
        limit: 100,
      });
      if (generation === request) choices = result.tasks;
    } catch (e) {
      error = String(e);
    }
  }
  async function create() {
    busy = true;
    error = "";
    try {
      await tasks.createBatch({
        tasks: [
          {
            ref: "task",
            title: title.trim(),
            description: description.trim() || undefined,
            agent_id: agentId,
            priority,
            backlog,
            start_after: scheduleTimestamp(date) ?? undefined,
            new_session: !dependencies.length && newSession,
            depends_on: dependencies.length ? dependencies : undefined,
          },
        ],
      });
      oncreated();
      dialog.close();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<dialog
  use:planningViewport
  bind:this={dialog}
  {onclose}
  aria-label="Create task"
  class="create-sheet"
>
  <form
    onsubmit={(e) => {
      e.preventDefault();
      void create();
    }}
  >
    <header>
      <h2>New task</h2>
      <button
        type="button"
        aria-label="Close new task"
        onclick={() => dialog.close()}>×</button
      >
    </header>
    <div class="body">
      <label
        >Task title<input
          required
          bind:value={title}
          placeholder="Task title..."
        /></label
      ><label
        >Description<textarea
          bind:value={description}
          placeholder="Description (optional)..."
          rows="2"
        ></textarea></label
      >
      <label
        >Agent<select required bind:value={agentId}
          >{#each agentList as agent}<option value={agent.id}
              >{projectList.find((p) => p.id === agent.project_id)?.name ??
                "No project"} · {agent.title || agent.name}</option
            >{/each}</select
        ></label
      >
      <label>Start in<select bind:value={backlog}><option value={false}>To do — pick up when eligible</option><option value={true}>Backlog — keep for later</option></select></label>
      <label
        >Priority<select bind:value={priority}
          ><option value={0}>Normal</option><option value={5}>High</option
          ><option value={10}>Urgent</option></select
        ></label
      >
      <label
        >Start no earlier than<input
          type="datetime-local"
          bind:value={date}
        /></label
      >
      <p>
        {timezone()}. To do tasks become eligible after this date. Backlog stays
        parked until you move it to To do; dependencies and agent availability still apply.
      </p>
      <details>
        <summary>Dependencies and conversation</summary><label
          >Find dependencies<input
            type="search"
            bind:value={search}
            oninput={loadDependencies}
          /></label
        >
        <div class="dependency-choices">
          {#each choices as task}<label
              ><input
                type="checkbox"
                value={task.id}
                bind:group={dependencies}
              />{task.title}</label
            >{/each}
        </div>
        <p>
          {dependencies.length} selected. Search to find tasks outside the first
          100 results.
        </p>
        <label class="check"
          ><input
            type="checkbox"
            bind:checked={newSession}
            disabled={dependencies.length > 0}
          />Start a fresh conversation</label
        >
      </details>
      {#if error}<p role="alert" class="error">{error}</p>{/if}
    </div>
    <footer>
      <button type="button" onclick={() => dialog.close()}>Cancel</button
      ><button class="primary" disabled={busy || !title.trim() || !agentId}
        >{busy
          ? "Saving…"
          : backlog
            ? "Add to Backlog"
            : date
              ? "Create scheduled task"
              : "Create in To do"}</button
      >
    </footer>
  </form>
</dialog>

<style>
  .create-sheet {
    width: min(540px, calc(100vw - 24px));
    max-height: calc(100dvh - 40px);
    margin: auto;
    border: 1px solid hsl(var(--border));
    border-radius: 18px;
    background: hsl(var(--background));
    color: hsl(var(--foreground));
    padding: 0;
  }
  .create-sheet::backdrop {
    background: #0006;
  }
  form {
    display: flex;
    flex-direction: column;
    max-height: inherit;
  }
  header,
  footer {
    padding: 16px 20px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    flex-shrink: 0;
  }
  header {
    border-bottom: 1px solid hsl(var(--border));
  }
  h2 {
    font-size: 17px;
    font-weight: 650;
  }
  .body {
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 0 20px 16px;
  }
  label {
    display: block;
    margin-top: 14px;
    font-size: 12px;
    font-weight: 500;
  }
  input,
  select,
  textarea {
    display: block;
    min-width: 0;
    max-width: 100%;
    width: 100%;
    margin-top: 6px;
  }
  button,
  input,
  select,
  textarea {
    border: 1px solid hsl(var(--border));
    border-radius: 8px;
    padding: 10px;
    min-height: 44px;
    background: hsl(var(--card));
    font-size: 12px;
  }
  p {
    font-size: 11px;
    color: hsl(var(--muted-foreground));
    line-height: 1.5;
    margin-top: 6px;
  }
  summary {
    font-size: 12px;
    cursor: pointer;
    margin-top: 16px;
  }
  footer {
    border-top: 1px solid hsl(var(--border));
    padding-bottom: max(16px, env(safe-area-inset-bottom));
  }
  .primary {
    background: hsl(var(--primary));
    color: hsl(var(--primary-foreground));
    flex: 1;
  }
  button:disabled {
    opacity: 0.5;
  }
  .error {
    color: hsl(var(--destructive));
  }
  .dependency-choices {
    max-height: 150px;
    overflow: auto;
  }
  .dependency-choices label,
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    font-weight: 400;
  }
  .dependency-choices input,
  .check input {
    width: 18px;
    min-height: 18px;
    margin: 0;
  }
  @media (max-width: 640px) {
    input, select, textarea { font-size: 16px; }
    .create-sheet {
      width: 100%;
      max-width: 100vw;
      top: calc(var(--planning-viewport-top, 0px) + var(--planning-viewport-height, 100dvh));
      bottom: auto;
      transform: translateY(-100%);
      margin: 0;
      border-radius: 18px 18px 0 0;
      max-height: calc(var(--planning-viewport-height, 100dvh) - 12px);
    }
  }
</style>
