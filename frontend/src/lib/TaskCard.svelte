<script lang="ts">
	import { onMount } from 'svelte';
	import { tasks } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	interface Props {
		taskId: string;
		initialTitle: string;
		initialStatus: string;
		createdAt: string;
	}

	let { taskId, initialTitle, initialStatus, createdAt }: Props = $props();

	let title = $state(initialTitle);
	let status = $state(initialStatus);
	let subtasksTotal = $state(0);
	let subtasksCompleted = $state(0);

	onMount(() => {
		// Poll for live status every 3 seconds while pending/in_progress
		const interval = setInterval(async () => {
			try {
				const task = await tasks.get(taskId);
				title = task.title;
				status = task.status;
				// Check subtasks
				const sub = await tasks.subtasks(taskId).catch(() => null);
				if (sub) {
					const c = sub.counts;
					subtasksTotal = c.pending + c.in_progress + c.waiting_for_input + c.blocked + c.completed + c.cancelled;
					subtasksCompleted = c.completed;
				}
			} catch { /* task may not exist yet */ }

			// Stop polling when terminal
			if (status === 'completed' || status === 'failed' || status === 'cancelled') {
				clearInterval(interval);
			}
		}, 3000);

		return () => clearInterval(interval);
	});

	const statusColor = $derived(
		status === 'completed' ? 'emerald' :
		status === 'failed' ? 'red' :
		status === 'in_progress' ? 'blue' : 'amber'
	);

	const statusLabel = $derived(
		status === 'in_progress' ? 'in progress' : status
	);
</script>

<div class="flex gap-3">
	<div class="flex-shrink-0 h-9 w-9 rounded-full flex items-center justify-center text-xs bg-secondary text-muted-foreground">
		&#x2611;
	</div>
	<div class="flex-1 max-w-[75%]">
		<div class="rounded-xl border px-4 py-3 text-sm
			{statusColor === 'emerald' ? 'border-emerald-500/30 bg-emerald-500/5' :
			 statusColor === 'red' ? 'border-red-500/30 bg-red-500/5' :
			 statusColor === 'blue' ? 'border-blue-500/30 bg-blue-500/5' :
			 'border-amber-500/30 bg-amber-500/5'}">
			<div class="flex items-center gap-2">
				<span class="h-2 w-2 rounded-full
					{statusColor === 'emerald' ? 'bg-emerald-400' :
					 statusColor === 'red' ? 'bg-red-400' :
					 statusColor === 'blue' ? 'bg-blue-400 animate-pulse' :
					 'bg-amber-400'}"></span>
				<span class="font-medium">{title}</span>
			</div>
			{#if subtasksTotal > 0}
				<div class="mt-2">
					<div class="flex items-center justify-between text-xs text-muted-foreground mb-1">
						<span>{subtasksCompleted}/{subtasksTotal} subtasks</span>
						<span>{Math.round((subtasksCompleted / subtasksTotal) * 100)}%</span>
					</div>
					<div class="h-1.5 rounded-full bg-muted overflow-hidden">
						<div class="h-full rounded-full transition-all duration-500
							{statusColor === 'emerald' ? 'bg-emerald-500' : statusColor === 'red' ? 'bg-red-500' : 'bg-blue-500'}"
							style="width: {(subtasksCompleted / subtasksTotal) * 100}%"></div>
					</div>
				</div>
			{/if}
			<div class="text-xs text-muted-foreground mt-1.5 flex items-center gap-2">
				<span>Task {statusLabel}</span>
				<span>&middot;</span>
				<span>{timeAgo(createdAt)}</span>
				<a href="/tasks/{taskId}" class="underline hover:text-foreground ml-auto">View task</a>
			</div>
		</div>
	</div>
</div>
