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
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div class="flex justify-between items-center">
				<h2 class="text-sm font-semibold">Provider Configuration</h2>
				<a href="/setup" class="text-xs text-primary hover:text-primary/80 border border-border rounded-md px-3 py-1.5 hover:bg-accent transition-colors">
					Change
				</a>
			</div>
			<dl class="space-y-2 text-sm">
				<div class="flex justify-between">
					<dt class="text-muted-foreground">Default provider</dt>
					<dd>{config.llm.default_provider}</dd>
				</div>
				{#if config.llm.local_model}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Local model</dt>
						<dd>{config.llm.local_model}</dd>
					</div>
				{/if}
				{#if config.llm.has_openai_key}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">OpenAI API key</dt>
						<dd class="text-emerald-400">configured</dd>
					</div>
				{/if}
				{#if config.llm.openai_base_url}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">OpenAI base URL</dt>
						<dd class="text-xs font-mono">{config.llm.openai_base_url}</dd>
					</div>
				{/if}
				{#if config.llm.has_anthropic_key}
					<div class="flex justify-between">
						<dt class="text-muted-foreground">Anthropic API key</dt>
						<dd class="text-emerald-400">configured</dd>
					</div>
				{/if}
			</dl>
		</div>

		<!-- Per-Agent Overrides -->
		<div class="space-y-4">
			<div>
				<h2 class="text-sm font-semibold">Per-Agent Overrides</h2>
				<p class="text-xs text-muted-foreground mt-1">Agents can override the default provider. Edit in the agent config page.</p>
			</div>
			{#each config.agents as agent}
				{#if agent.llm}
					<div class="rounded-lg border border-border bg-card p-4 space-y-2">
						<div class="flex justify-between items-center">
							<a href="/agents/{agent.name}" class="text-sm font-semibold hover:text-primary transition-colors">{agent.name}</a>
							<span class="text-xs text-muted-foreground bg-muted px-2 py-0.5 rounded">{agent.model ?? 'default'}</span>
						</div>
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
					</div>
				{/if}
			{/each}
		</div>
	{:else}
		<div class="rounded-lg border border-border bg-card p-4">
			<p class="text-sm text-muted-foreground">Loading...</p>
		</div>
	{/if}
</div>
