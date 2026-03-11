<script lang="ts">
	import { onMount } from 'svelte';
	import { health } from '$lib/api';

	let serverInfo = $state<{ status: string; version: string } | null>(null);

	onMount(async () => {
		serverInfo = await health.check().catch(() => null);
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

	<!-- Configuration reference -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Configuration</h2>
		<p class="text-sm text-muted-foreground">
			Edit <code class="bg-muted px-1.5 py-0.5 rounded text-xs">xpressclaw.yaml</code> in your project directory to configure agents, budget limits, and tools.
		</p>
		<pre class="text-xs bg-muted/50 rounded-md p-3 overflow-x-auto text-muted-foreground"><code>system:
  budget:
    daily: $20.00
    on_exceeded: pause

agents:
  - name: atlas
    backend: claude-code
    role: Your executive assistant
    model: claude-sonnet-4-20250514

tools:
  builtin:
    filesystem: ~/workspace
    shell:
      enabled: true</code></pre>
	</div>
</div>
