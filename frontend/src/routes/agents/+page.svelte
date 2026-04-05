<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { agents } from '$lib/api';
	import type { Agent } from '$lib/api';
	import { statusColor, timeAgo, agentAvatar } from '$lib/utils';

	let agentList = $state<Agent[]>([]);
	let loading = $state(true);
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	onMount(async () => {
		agentList = await agents.list().catch(() => []);
		loading = false;
		// Poll every 5s so reconciler progress is visible
		pollTimer = setInterval(async () => {
			agentList = await agents.list().catch(() => agentList);
		}, 5000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function handleStart(id: string) {
		try {
			await agents.start(id);
			agentList = await agents.list();
		} catch (e) {
			alert(String(e));
		}
	}

	async function handleStop(id: string) {
		try {
			await agents.stop(id);
			agentList = await agents.list();
		} catch (e) {
			alert(String(e));
		}
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Agents</h1>
			<p class="text-sm text-muted-foreground mt-1">{agentList.length} configured</p>
		</div>
		<a href="/setup?mode=add-agent"
			class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90">
			+ Add Agent
		</a>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if agentList.length === 0}
		<div class="rounded-lg border border-border bg-card p-8 text-center">
			<p class="text-muted-foreground">No agents configured.</p>
			<p class="text-sm text-muted-foreground mt-2">Complete the <a href="/setup" class="text-primary hover:underline">setup wizard</a> to add agents.</p>
		</div>
	{:else}
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each agentList as agent}
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<div class="flex items-start justify-between">
						<div class="flex items-center gap-3">
							<img src={agentAvatar(agent)} alt="" class="h-9 w-9 rounded-full object-cover flex-shrink-0" />
							<div>
								<a href="/agents/{agent.id}" class="text-sm font-semibold hover:underline">{agent.config?.display_name || agent.name}</a>
								<div class="text-xs text-muted-foreground mt-0.5">{agent.backend}</div>
							</div>
						</div>
						<span class="inline-flex items-center gap-1.5 text-xs {statusColor(agent.status)}">
							<span class="h-1.5 w-1.5 rounded-full {agent.status === 'running' ? 'bg-emerald-400' : agent.status === 'starting' || agent.status === 'stopping' ? 'bg-amber-400 animate-pulse' : agent.status === 'error' ? 'bg-red-400' : 'bg-muted-foreground/30'}"></span>
							{agent.status}
							{#if agent.restart_count > 0 && agent.desired_status === 'running' && agent.status !== 'running'}
								<span class="text-muted-foreground">(attempt {agent.restart_count})</span>
							{/if}
						</span>
					</div>

					{#if agent.error_message}
						<div class="text-xs text-destructive bg-destructive/10 rounded px-2 py-1">{agent.error_message}</div>
					{/if}

					<div class="text-xs text-muted-foreground">
						Created {timeAgo(agent.created_at)}
						{#if agent.started_at}
							&middot; Started {timeAgo(agent.started_at)}
						{/if}
					</div>

					<div class="flex gap-2">
						{#if agent.desired_status === 'running'}
							<button
								onclick={() => handleStop(agent.id)}
								class="rounded-md border border-border bg-secondary px-3 py-1.5 text-xs font-medium hover:bg-accent transition-colors"
							>
								Stop
							</button>
						{:else}
							<button
								onclick={() => handleStart(agent.id)}
								class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
							>
								Start
							</button>
						{/if}
						<a
							href="/agents/{agent.id}"
							class="rounded-md border border-border bg-secondary px-3 py-1.5 text-xs font-medium hover:bg-accent transition-colors"
						>
							Details
						</a>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
