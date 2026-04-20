<script lang="ts">
	import { onMount } from 'svelte';
	import { tasks as tasksApi } from '$lib/api';
	import type { Task } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	interface Props {
		agentId: string;
	}

	let { agentId }: Props = $props();

	let taskList = $state<Task[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Create task form
	let showCreateForm = $state(false);
	let newTaskTitle = $state('');
	let creating = $state(false);
	let composing = $state(false);

	onMount(() => {
		loadTasks();
	});

	async function loadTasks() {
		loading = true;
		error = null;
		try {
			const result = await tasksApi.list(undefined, agentId);
			taskList = result.tasks;
		} catch (e) {
			error = `Failed to load tasks: ${e}`;
		}
		loading = false;
	}

	async function createTask() {
		if (!newTaskTitle.trim() || creating) return;
		creating = true;
		try {
			await tasksApi.create({
				title: newTaskTitle.trim(),
				agent_id: agentId,
			});
			newTaskTitle = '';
			showCreateForm = false;
			await loadTasks();
		} catch (e) {
			error = `Failed to create task: ${e}`;
		}
		creating = false;
	}
</script>

<div class="space-y-6">
	<div class="rounded-lg border border-border bg-card p-4 space-y-4">
		<div class="flex items-center justify-between">
			<h2 class="text-sm font-semibold">Tasks</h2>
			<button
				onclick={() => { showCreateForm = !showCreateForm; }}
				class="rounded-md border border-border px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
			>
				{showCreateForm ? 'Cancel' : 'Create Task'}
			</button>
		</div>

		{#if showCreateForm}
			<div class="flex gap-2">
				<input
					type="text"
					bind:value={newTaskTitle}
					placeholder="Task title..."
					onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter' && !e.isComposing && !composing && e.keyCode !== 229) createTask(); }}
					oncompositionstart={() => (composing = true)}
					oncompositionend={() => setTimeout(() => (composing = false), 0)}
					class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
				/>
				<button
					onclick={createTask}
					disabled={!newTaskTitle.trim() || creating}
					class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
				>
					{creating ? 'Creating...' : 'Add'}
				</button>
			</div>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading tasks...</p>
		{:else if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{error}
			</div>
		{:else if taskList.length === 0}
			<p class="text-sm text-muted-foreground italic">No tasks assigned to this agent.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full text-sm">
					<thead>
						<tr class="border-b border-border text-left">
							<th class="py-2 pr-4 text-xs font-medium text-muted-foreground">Title</th>
							<th class="py-2 pr-4 text-xs font-medium text-muted-foreground">Status</th>
							<th class="py-2 text-xs font-medium text-muted-foreground">Created</th>
						</tr>
					</thead>
					<tbody>
						{#each taskList as task}
							<tr class="border-b border-border/50 hover:bg-accent/30">
								<td class="py-2 pr-4">
									<a href="/tasks/{task.id}" class="text-foreground hover:text-primary transition-colors">
										{task.title}
									</a>
								</td>
								<td class="py-2 pr-4">
									<span class="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium {statusColor(task.status)}">
										{task.status}
									</span>
								</td>
								<td class="py-2 text-xs text-muted-foreground">
									{timeAgo(task.created_at)}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>
</div>
