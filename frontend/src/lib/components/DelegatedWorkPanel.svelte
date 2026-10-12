<script lang="ts">
	import { onMount } from 'svelte';
	import { ArrowUpRight, GitBranch } from '@lucide/svelte';
	import type { Agent, DelegatedWork } from '$lib/api';
	import { elapsedTimeLabel, serverTimestampMs } from '$lib/serverTime';

	let { items, agents }: { items: DelegatedWork[]; agents: Agent[] } = $props();
	let now = $state(Date.now());
	let visible = $state(true);
	let outstanding = $derived(items.filter(item => item.status !== 'completed'));
	let completed = $derived(items.filter(item => item.status === 'completed'));

	onMount(() => {
		const updateVisibility = () => (visible = !document.hidden);
		updateVisibility();
		document.addEventListener('visibilitychange', updateVisibility);
		return () => document.removeEventListener('visibilitychange', updateVisibility);
	});
	$effect(() => {
		if (!visible || items.length === 0) return;
		now = Date.now();
		const timer = setInterval(() => { now = Date.now(); }, 1000);
		return () => clearInterval(timer);
	});

	function agentName(id: string | null): string {
		const agent = agents.find(agent => agent.id === id);
		return agent?.title || agent?.name || id || 'Unassigned';
	}

	function label(status: string): string {
		const labels: Record<string, string> = {
			running: 'Working', preparing: 'Starting', queued: 'Queued', idle: 'Not running',
			waiting_for_input: 'Needs input', awaiting_review: 'Awaiting review',
			waiting_for_subtasks: 'Waiting for delegated work', waiting_for_dependencies: 'Waiting for dependencies',
			completed: 'Completed', cancelled: 'Cancelled', failed: 'Failed', interrupted: 'Interrupted',
			blocked: 'Blocked', backlog: 'Backlog', scheduled: 'Scheduled',
		};
		return labels[status] ?? status.replaceAll('_', ' ');
	}

	function tone(status: string): string {
		if (['failed', 'blocked', 'cancelled', 'interrupted'].includes(status)) return 'bg-red-500/10 text-red-500';
		if (status === 'completed') return 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400';
		if (['running', 'preparing'].includes(status)) return 'bg-blue-500/10 text-blue-500';
		return 'bg-amber-500/10 text-amber-600 dark:text-amber-400';
	}

	function activityLabel(kind: string): string {
		const labels: Record<string, string> = {
			tool_call: 'Tool activity', agent_thought: 'Thinking', runner_progress: 'Progress update',
			attempt_queued: 'Work queued', attempt_preparing: 'Worker starting', attempt_running: 'Worker started',
			attempt_completed: 'Turn completed', attempt_failed: 'Turn failed', attempt_cancelled: 'Turn cancelled',
			attempt_interrupted: 'Turn interrupted', elicitation_requested: 'Input requested',
		};
		return labels[kind] ?? 'Activity recorded';
	}

	function duration(item: DelegatedWork): string | null {
		const start = serverTimestampMs(item.started_at);
		if (start === null) return null;
		const finish = serverTimestampMs(item.finished_at);
		if (finish !== null) return `Last run: ${elapsedTimeLabel(finish - start)}`;
		if (['running', 'preparing', 'waiting_for_input'].includes(item.activity_status)) {
			return `Elapsed: ${elapsedTimeLabel(now - start)}`;
		}
		return null;
	}

	function lastActivity(item: DelegatedWork): string | null {
		const timestamp = serverTimestampMs(item.latest_event_at);
		if (timestamp === null || !item.latest_event_type) return null;
		return `${activityLabel(item.latest_event_type)} · ${elapsedTimeLabel(now - timestamp)} ago`;
	}
</script>

{#snippet card(item: DelegatedWork)}
	<li class="min-w-0 rounded-lg border border-border bg-background px-3 py-2.5" data-delegated-task={item.task_id}>
		<div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
			<span class="font-medium text-foreground">{agentName(item.agent_id)}</span>
			<span class="rounded-full px-2 py-0.5 {tone(item.activity_status)}">{label(item.activity_status)}</span>
			{#if item.blocks_parent}<span class="text-muted-foreground">Required by this task</span>{/if}
		</div>
		<a href="/tasks/{item.task_id}" class="mt-1.5 flex items-start gap-1 text-sm font-medium text-foreground hover:text-primary hover:underline">
			<span class="min-w-0 break-words">{item.title}</span>
			<ArrowUpRight class="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden="true" />
		</a>
		{#if item.error_message}<p class="mt-2 whitespace-pre-wrap break-words text-xs text-destructive" data-delegated-error>{item.error_message}</p>{/if}
		<div class="mt-1.5 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
			{#if duration(item)}<span data-delegated-duration>{duration(item)}</span>{/if}
			{#if lastActivity(item)}<span title={item.latest_event_at ?? undefined}>{lastActivity(item)}</span>{/if}
			{#if !item.started_at && !item.latest_event_at}<span>Has not started</span>{/if}
		</div>
	</li>
{/snippet}

{#if items.length > 0}
	<section aria-label="Delegated work" class="my-3 rounded-xl border border-border bg-muted/25 p-3 sm:p-4">
		<div class="mb-3 flex items-center gap-2">
			<GitBranch class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
			<h3 class="text-sm font-semibold">Delegated work</h3>
			<span class="text-xs text-muted-foreground">{outstanding.length} open · {completed.length} completed</span>
		</div>
		{#if outstanding.length > 0}
			<ul class="space-y-2">{#each outstanding as item (item.task_id)}{@render card(item)}{/each}</ul>
		{/if}
		{#if completed.length > 0}
			<details class="mt-2">
				<summary class="cursor-pointer py-1 text-xs text-muted-foreground hover:text-foreground">Completed delegated work ({completed.length})</summary>
				<ul class="mt-2 space-y-2">{#each completed as item (item.task_id)}{@render card(item)}{/each}</ul>
			</details>
		{/if}
	</section>
{/if}
