<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import { agents } from '$lib/api';
	import type { Agent } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let agent = $state<Agent | null>(null);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			agent = await agents.get($page.params.id);
		} catch (e) {
			error = String(e);
		}
	});

	async function handleStart() {
		if (!agent) return;
		try {
			agent = await agents.start(agent.id);
		} catch (e) {
			alert(String(e));
		}
	}

	async function handleStop() {
		if (!agent) return;
		try {
			agent = await agents.stop(agent.id);
		} catch (e) {
			alert(String(e));
		}
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center gap-2 text-sm text-muted-foreground">
		<a href="/agents" class="hover:text-foreground">Agents</a>
		<span>/</span>
		<span class="text-foreground">{agent?.name ?? '...'}</span>
	</div>

	{#if error}
		<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
	{:else if agent}
		<div class="flex items-start justify-between">
			<div>
				<h1 class="text-2xl font-bold">{agent.name}</h1>
				<p class="text-sm text-muted-foreground mt-1">
					<span class="{statusColor(agent.status)}">{agent.status}</span>
					&middot; {agent.backend}
				</p>
			</div>
			<div class="flex gap-2">
				{#if agent.status === 'running'}
					<button
						onclick={handleStop}
						class="rounded-md border border-border bg-secondary px-4 py-2 text-sm font-medium hover:bg-accent transition-colors"
					>
						Stop
					</button>
				{:else}
					<button
						onclick={handleStart}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
					>
						Start
					</button>
				{/if}
			</div>
		</div>

		<div class="grid grid-cols-1 md:grid-cols-2 gap-4">
			<div class="rounded-lg border border-border bg-card p-4 space-y-3">
				<h2 class="text-sm font-semibold">Details</h2>
				<dl class="space-y-2 text-sm">
					<div class="flex justify-between">
						<dt class="text-muted-foreground">ID</dt>
						<dd class="font-mono text-xs">{agent.id}</dd>
					</div>
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Backend</dt>
						<dd>{agent.backend}</dd>
					</div>
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Status</dt>
						<dd class="{statusColor(agent.status)}">{agent.status}</dd>
					</div>
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Created</dt>
						<dd>{timeAgo(agent.created_at)}</dd>
					</div>
					{#if agent.started_at}
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Started</dt>
							<dd>{timeAgo(agent.started_at)}</dd>
						</div>
					{/if}
					{#if agent.container_id}
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Container</dt>
							<dd class="font-mono text-xs truncate max-w-[200px]">{agent.container_id}</dd>
						</div>
					{/if}
				</dl>
			</div>

			{#if agent.error_message}
				<div class="rounded-lg border border-destructive/50 bg-card p-4 space-y-2">
					<h2 class="text-sm font-semibold text-destructive">Error</h2>
					<pre class="text-xs text-muted-foreground whitespace-pre-wrap">{agent.error_message}</pre>
				</div>
			{/if}
		</div>
	{:else}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{/if}
</div>
