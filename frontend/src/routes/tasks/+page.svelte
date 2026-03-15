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

	async function createTask() {
		if (!newTitle.trim()) return;
		await tasks.create({
			title: newTitle,
			description: newDesc || undefined,
			agent_id: newAgentId || undefined,
			priority: newPriority || undefined
		});
		newTitle = '';
		newDesc = '';
		newAgentId = '';
		newPriority = 0;
		showCreate = false;
		await load();
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
		return agent?.name ?? agentId;
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
			onclick={() => (showCreate = !showCreate)}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			New Task
		</button>
	</div>

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
					<label class="block text-xs text-muted-foreground mb-1">Assign to Agent</label>
					<select
						bind:value={newAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						<option value="">Unassigned</option>
						{#each agentList as agent}
							<option value={agent.id}>{agent.name}</option>
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
			<div class="flex gap-2">
				<button
					onclick={createTask}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90"
				>
					Create
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
						<div class="rounded-md border border-border bg-card p-3 space-y-2">
							<div class="flex items-start justify-between gap-2">
								<span class="text-sm font-medium">{task.title}</span>
								<button
									onclick={() => deleteTask(task.id)}
									class="text-xs text-muted-foreground hover:text-destructive shrink-0"
									title="Delete"
								>&times;</button>
							</div>
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
											onclick={() => cancelTask(task.id)}
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
						</div>
					{:else}
						<div class="text-center text-xs text-muted-foreground py-8">No tasks</div>
					{/each}
				</div>
			</div>
		{/each}
	</div>
</div>
