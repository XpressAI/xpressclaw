<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, tasks, memory, activity, health } from '$lib/api';
	import type { Agent, TaskCounts, MemoryStats, ActivityEvent } from '$lib/api';
	import { timeAgo, formatCost, statusColor } from '$lib/utils';

	let agentList = $state<Agent[]>([]);
	let taskCounts = $state<TaskCounts | null>(null);
	let memoryStats = $state<MemoryStats | null>(null);
	let recentActivity = $state<ActivityEvent[]>([]);
	let serverStatus = $state<string>('checking...');
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const [h, a, t, m, act] = await Promise.all([
				health.check().catch(() => null),
				agents.list().catch(() => []),
				tasks.list().catch(() => ({ tasks: [], counts: null })),
				memory.stats().catch(() => null),
				activity.list(10).catch(() => [])
			]);

			serverStatus = h ? 'online' : 'offline';
			agentList = a;
			taskCounts = t.counts;
			memoryStats = m;
			recentActivity = act;
		} catch (e) {
			error = String(e);
		}
	});

	function totalTasks(c: TaskCounts): number {
		return c.pending + c.in_progress + c.waiting_for_input + c.blocked + c.completed + c.cancelled;
	}
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Dashboard</h1>
		<p class="text-sm text-muted-foreground mt-1">
			Server: <span class="{serverStatus === 'online' ? 'text-emerald-400' : 'text-red-400'}">{serverStatus}</span>
		</p>
	</div>

	{#if error}
		<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">
			{error}
		</div>
	{/if}

	<!-- Stats cards -->
	<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
		<div class="rounded-lg border border-border bg-card p-4">
			<div class="text-sm text-muted-foreground">Agents</div>
			<div class="mt-1 text-2xl font-bold">{agentList.length}</div>
			<div class="mt-1 text-xs text-muted-foreground">
				{agentList.filter(a => a.status === 'running').length} running
			</div>
		</div>

		<div class="rounded-lg border border-border bg-card p-4">
			<div class="text-sm text-muted-foreground">Tasks</div>
			<div class="mt-1 text-2xl font-bold">{taskCounts ? totalTasks(taskCounts) : '—'}</div>
			<div class="mt-1 text-xs text-muted-foreground">
				{taskCounts?.in_progress ?? 0} in progress, {taskCounts?.pending ?? 0} pending
			</div>
		</div>

		<div class="rounded-lg border border-border bg-card p-4">
			<div class="text-sm text-muted-foreground">Memories</div>
			<div class="mt-1 text-2xl font-bold">{memoryStats?.zettelkasten.total_memories ?? '—'}</div>
			<div class="mt-1 text-xs text-muted-foreground">
				{memoryStats?.vector.embedding_count ?? 0} embeddings
			</div>
		</div>

		<div class="rounded-lg border border-border bg-card p-4">
			<div class="text-sm text-muted-foreground">Links</div>
			<div class="mt-1 text-2xl font-bold">{memoryStats?.zettelkasten.total_links ?? '—'}</div>
			<div class="mt-1 text-xs text-muted-foreground">
				{memoryStats?.zettelkasten.total_tags ?? 0} tags
			</div>
		</div>
	</div>

	<!-- Two columns: Agents + Recent Activity -->
	<div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
		<!-- Agents -->
		<div class="rounded-lg border border-border bg-card">
			<div class="flex items-center justify-between border-b border-border px-4 py-3">
				<h2 class="text-sm font-semibold">Agents</h2>
				<a href="/agents" class="text-xs text-muted-foreground hover:text-foreground">View all</a>
			</div>
			<div class="divide-y divide-border">
				{#each agentList.slice(0, 5) as agent}
					<a href="/agents/{agent.id}" class="flex items-center gap-3 px-4 py-3 hover:bg-accent/50 transition-colors">
						<div class="h-2 w-2 rounded-full {agent.status === 'running' ? 'bg-emerald-400' : 'bg-muted-foreground/30'}"></div>
						<div class="flex-1 min-w-0">
							<div class="text-sm font-medium truncate">{agent.name}</div>
							<div class="text-xs text-muted-foreground">{agent.backend}</div>
						</div>
						<span class="text-xs {statusColor(agent.status)}">{agent.status}</span>
					</a>
				{:else}
					<div class="px-4 py-6 text-center text-sm text-muted-foreground">No agents configured</div>
				{/each}
			</div>
		</div>

		<!-- Recent Activity -->
		<div class="rounded-lg border border-border bg-card">
			<div class="flex items-center justify-between border-b border-border px-4 py-3">
				<h2 class="text-sm font-semibold">Recent Activity</h2>
			</div>
			<div class="divide-y divide-border">
				{#each recentActivity.slice(0, 8) as event}
					{@const data = typeof event.event_data === 'string' ? (() => { try { return JSON.parse(event.event_data); } catch { return event.event_data; } })() : event.event_data}
					<a href={event.agent_id ? `/agents/${event.agent_id}?tab=logs` : '#'}
						class="block px-4 py-2.5 hover:bg-accent/50 transition-colors">
						<div class="flex items-center justify-between">
							<span class="text-xs font-medium {event.event_type === 'task_completed' ? 'text-emerald-400' : event.event_type === 'agent_response' ? 'text-blue-400' : 'text-muted-foreground'}">
								{event.event_type === 'agent_response' ? 'Conversation' : event.event_type === 'task_completed' ? 'Task completed' : event.event_type.replace('_', ' ')}
							</span>
							<span class="text-xs text-muted-foreground">{timeAgo(event.timestamp)}</span>
						</div>
						<div class="text-xs text-muted-foreground mt-0.5">
							{#if event.agent_id}
								<span class="text-foreground/70">{event.agent_id}</span>
							{/if}
							{#if data?.title}
								— {data.title}
							{/if}
						</div>
					</a>
				{:else}
					<div class="px-4 py-6 text-center text-sm text-muted-foreground">No recent activity</div>
				{/each}
			</div>
		</div>
	</div>
</div>
