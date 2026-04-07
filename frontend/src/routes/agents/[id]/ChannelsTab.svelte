<script lang="ts">
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { connectors } from '$lib/api';
	import type { Connector, Channel } from '$lib/api';

	let allConnectors = $state<Connector[]>([]);
	let boundChannels = $state<{ channel: Channel; connector: Connector }[]>([]);
	let availableChannels = $state<{ channel: Channel; connector: Connector }[]>([]);
	let loading = $state(true);

	let agentId = $derived($page.params.id);

	onMount(() => { loadChannels(); });

	async function loadChannels() {
		loading = true;
		try {
			const conns = await connectors.list();
			allConnectors = conns;

			const bound: typeof boundChannels = [];
			const available: typeof availableChannels = [];

			for (const conn of conns) {
				if (!conn.enabled) continue;
				const channels = await connectors.channels(conn.id);
				for (const ch of channels) {
					if (ch.agent_id === agentId) {
						bound.push({ channel: ch, connector: conn });
					} else if (!ch.agent_id) {
						available.push({ channel: ch, connector: conn });
					}
				}
			}

			boundChannels = bound;
			availableChannels = available;
		} catch (e) {
			console.error('Failed to load channels:', e);
		}
		loading = false;
	}

	async function unbind(connectorId: string, channelId: string) {
		try {
			await connectors.updateChannel(connectorId, channelId, { agent_id: null });
			await loadChannels();
		} catch (e) {
			console.error('Failed to unbind:', e);
		}
	}

	async function bind(connectorId: string, channelId: string) {
		try {
			await connectors.updateChannel(connectorId, channelId, { agent_id: agentId });
			await loadChannels();
		} catch (e) {
			console.error('Failed to bind:', e);
		}
	}

	function typeIcon(type: string): string {
		switch (type) {
			case 'telegram': return '✈';
			case 'email': return '✉';
			case 'slack': return '#';
			case 'webhook': return '🌐';
			case 'file_watcher': return '📁';
			case 'github': return '⌨';
			case 'jira': return '📋';
			default: return '🔌';
		}
	}
</script>

<div class="space-y-6">
	{#if loading}
		<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">Loading channels...</div>
	{:else}
		<!-- Bound channels -->
		<div>
			<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-3">Connected Channels</h3>
			{#if boundChannels.length === 0}
				<div class="rounded-lg border border-border bg-muted/30 p-4 text-sm text-muted-foreground text-center">
					No channels connected to this agent. Bind a channel below to start receiving messages.
				</div>
			{:else}
				<div class="space-y-2">
					{#each boundChannels as { channel, connector }}
						<div class="flex items-center justify-between rounded-lg border border-border bg-card p-3">
							<div class="flex items-center gap-3">
								<span class="text-lg">{typeIcon(connector.connector_type)}</span>
								<div>
									<div class="text-sm font-medium">{channel.name}</div>
									<div class="text-xs text-muted-foreground">{connector.name} ({connector.connector_type})</div>
								</div>
							</div>
							<div class="flex items-center gap-2">
								<span class="text-[10px] rounded-full border border-emerald-500/30 bg-emerald-500/10 text-emerald-400 px-2 py-0.5">Connected</span>
								<button
									onclick={() => unbind(connector.id, channel.id)}
									class="rounded-md border border-border px-2.5 py-1 text-xs text-muted-foreground hover:text-destructive hover:border-destructive/50 transition-colors"
								>
									Disconnect
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<!-- Available channels to bind -->
		{#if availableChannels.length > 0}
			<div>
				<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-3">Available Channels</h3>
				<p class="text-xs text-muted-foreground mb-2">Connect a channel to route its messages directly to this agent.</p>
				<div class="space-y-2">
					{#each availableChannels as { channel, connector }}
						<div class="flex items-center justify-between rounded-lg border border-border/50 bg-background p-3">
							<div class="flex items-center gap-3">
								<span class="text-lg opacity-50">{typeIcon(connector.connector_type)}</span>
								<div>
									<div class="text-sm text-muted-foreground">{channel.name}</div>
									<div class="text-xs text-muted-foreground/70">{connector.name} ({connector.connector_type})</div>
								</div>
							</div>
							<button
								onclick={() => bind(connector.id, channel.id)}
								class="rounded-md bg-primary px-2.5 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
							>
								Connect
							</button>
						</div>
					{/each}
				</div>
			</div>
		{/if}

		<!-- Help text -->
		{#if allConnectors.length === 0}
			<div class="rounded-lg border border-border bg-muted/30 p-4 text-center space-y-2">
				<p class="text-sm text-muted-foreground">No connectors configured yet.</p>
				<a href="/settings/connectors" class="text-xs text-primary hover:underline">
					Set up connectors in Settings
				</a>
			</div>
		{/if}
	{/if}
</div>
