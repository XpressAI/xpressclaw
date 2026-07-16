<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, tasks } from '$lib/api';
	import type { Agent, Task, TaskCounts } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let taskList = $state<Task[]>([]);
	let agentList = $state<Agent[]>([]);
	let counts = $state<TaskCounts | null>(null);
	let loading = $state(true);
	let showCreate = $state(false);
	let newTitle = $state('');
	let newDesc = $state('');
	let newAgentId = $state('');
	let newPriority = $state(0);
	let newDependsOn = $state<string[]>([]);
	let newSession = $state(false);
	let filter = $state<'attention' | 'active' | 'all' | 'done'>('active');
	let formError = $state('');
	let creating = $state(false);

	let visibleTasks = $derived(taskList.filter((task) => {
		if (filter === 'attention') return ['waiting_for_input', 'blocked'].includes(task.status);
		if (filter === 'active') return ['pending', 'in_progress', 'waiting_for_input', 'blocked'].includes(task.status);
		if (filter === 'done') return ['completed', 'cancelled'].includes(task.status);
		return true;
	}));

	onMount(async () => {
		await Promise.all([load(), loadAgents()]);
	});

	async function loadAgents() {
		agentList = await agents.list().catch(() => []);
		if (!newAgentId && agentList.length > 0) newAgentId = agentList[0].id;
	}

	async function load() {
		loading = true;
		try {
			const result = await tasks.list();
			taskList = result.tasks;
			counts = result.counts;
		} catch {
			taskList = [];
		}
		loading = false;
	}

	function statusCount(key: keyof TaskCounts): number {
		if (!counts) return 0;
		return counts[key];
	}

	/** Existing incomplete tasks that can be selected as dependencies. */
	let availableDeps = $derived(taskList.filter(t => t.status !== 'completed' && t.status !== 'cancelled'));

	function toggleDep(id: string) {
		if (newDependsOn.includes(id)) {
			newDependsOn = newDependsOn.filter(d => d !== id);
		} else {
			newDependsOn = [...newDependsOn, id];
		}
	}

	async function createTask() {
		if (!newTitle.trim() || !newAgentId || creating) return;
		creating = true;
		formError = '';
		try {
			// Batch creation records dependencies before the dispatcher can
			// claim the task, even when the batch contains only one item.
			await tasks.createBatch({ tasks: [{
				ref: 'task',
				title: newTitle.trim(),
				description: newDesc.trim() || undefined,
				agent_id: newAgentId,
				priority: newPriority || undefined,
				new_session: newDependsOn.length === 0 && newSession,
				depends_on: newDependsOn.length > 0 ? newDependsOn : undefined
			}] });
			newTitle = '';
			newDesc = '';
			newPriority = 0;
			newDependsOn = [];
			newSession = false;
			showCreate = false;
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		} finally {
			creating = false;
		}
	}

	function openCreate() {
		if (agentList.length === 0) return;
		formError = '';
		if (!newAgentId) newAgentId = agentList[0].id;
		showCreate = !showCreate;
	}

	async function cancelTask(id: string) {
		await tasks.updateStatus(id, 'cancelled');
		await load();
	}

	async function deleteTask(id: string) {
		if (!confirm('Delete this task?')) return;
		await tasks.delete(id);
		await load();
	}

	function agentName(agentId: string | null): string | null {
		if (!agentId) return null;
		const agent = agentList.find((a) => a.id === agentId);
		return agent?.title || agent?.name || agentId;
	}

	function statusMeta(status: string): { label: string; dot: string; tone: string } {
		if (status === 'in_progress') return { label: 'Working', dot: 'bg-blue-400 animate-pulse', tone: 'text-blue-400' };
		if (status === 'pending') return { label: 'Queued', dot: 'bg-amber-400', tone: 'text-amber-400' };
		if (status === 'waiting_for_input') return { label: 'Waiting for you', dot: 'bg-orange-400 animate-pulse', tone: 'text-orange-400' };
		if (status === 'blocked') return { label: 'Blocked', dot: 'bg-red-400', tone: 'text-red-400' };
		if (status === 'completed') return { label: 'Completed', dot: 'bg-emerald-400', tone: 'text-emerald-400' };
		return { label: 'Cancelled', dot: 'bg-muted-foreground', tone: 'text-muted-foreground' };
	}
</script>

