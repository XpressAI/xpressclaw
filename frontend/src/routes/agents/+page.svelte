<script lang="ts">
	import { onMount } from 'svelte';
	import { agents } from '$lib/api';
	import type { Agent } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let agentList = $state<Agent[]>([]);
	let loading = $state(true);

	onMount(async () => {
		agentList = await agents.list().catch(() => []);
		loading = false;
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
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if agentList.length === 0}
		<div class="rounded-lg border border-border bg-card p-8 text-center">
			<p class="text-muted-foreground">No agents configured.</p>
			<p class="text-sm text-muted-foreground mt-2">Add agents to your <code class="text-xs bg-muted px-1 py-0.5 rounded">xpressclaw.yaml</code> and run <code class="text-xs bg-muted px-1 py-0.5 rounded">xpressclaw up</code></p>
		</div>
	{:else}
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each agentList as agent}
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<div class="flex items-start justify-between">
						<div>
							<a href="/agents/{agent.id}" class="text-sm font-semibold hover:underline">{agent.name}</a>
							<div class="text-xs text-muted-foreground mt-0.5">{agent.backend}</div>
						</div>
						<span class="inline-flex items-center gap-1.5 text-xs {statusColor(agent.status)}">
							<span class="h-1.5 w-1.5 rounded-full {agent.status === 'running' ? 'bg-emerald-400' : agent.status === 'error' ? 'bg-red-400' : 'bg-muted-foreground/30'}"></span>
							{agent.status}
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
						{#if agent.status === 'running'}
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
