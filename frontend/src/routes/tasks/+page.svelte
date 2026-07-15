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
	let formError = $state('');
	let creating = $state(false);

	const columns: { key: keyof TaskCounts; label: string; color: string }[] = [
		{ key: 'pending', label: 'Pending', color: 'text-yellow-400' },
		{ key: 'in_progress', label: 'In Progress', color: 'text-blue-400' },
		{ key: 'waiting_for_input', label: 'Waiting for Input', color: 'text-orange-400' },
		{ key: 'completed', label: 'Completed', color: 'text-emerald-400' },
		{ key: 'cancelled', label: 'Cancelled', color: 'text-red-400' }
	];

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

	function tasksByStatus(status: string): Task[] {
		return taskList.filter((t) => t.status === status);
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
				depends_on: newDependsOn.length > 0 ? newDependsOn : undefined
			}] });
			newTitle = '';
			newDesc = '';
			newPriority = 0;
			newDependsOn = [];
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
		return agent?.config?.display_name || agent?.name || agentId;
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
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
			Tasks need a session so they can run. <a href="/setup?mode=add-session" class="font-medium text-primary hover:underline">Create a session</a> first.
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
			<div class="flex gap-3">
				<div class="flex-1">
					<label class="block text-xs text-muted-foreground mb-1">Assign to Session</label>
					<select
						bind:value={newAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						{#each agentList as agent}
							<option value={agent.id}>{agent.config?.display_name || agent.name}</option>
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
							This task will wait for {newDependsOn.length} task{newDependsOn.length > 1 ? 's' : ''} to complete.
						</div>
					{/if}
				</div>
			{/if}
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

	<!-- Kanban columns -->
	<div class="grid grid-cols-1 lg:grid-cols-5 gap-4">
		{#each columns as col}
			{@const colTasks = tasksByStatus(col.key)}
			<div class="rounded-lg border border-border bg-card/50">
				<div class="border-b border-border px-4 py-3 flex items-center justify-between">
					<h2 class="text-sm font-semibold {col.color}">{col.label}</h2>
					<span class="text-xs text-muted-foreground">
						{colTasks.length}
					</span>
				</div>
				<div class="p-2 space-y-2 min-h-[200px]">
					{#each colTasks as task}
						<a href="/tasks/{task.id}" class="block rounded-md border border-border bg-card p-3 space-y-2 hover:border-primary/30 transition-colors">
							<div class="flex items-start justify-between gap-2">
								<span class="text-sm font-medium">{task.title}</span>
								<button
									onclick={(event) => { event.preventDefault(); event.stopPropagation(); deleteTask(task.id); }}
									class="text-xs text-muted-foreground hover:text-destructive shrink-0"
									title="Delete"
								>&times;</button>
							</div>
							{#if task.blocked_by && task.blocked_by.length > 0}
								<div class="text-xs text-amber-500">
									⏳ Waiting on {task.blocked_by.length} task{task.blocked_by.length > 1 ? 's' : ''}
								</div>
							{/if}
							{#if task.description}
								<p class="text-xs text-muted-foreground line-clamp-2">{task.description}</p>
							{/if}
							<div class="flex items-center justify-between">
								<div class="flex items-center gap-2">
									{#if task.agent_id}
										<span class="text-xs bg-muted px-1.5 py-0.5 rounded">{agentName(task.agent_id)}</span>
									{:else}
										<span class="text-xs text-muted-foreground italic">unassigned</span>
									{/if}
									{#if task.priority >= 10}
										<span class="text-xs text-red-400">urgent</span>
									{:else if task.priority >= 5}
										<span class="text-xs text-orange-400">high</span>
									{/if}
								</div>
								<div class="flex gap-1">
									{#if col.key !== 'completed' && col.key !== 'cancelled'}
										<button
											onclick={(event) => { event.preventDefault(); event.stopPropagation(); cancelTask(task.id); }}
											class="text-xs text-muted-foreground hover:text-red-400"
											title="Cancel task"
										>Cancel</button>
									{/if}
								</div>
							</div>
							{#if task.completed_at}
								<div class="text-xs text-muted-foreground">
									Completed {timeAgo(task.completed_at)}
								</div>
							{:else}
								<div class="text-xs text-muted-foreground">
									Created {timeAgo(task.created_at)}
								</div>
							{/if}
						</a>
					{:else}
						<div class="text-center text-xs text-muted-foreground py-8">No tasks</div>
					{/each}
				</div>
			</div>
		{/each}
	</div>
</div>