<div class="space-y-6 p-4 sm:p-6">
	<div class="flex items-center justify-between gap-3">
		<div>
			<h1 class="text-2xl font-bold">Tasks</h1>
			{#if counts}
				<p class="text-sm text-muted-foreground mt-1">
					{statusCount('pending')} pending, {statusCount('in_progress')} in progress, {statusCount('completed')} completed
				</p>
			{/if}
		</div>
		<button
			onclick={openCreate}
			disabled={agentList.length === 0}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors disabled:cursor-not-allowed disabled:opacity-50"
		>
			New Task
		</button>
	</div>

	{#if agentList.length === 0}
		<div class="rounded-lg border border-dashed border-border bg-card p-5 text-sm text-muted-foreground">
			Tasks need a project so they know which workspace and agent to use. <a href="/setup?mode=add-session" class="font-medium text-primary hover:underline">Create a project</a> first.
		</div>
	{/if}

	{#if showCreate}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<input
				type="text"
				placeholder="Task title..."
				bind:value={newTitle}
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<textarea
				placeholder="Description (optional)..."
				bind:value={newDesc}
				rows="2"
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring resize-none"
			></textarea>
			<div class="flex flex-col gap-3 sm:flex-row">
				<div class="flex-1">
					<label class="block text-xs text-muted-foreground mb-1">Project</label>
					<select
						bind:value={newAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						{#each agentList as agent}
							<option value={agent.id}>{agent.title || agent.name}</option>
						{/each}
					</select>
				</div>
				<div class="w-24">
					<label class="block text-xs text-muted-foreground mb-1">Priority</label>
					<select
						bind:value={newPriority}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						<option value={0}>Normal</option>
						<option value={5}>High</option>
						<option value={10}>Urgent</option>
					</select>
				</div>
			</div>
			{#if availableDeps.length > 0}
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Depends on (optional)</label>
					<div class="flex flex-wrap gap-1.5 max-h-24 overflow-y-auto">
						{#each availableDeps as dep}
							<button
								type="button"
								onclick={() => toggleDep(dep.id)}
								class="rounded-md border px-2 py-1 text-xs transition-colors
									{newDependsOn.includes(dep.id)
										? 'border-primary bg-primary/10 text-primary'
										: 'border-border text-muted-foreground hover:border-primary/50'}"
							>
								{dep.title}
							</button>
						{/each}
					</div>
					{#if newDependsOn.length > 0}
						<div class="text-xs text-muted-foreground mt-1">
							This task will wait, then continue the conversation it depends on.
						</div>
					{/if}
				</div>
			{/if}
			<label class="flex items-start gap-2 text-xs text-muted-foreground {newDependsOn.length > 0 ? 'opacity-50' : ''}">
				<input type="checkbox" bind:checked={newSession} disabled={newDependsOn.length > 0} class="mt-0.5 h-3.5 w-3.5 accent-primary" />
				<span><strong class="font-medium text-foreground">Start a fresh conversation</strong><br />Otherwise this continues the project’s active agent conversation.</span>
			</label>
			{#if formError}<p class="text-xs text-destructive">{formError}</p>{/if}
			<div class="flex gap-2">
				<button
					onclick={createTask}
					disabled={!newTitle.trim() || !newAgentId || creating}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
				>
					{creating ? 'Queuing…' : 'Create and queue'}
				</button>
				<button
					onclick={() => (showCreate = false)}
					class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent"
				>
					Cancel
				</button>
			</div>
		</div>
	{/if}

	<div class="flex gap-1 overflow-x-auto rounded-xl border border-border bg-card p-1">
		{#each [
			{ id: 'attention', label: 'Needs you', count: statusCount('waiting_for_input') + statusCount('blocked') },
			{ id: 'active', label: 'Active', count: statusCount('pending') + statusCount('in_progress') },
			{ id: 'all', label: 'All', count: taskList.length },
			{ id: 'done', label: 'Done', count: statusCount('completed') }
		] as item}
			<button onclick={() => (filter = item.id as typeof filter)} class="shrink-0 rounded-lg px-3 py-2 text-xs font-medium {filter === item.id ? 'bg-secondary text-foreground' : 'text-muted-foreground hover:text-foreground'}">
				{item.label} <span class="ml-1 opacity-70">{item.count}</span>
			</button>
		{/each}
	</div>

	<div class="overflow-hidden rounded-xl border border-border bg-card">
		{#each visibleTasks as task (task.id)}
			{@const meta = statusMeta(task.status)}
			<a href="/tasks/{task.id}" class="group flex items-start gap-3 border-b border-border px-4 py-4 last:border-b-0 hover:bg-accent/30">
				<span class="mt-1.5 h-2.5 w-2.5 shrink-0 rounded-full {meta.dot}"></span>
				<div class="min-w-0 flex-1">
					<div class="flex items-start justify-between gap-3">
						<h2 class="min-w-0 truncate text-sm font-medium group-hover:text-primary">{task.title}</h2>
						<span class="shrink-0 text-[11px] {meta.tone}">{meta.label}</span>
					</div>
					{#if task.description}<p class="mt-1 line-clamp-2 text-xs text-muted-foreground">{task.description}</p>{/if}
					<div class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground">
						<span>{agentName(task.agent_id) ?? 'Unassigned'}</span><span>·</span><span>{timeAgo(task.updated_at)}</span>
						{#if task.blocked_by && task.blocked_by.length > 0}<span class="text-amber-500">· Waiting on {task.blocked_by.length}</span>{/if}
						{#if task.priority >= 5}<span class="text-orange-400">· High priority</span>{/if}
					</div>
				</div>
				<div class="flex shrink-0 items-center gap-2">
					{#if !['completed', 'cancelled'].includes(task.status)}<button onclick={(event) => { event.preventDefault(); event.stopPropagation(); cancelTask(task.id); }} class="hidden text-xs text-muted-foreground hover:text-destructive sm:block">Cancel</button>{/if}
					<button onclick={(event) => { event.preventDefault(); event.stopPropagation(); deleteTask(task.id); }} aria-label="Delete task" class="hidden text-lg leading-none text-muted-foreground hover:text-destructive sm:block">×</button>
				</div>
			</a>
		{:else}
			<div class="px-4 py-16 text-center text-sm text-muted-foreground">No tasks in this view.</div>
		{/each}
	</div>
</div>
