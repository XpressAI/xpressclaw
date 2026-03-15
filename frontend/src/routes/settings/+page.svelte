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
		<h1 class="text-2xl font-bold">Settings</h1>
		<p class="text-sm text-muted-foreground mt-1">System configuration</p>
	</div>

	<!-- Server info -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Server</h2>
		<dl class="space-y-2 text-sm">
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Status</dt>
				<dd class="{serverInfo?.status === 'ok' ? 'text-emerald-400' : 'text-red-400'}">
					{serverInfo?.status ?? 'Unknown'}
				</dd>
			</div>
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Version</dt>
				<dd>{serverInfo?.version ?? '—'}</dd>
			</div>
		</dl>
	</div>

	{#if config}
		<!-- LLM Provider -->
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">LLM Provider</h2>
			<dl class="space-y-2 text-sm">
				<div class="flex justify-between">
					<dt class="text-muted-foreground">Provider</dt>
					<dd>{config.llm.default_provider}</dd>
				</div>
				{#if config.llm.local_model}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Model</dt>
						<dd>{config.llm.local_model}</dd>
					</div>
				{/if}
				{#if config.llm.has_openai_key}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">OpenAI API Key</dt>
						<dd class="text-emerald-400">configured</dd>
					</div>
				{/if}
				{#if config.llm.openai_base_url}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">OpenAI Base URL</dt>
						<dd class="text-xs">{config.llm.openai_base_url}</dd>
					</div>
				{/if}
				{#if config.llm.has_anthropic_key}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Anthropic API Key</dt>
						<dd class="text-emerald-400">configured</dd>
					</div>
				{/if}
			</dl>
		</div>

		<!-- Agents -->
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">Agents</h2>
			<div class="space-y-3">
				{#each config.agents as agent}
					<div class="rounded-md border border-border p-3 space-y-1">
						<div class="flex justify-between items-center">
							<span class="text-sm font-medium">{agent.name}</span>
							<span class="text-xs text-muted-foreground bg-muted px-2 py-0.5 rounded">{agent.backend}</span>
						</div>
						{#if agent.model}
							<div class="text-xs text-muted-foreground">Model: {agent.model}</div>
						{/if}
						{#if agent.tools.length > 0}
							<div class="text-xs text-muted-foreground">Tools: {agent.tools.join(', ')}</div>
						{/if}
					</div>
				{/each}
			</div>
		</div>

		<!-- Budget -->
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">Budget</h2>
			<dl class="space-y-2 text-sm">
				<div class="flex justify-between">
					<dt class="text-muted-foreground">Daily limit</dt>
					<dd>{config.system.budget.daily}</dd>
				</div>
				{#if config.system.budget.monthly}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Monthly limit</dt>
						<dd>{config.system.budget.monthly}</dd>
					</div>
				{/if}
				<div class="flex justify-between">
					<dt class="text-muted-foreground">On exceeded</dt>
					<dd>{config.system.budget.on_exceeded}</dd>
				</div>
			</dl>
		</div>

		<!-- MCP Servers -->
		{#if config.mcp_servers.length > 0}
			<div class="rounded-lg border border-border bg-card p-4 space-y-3">
				<h2 class="text-sm font-semibold">Connectors (MCP)</h2>
				<div class="flex flex-wrap gap-2">
					{#each config.mcp_servers as server}
						<span class="text-xs bg-muted px-2 py-1 rounded">{server}</span>
					{/each}
				</div>
			</div>
		{/if}

		<!-- Config file path -->
		<p class="text-xs text-muted-foreground">
			Configuration is stored in <code class="bg-muted px-1.5 py-0.5 rounded">~/.xpressclaw/xpressclaw.yaml</code>
		</p>
	{:else}
		<div class="rounded-lg border border-border bg-card p-4">
			<p class="text-sm text-muted-foreground">Loading configuration...</p>
		</div>
	{/if}
</div>
