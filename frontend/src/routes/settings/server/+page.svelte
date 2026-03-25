<script lang="ts">
	import { onMount } from 'svelte';
	import { health, setup } from '$lib/api';
	import type { LiveConfig } from '$lib/api';

	let serverInfo = $state<{ status: string; version: string } | null>(null);
	let config = $state<LiveConfig | null>(null);

	onMount(async () => {
		[serverInfo, config] = await Promise.all([
			health.check().catch(() => null),
			setup.getConfig().catch(() => null)
		]);
	});
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Server</h1>
		<p class="text-sm text-muted-foreground mt-1">Server status and system information</p>
	</div>

	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Status</h2>
		<dl class="space-y-2 text-sm">
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Health</dt>
				<dd class="{serverInfo?.status === 'ok' ? 'text-emerald-400' : 'text-red-400'}">
					{serverInfo?.status ?? 'Unknown'}
				</dd>
			</div>
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Version</dt>
				<dd>{serverInfo?.version ?? '—'}</dd>
			</div>
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Address</dt>
				<dd class="text-xs font-mono">{window.location.origin}</dd>
			</div>
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Isolation</dt>
				<dd>docker</dd>
			</div>
		</dl>
	</div>

	{#if config}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">System Defaults</h2>
			<p class="text-xs text-muted-foreground">Inherited by all agents unless overridden.</p>
			<dl class="space-y-2 text-sm">
				<div class="flex justify-between">
					<dt class="text-muted-foreground">Daily budget</dt>
					<dd>{config.system.budget.daily ?? 'none'}</dd>
				</div>
				{#if config.system.budget.monthly}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Monthly budget</dt>
						<dd>{config.system.budget.monthly}</dd>
					</div>
				{/if}
				<div class="flex justify-between">
					<dt class="text-muted-foreground">On budget exceeded</dt>
					<dd>{config.system.budget.on_exceeded}</dd>
				</div>
			</dl>
		</div>

		{#if config.mcp_servers.length > 0}
			<div class="rounded-lg border border-border bg-card p-4 space-y-3">
				<h2 class="text-sm font-semibold">MCP Servers</h2>
				<p class="text-xs text-muted-foreground">Registered connectors.</p>
				<div class="flex flex-wrap gap-2">
					{#each config.mcp_servers as server}
						<span class="text-xs bg-muted px-2 py-1 rounded">{server}</span>
					{/each}
				</div>
			</div>
		{/if}
	{/if}
</div>
