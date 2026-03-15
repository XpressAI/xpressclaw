<script lang="ts">
	import { onMount } from 'svelte';
	import { tasks } from '$lib/api';
	import type { Task, TaskCounts } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let taskList = $state<Task[]>([]);
	let counts = $state<TaskCounts | null>(null);
	let filter = $state<string>('');
	let loading = $state(true);
	let showCreate = $state(false);
	let newTitle = $state('');
	let newDesc = $state('');

	const columns = [
		{ key: 'pending', label: 'Pending' },
		{ key: 'in_progress', label: 'In Progress' },
		{ key: 'completed', label: 'Completed' }
	];

	onMount(() => load());

	async function load() {
		loading = true;
		try {
			const result = await tasks.list(filter || undefined);
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

	async function createTask() {
		if (!newTitle.trim()) return;
		await tasks.create({ title: newTitle, description: newDesc || undefined });
		newTitle = '';
		newDesc = '';
		showCreate = false;
		await load();
	}

	async function moveTask(id: string, status: string) {
		await tasks.updateStatus(id, status);
		await load();
	}

	async function deleteTask(id: string) {
		if (!confirm('Delete this task?')) return;
		await tasks.delete(id);
		await load();
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Tasks</h1>
			<p class="text-sm text-muted-foreground mt-1">
				{counts ? `${counts.pending} pending, ${counts.in_progress} in progress, ${counts.completed} completed` : 'Loading...'}
			</p>
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
	<div class="grid grid-cols-1 lg:grid-cols-3 gap-4">
		{#each columns as col}
			<div class="rounded-lg border border-border bg-card/50">
				<div class="border-b border-border px-4 py-3 flex items-center justify-between">
					<h2 class="text-sm font-semibold">{col.label}</h2>
					<span class="text-xs text-muted-foreground">
						{tasksByStatus(col.key).length}
					</span>
				</div>
				<div class="p-2 space-y-2 min-h-[200px]">
					{#each tasksByStatus(col.key) as task}
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
								<div class="text-xs text-muted-foreground">
									{#if task.agent_id}
										<span class="bg-muted px-1.5 py-0.5 rounded">{task.agent_id}</span>
									{/if}
								</div>
								<div class="flex gap-1">
									{#if col.key === 'pending'}
										<button
											onclick={() => moveTask(task.id, 'in_progress')}
											class="text-xs text-blue-400 hover:underline"
										>Start</button>
									{:else if col.key === 'in_progress'}
										<button
											onclick={() => moveTask(task.id, 'completed')}
											class="text-xs text-emerald-400 hover:underline"
										>Complete</button>
									{/if}
								</div>
							</div>
						</div>
					{:else}
						<div class="text-center text-xs text-muted-foreground py-8">No tasks</div>
					{/each}
				</div>
			</div>
		{/each}
	</div>
</div>
