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

	<!-- Server -->
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
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Isolation</dt>
				<dd>docker</dd>
			</div>
		</dl>
	</div>

	{#if config}
		<!-- LLM Providers -->
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div class="flex justify-between items-center">
				<div>
					<h2 class="text-sm font-semibold">LLM Providers</h2>
					<p class="text-xs text-muted-foreground">Available providers. Each agent selects its own model.</p>
				</div>
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
						<dd class="text-xs">{config.llm.openai_base_url}</dd>
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

		<!-- System Defaults -->
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">System Defaults</h2>
			<p class="text-xs text-muted-foreground">Default settings inherited by all agents unless overridden.</p>
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

		<!-- Per-Agent Configuration -->
		<div class="space-y-4">
			<div>
				<h2 class="text-sm font-semibold">Agents</h2>
				<p class="text-xs text-muted-foreground mt-1">Per-agent settings override system defaults.</p>
			</div>
			{#each config.agents as agent}
				<div class="rounded-lg border border-border bg-card p-4 space-y-4">
					<div class="flex justify-between items-center">
						<h3 class="text-base font-semibold">{agent.name}</h3>
						<span class="text-xs text-muted-foreground bg-muted px-2 py-0.5 rounded">{agent.backend}</span>
					</div>

					<dl class="space-y-2 text-sm">
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Model</dt>
							<dd>{agent.model ?? `${config.llm.default_provider} default`}</dd>
						</div>
					</dl>

					{#if agent.role}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">System prompt</dt>
							<dd class="text-xs bg-muted/50 rounded px-3 py-2 whitespace-pre-wrap max-h-32 overflow-y-auto font-mono">{agent.role}</dd>
						</div>
					{/if}

					{#if agent.tools.length > 0}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">Tools</dt>
							<dd class="flex flex-wrap gap-1.5">
								{#each agent.tools as tool}
									<span class="text-xs bg-muted px-2 py-0.5 rounded">{tool}</span>
								{/each}
							</dd>
						</div>
					{/if}

					{#if agent.volumes && agent.volumes.length > 0}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">Volumes</dt>
							<dd class="space-y-1">
								{#each agent.volumes as vol}
									<div class="text-xs font-mono bg-muted/50 px-2 py-1 rounded">{vol}</div>
								{/each}
							</dd>
						</div>
					{/if}

					{#if agent.budget}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">Budget <span class="text-emerald-400/70">(override)</span></dt>
							<dl class="space-y-1 text-sm pl-2">
								{#if agent.budget.daily}
									<div class="flex justify-between">
										<dt class="text-muted-foreground">Daily</dt>
										<dd>{agent.budget.daily}</dd>
									</div>
								{/if}
								{#if agent.budget.monthly}
									<div class="flex justify-between">
										<dt class="text-muted-foreground">Monthly</dt>
										<dd>{agent.budget.monthly}</dd>
									</div>
								{/if}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">On exceeded</dt>
									<dd>{agent.budget.on_exceeded}</dd>
								</div>
							</dl>
						</div>
					{/if}

					{#if agent.rate_limit}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">Rate limit <span class="text-emerald-400/70">(override)</span></dt>
							<dl class="space-y-1 text-sm pl-2">
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Requests/min</dt>
									<dd>{agent.rate_limit.requests_per_minute}</dd>
								</div>
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Tokens/min</dt>
									<dd>{agent.rate_limit.tokens_per_minute.toLocaleString()}</dd>
								</div>
							</dl>
						</div>
					{/if}

					{#if agent.wake_on && agent.wake_on.length > 0}
						<div>
							<dt class="text-xs text-muted-foreground mb-1">Wake-on triggers</dt>
							<dd class="flex flex-wrap gap-1.5">
								{#each agent.wake_on as trigger}
									{#if trigger.schedule}
										<span class="text-xs bg-muted px-2 py-0.5 rounded">{trigger.schedule}</span>
									{/if}
									{#if trigger.event}
										<span class="text-xs bg-muted px-2 py-0.5 rounded">{trigger.event}</span>
									{/if}
								{/each}
							</dd>
						</div>
					{/if}
				</div>
			{/each}
		</div>

		<!-- MCP Servers -->
		{#if config.mcp_servers.length > 0}
			<div class="rounded-lg border border-border bg-card p-4 space-y-3">
				<h2 class="text-sm font-semibold">Connectors (MCP)</h2>
				<p class="text-xs text-muted-foreground">Available MCP servers. Per-agent access is controlled via tools configuration.</p>
				<div class="flex flex-wrap gap-2">
					{#each config.mcp_servers as server}
						<span class="text-xs bg-muted px-2 py-1 rounded">{server}</span>
					{/each}
				</div>
			</div>
		{/if}
	{:else}
		<div class="rounded-lg border border-border bg-card p-4">
			<p class="text-sm text-muted-foreground">Loading configuration...</p>
		</div>
	{/if}
</div>
