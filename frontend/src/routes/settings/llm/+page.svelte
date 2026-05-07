<script lang="ts">
	import { onMount } from 'svelte';
	import { setup } from '$lib/api';
	import type { LiveConfig } from '$lib/api';

	let config = $state<LiveConfig | null>(null);

	onMount(async () => {
		config = await setup.getConfig().catch(() => null);
	});
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">LLM Providers</h1>
		<p class="text-sm text-muted-foreground mt-1">Model providers and API configuration</p>
	</div>

	{#if config}
		<div class="rounded-lg border border-border bg-card p-4">
			<p class="text-sm text-muted-foreground">
				Each agent declares its own provider, model, API key, and base URL.
				There is no global LLM configuration — to change a provider, edit the
				agent on its config page.
			</p>
		</div>

		<!-- Per-Agent LLM configuration -->
		<div class="space-y-4">
			<div>
				<h2 class="text-sm font-semibold">Per-Agent Configuration</h2>
				<p class="text-xs text-muted-foreground mt-1">Click an agent to edit its provider and model.</p>
			</div>
			{#if config.agents.length === 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<p class="text-sm text-muted-foreground">No agents configured yet.</p>
				</div>
			{/if}
			{#each config.agents as agent}
				<div class="rounded-lg border border-border bg-card p-4 space-y-2">
					<div class="flex justify-between items-center">
						<a href="/agents/{agent.name}" class="text-sm font-semibold hover:text-primary transition-colors">{agent.display_name ?? agent.name}</a>
						<span class="text-xs text-muted-foreground bg-muted px-2 py-0.5 rounded">{agent.llm?.model ?? agent.model ?? '(no model)'}</span>
					</div>
					{#if agent.llm}
						<dl class="space-y-1 text-sm">
							{#if agent.llm.provider}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Provider</dt>
									<dd>{agent.llm.provider}</dd>
								</div>
							{/if}
							{#if agent.llm.base_url}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Base URL</dt>
									<dd class="text-xs font-mono">{agent.llm.base_url}</dd>
								</div>
							{/if}
							{#if agent.llm.api_key}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">API key</dt>
									<dd class="text-emerald-400">set</dd>
								</div>
							{/if}
						</dl>
					{:else}
						<p class="text-xs text-amber-500">No LLM configured for this agent.</p>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="rounded-lg border border-border bg-card p-4">
			<p class="text-sm text-muted-foreground">Loading...</p>
		</div>
	{/if}
</div>
