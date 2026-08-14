<script lang="ts">
	import { onMount } from 'svelte';
	import { health, setup } from '$lib/api';
	import type { DockerStatus, LiveConfig } from '$lib/api';

	let serverInfo = $state<{ status: string; version: string; build: string } | null>(null);
	let config = $state<LiveConfig | null>(null);
	let runtime = $state<DockerStatus | null>(null);
	let clientAddress = $state('');
	const tunnelCommand = 'ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host';

	onMount(async () => {
		clientAddress = window.location.origin;
		[serverInfo, config, runtime] = await Promise.all([
			health.check().catch(() => null),
			setup.getConfig().catch(() => null),
			setup.checkDocker().catch(() => null)
		]);
	});
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Instance</h1>
		<p class="text-sm text-muted-foreground mt-1">The control plane that owns this Project data and keeps Agents running</p>
	</div>

	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">This instance</h2>
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
				<dt class="text-muted-foreground">Build</dt>
				<dd>{serverInfo?.build ?? '—'}</dd>
			</div>
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Client address</dt>
				<dd class="text-xs font-mono">{clientAddress || '—'}</dd>
			</div>
			{#if config}
				<div class="flex items-start justify-between gap-4">
					<dt class="text-muted-foreground">Configuration</dt>
					<dd class="break-all text-right font-mono text-xs">{config.instance.config_path}</dd>
				</div>
				<div class="flex items-start justify-between gap-4">
					<dt class="text-muted-foreground">Local data</dt>
					<dd class="break-all text-right font-mono text-xs">{config.instance.data_dir}</dd>
				</div>
				<div class="flex items-start justify-between gap-4">
					<dt class="text-muted-foreground">Managed workspaces</dt>
					<dd class="break-all text-right font-mono text-xs">{config.instance.workspace_dir}</dd>
				</div>
			{/if}
			<div class="flex justify-between">
				<dt class="text-muted-foreground">Container runtime</dt>
				<dd class="capitalize">{runtime?.runtime ?? (runtime?.available ? 'Available' : 'Unavailable')}</dd>
			</div>
			{#if runtime?.version}
				<div class="flex justify-between">
					<dt class="text-muted-foreground">Runtime version</dt>
					<dd>{runtime.version}{runtime.rootless ? ' · rootless' : ''}</dd>
				</div>
			{/if}
			{#if runtime?.socket}
				<div class="flex items-start justify-between gap-4">
					<dt class="text-muted-foreground">Endpoint</dt>
					<dd class="break-all text-right font-mono text-xs">{runtime.socket}</dd>
				</div>
			{/if}
		</dl>
	</div>

	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<div>
			<h2 class="text-sm font-semibold">Remote access</h2>
			<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
				This page is a client of the control plane. Closing it or losing the network does not stop queued work; reconnect to the same instance to continue.
			</p>
		</div>
		<p class="text-xs leading-relaxed text-muted-foreground">
			XpressClaw has no built-in remote authentication yet. Keep its default loopback address and connect from another device through an SSH tunnel or an authenticated HTTPS proxy. Do not expose its port directly to a LAN or the internet.
		</p>
		<div class="rounded-md bg-muted px-3 py-2 font-mono text-[11px] text-foreground break-all">{tunnelCommand}</div>
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
				<p class="text-xs text-muted-foreground">Available for attachment to ACP harnesses.</p>
				<div class="flex flex-wrap gap-2">
					{#each config.mcp_servers as server}
						<span class="text-xs bg-muted px-2 py-1 rounded">{server.name} · {server.type}</span>
					{/each}
				</div>
			</div>
		{/if}
	{/if}
</div>
