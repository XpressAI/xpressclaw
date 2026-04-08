<script lang="ts">
	import { onMount } from 'svelte';
	import { connectors, agents } from '$lib/api';
	import type { Connector, Channel, Agent } from '$lib/api';

	let connectorList = $state<Connector[]>([]);
	let channelMap = $state<Record<string, Channel[]>>({});
	let agentList = $state<Agent[]>([]);
	let loading = $state(true);
	let expandedId = $state<string | null>(null);

	// Test results
	let testResults = $state<Record<string, { ok: boolean; error?: string; loading: boolean }>>({});

	// Add connector modal
	let showAddConnector = $state(false);
	let addStep = $state<'type' | 'config'>('type');
	let addForm = $state<{ name: string; connector_type: string; config: Record<string, unknown> }>({
		name: '', connector_type: '', config: {}
	});
	let addError = $state('');
	let addSaving = $state(false);

	// Add channel modal
	let showAddChannel = $state<string | null>(null);
	let channelForm = $state<{ name: string; channel_type: string; config: Record<string, unknown>; agent_id: string }>({
		name: '', channel_type: 'source', config: {}, agent_id: ''
	});
	let channelError = $state('');
	let channelSaving = $state(false);

	const connectorTypes = [
		{ id: 'webhook', label: 'Webhook', icon: 'M12 21a9.004 9.004 0 008.716-6.747M12 21a9.004 9.004 0 01-8.716-6.747M12 21c2.485 0 4.5-4.03 4.5-9S14.485 3 12 3m0 18c-2.485 0-4.5-4.03-4.5-9S9.515 3 12 3m0 0a8.997 8.997 0 017.843 4.582M12 3a8.997 8.997 0 00-7.843 4.582m15.686 0A11.953 11.953 0 0112 10.5c-2.998 0-5.74-1.1-7.843-2.918m15.686 0A8.959 8.959 0 0121 12c0 .778-.099 1.533-.284 2.253m0 0A17.919 17.919 0 0112 16.5c-3.162 0-6.133-.815-8.716-2.247m0 0A9.015 9.015 0 013 12c0-1.605.42-3.113 1.157-4.418' },
		{ id: 'telegram', label: 'Telegram', icon: 'M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5' },
		{ id: 'file_watcher', label: 'File Watcher', icon: 'M2.25 12.75V12A2.25 2.25 0 014.5 9.75h15A2.25 2.25 0 0121.75 12v.75m-8.69-6.44l-2.12-2.12a1.5 1.5 0 00-1.061-.44H4.5A2.25 2.25 0 002.25 6v12a2.25 2.25 0 002.25 2.25h15A2.25 2.25 0 0021.75 18V9a2.25 2.25 0 00-2.25-2.25h-5.379a1.5 1.5 0 01-1.06-.44z' },
		{ id: 'email', label: 'Email', icon: 'M21.75 6.75v10.5a2.25 2.25 0 01-2.25 2.25h-15a2.25 2.25 0 01-2.25-2.25V6.75m19.5 0A2.25 2.25 0 0019.5 4.5h-15a2.25 2.25 0 00-2.25 2.25m19.5 0v.243a2.25 2.25 0 01-1.07 1.916l-7.5 4.615a2.25 2.25 0 01-2.36 0L3.32 8.91a2.25 2.25 0 01-1.07-1.916V6.75' },
		{ id: 'github', label: 'GitHub', icon: 'M17.25 6.75L22.5 12l-5.25 5.25m-10.5 0L1.5 12l5.25-5.25m7.5-3l-4.5 16.5' },
		{ id: 'jira', label: 'Jira', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2' },
		{ id: 'slack', label: 'Slack', icon: 'M5.25 8.25h15m-16.5 7.5h15m-1.8-13.5l-3.9 19.5m-2.1-19.5l-3.9 19.5' },
	];

	function typeIcon(type: string): string {
		return connectorTypes.find(t => t.id === type)?.icon ?? 'M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m13.35-.622l1.757-1.757a4.5 4.5 0 00-6.364-6.364l-4.5 4.5a4.5 4.5 0 001.242 7.244';
	}

	function typeLabel(type: string): string {
		return connectorTypes.find(t => t.id === type)?.label ?? type;
	}

	function statusBadge(status: string): { text: string; classes: string } {
		switch (status) {
			case 'connected': return { text: 'Connected', classes: 'bg-emerald-500/10 text-emerald-400' };
			case 'error': return { text: 'Error', classes: 'bg-red-500/10 text-red-400' };
			default: return { text: 'Disconnected', classes: 'bg-muted text-muted-foreground' };
		}
	}

	function channelTypeBadge(type: string): { text: string; classes: string } {
		switch (type) {
			case 'source': return { text: 'Source', classes: 'bg-blue-500/10 text-blue-400' };
			case 'sink': return { text: 'Sink', classes: 'bg-amber-500/10 text-amber-400' };
			case 'both': return { text: 'Both', classes: 'bg-purple-500/10 text-purple-400' };
			default: return { text: type, classes: 'bg-muted text-muted-foreground' };
		}
	}

	function truncateJson(obj: Record<string, unknown>, maxLen = 60): string {
		const str = JSON.stringify(obj);
		if (str.length <= maxLen) return str;
		return str.slice(0, maxLen) + '...';
	}

	function agentName(id: string | null): string {
		if (!id) return 'Unbound';
		const agent = agentList.find(a => a.id === id);
		return agent?.config?.display_name || agent?.name || id;
	}

	onMount(() => load());

	async function load() {
		const [cl, al] = await Promise.all([
			connectors.list().catch(() => []),
			agents.list().catch(() => [])
		]);
		connectorList = cl;
		agentList = al;
		loading = false;
	}

	async function toggleExpand(id: string) {
		if (expandedId === id) {
			expandedId = null;
			return;
		}
		expandedId = id;
		if (!channelMap[id]) {
			channelMap[id] = await connectors.channels(id).catch(() => []);
		}
	}

	async function toggleEnabled(connector: Connector) {
		try {
			const updated = await connectors.update(connector.id, { enabled: !connector.enabled });
			const idx = connectorList.findIndex(c => c.id === connector.id);
			if (idx >= 0) connectorList[idx] = updated;
		} catch {}
	}

	async function testConnection(id: string) {
		testResults[id] = { ok: false, loading: true };
		try {
			const result = await connectors.test(id);
			testResults[id] = { ok: result.ok, error: result.error, loading: false };
		} catch (e) {
			testResults[id] = { ok: false, error: String(e), loading: false };
		}
	}

	let confirmDeleteConnector = $state<string | null>(null);
	let confirmDeleteChannel = $state<string | null>(null);

	async function deleteConnector(id: string) {
		try {
			await connectors.delete(id);
			if (expandedId === id) expandedId = null;
			delete channelMap[id];
			confirmDeleteConnector = null;
			await load();
		} catch {}
	}

	async function deleteChannel(connectorId: string, channelId: string) {
		try {
			await connectors.deleteChannel(connectorId, channelId);
			channelMap[connectorId] = await connectors.channels(connectorId).catch(() => []);
			confirmDeleteChannel = null;
		} catch {}
	}

	// --- Add Connector ---

	function openAddConnector() {
		addStep = 'type';
		addForm = { name: '', connector_type: '', config: {} };
		addError = '';
		showAddConnector = true;
	}

	function selectType(type: string) {
		addForm.connector_type = type;
		addForm.config = {};
		addStep = 'config';
	}

	async function saveConnector() {
		if (!addForm.name.trim()) { addError = 'Name is required'; return; }
		addSaving = true;
		addError = '';
		try {
			await connectors.create({
				name: addForm.name.trim(),
				connector_type: addForm.connector_type,
				config: addForm.config
			});
			showAddConnector = false;
			await load();
		} catch (e) {
			addError = String(e);
		}
		addSaving = false;
	}

	// --- Add Channel ---

	function openAddChannel(connectorId: string) {
		channelForm = { name: '', channel_type: 'source', config: {}, agent_id: '' };
		channelError = '';
		showAddChannel = connectorId;
	}

	async function saveChannel() {
		if (!showAddChannel) return;
		if (!channelForm.name.trim()) { channelError = 'Name is required'; return; }
		channelSaving = true;
		channelError = '';
		try {
			await connectors.createChannel(showAddChannel, {
				name: channelForm.name.trim(),
				channel_type: channelForm.channel_type || undefined,
				config: Object.keys(channelForm.config).length ? channelForm.config : undefined,
				agent_id: channelForm.agent_id || undefined
			});
			const cid = showAddChannel;
			showAddChannel = null;
			channelMap[cid] = await connectors.channels(cid).catch(() => []);
		} catch (e) {
			channelError = String(e);
		}
		channelSaving = false;
	}

	const comingSoonTypes = ['email', 'github', 'jira', 'slack'];
</script>

<div class="p-6 space-y-6">
	<!-- Header -->
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Connectors</h1>
			<p class="text-sm text-muted-foreground mt-1">
				{connectorList.length} connector{connectorList.length !== 1 ? 's' : ''} configured
			</p>
		</div>
		<button
			onclick={openAddConnector}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			Add Connector
		</button>
	</div>

	<!-- Loading -->
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if connectorList.length === 0}
		<!-- Empty state -->
		<div class="rounded-lg border border-border bg-card p-8 text-center space-y-3">
			<svg class="h-10 w-10 mx-auto text-muted-foreground/40" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m13.35-.622l1.757-1.757a4.5 4.5 0 00-6.364-6.364l-4.5 4.5a4.5 4.5 0 001.242 7.244" /></svg>
			<p class="text-sm text-muted-foreground">No connectors configured</p>
			<p class="text-xs text-muted-foreground/70">Add a webhook, Telegram bot, file watcher, or other integration to connect agents to the outside world.</p>
		</div>
	{:else}
		<!-- Connector list -->
		<div class="space-y-3">
			{#each connectorList as connector (connector.id)}
				{@const badge = statusBadge(connector.status)}
				{@const expanded = expandedId === connector.id}
				{@const test = testResults[connector.id]}
				<div class="rounded-lg border border-border bg-card transition-colors {expanded ? 'border-primary/50' : ''}">
					<!-- Connector row -->
					<div class="flex items-center gap-3 p-4">
						<!-- Type icon -->
						<div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-muted">
							<svg class="h-5 w-5 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d={typeIcon(connector.connector_type)} />
							</svg>
						</div>

						<!-- Name + type -->
						<div class="flex-1 min-w-0">
							<div class="text-sm font-semibold truncate">{connector.name}</div>
							<div class="text-xs text-muted-foreground">{typeLabel(connector.connector_type)}</div>
						</div>

						<!-- Status badge -->
						<span class="shrink-0 rounded-full px-2 py-0.5 text-[11px] font-medium {badge.classes}">
							{badge.text}
						</span>

						<!-- Error tooltip -->
						{#if connector.error_message}
							<span class="shrink-0 text-xs text-red-400 max-w-48 truncate" title={connector.error_message}>
								{connector.error_message}
							</span>
						{/if}

						<!-- Test button -->
						<button
							onclick={() => testConnection(connector.id)}
							disabled={test?.loading}
							class="shrink-0 rounded-md border border-border px-2.5 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-accent transition-colors disabled:opacity-50"
						>
							{#if test?.loading}
								Testing...
							{:else}
								Test
							{/if}
						</button>

						<!-- Test result indicator -->
						{#if test && !test.loading}
							{#if test.ok}
								<svg class="h-4 w-4 text-emerald-400 shrink-0" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
									<path stroke-linecap="round" stroke-linejoin="round" d="M4.5 12.75l6 6 9-13.5" />
								</svg>
							{:else}
								<span class="text-xs text-red-400 shrink-0" title={test.error}>Failed</span>
							{/if}
						{/if}

						<!-- Enabled toggle -->
						<label class="relative inline-flex items-center cursor-pointer shrink-0">
							<input
								type="checkbox"
								checked={connector.enabled}
								onchange={() => toggleEnabled(connector)}
								class="sr-only peer"
							/>
							<div class="w-8 h-[18px] bg-muted rounded-full peer peer-checked:bg-primary transition-colors after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-background after:rounded-full after:h-3.5 after:w-3.5 after:transition-all peer-checked:after:translate-x-full"></div>
						</label>

						<!-- Delete button -->
						{#if confirmDeleteConnector === connector.id}
							<span class="flex items-center gap-1 text-[10px] shrink-0">
								<button onclick={() => deleteConnector(connector.id)} class="text-destructive hover:underline">delete</button>
								<button onclick={() => (confirmDeleteConnector = null)} class="text-muted-foreground hover:underline">cancel</button>
							</span>
						{:else}
						<button
							onclick={() => (confirmDeleteConnector = connector.id)}
							class="shrink-0 rounded-md p-1.5 text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
							title="Delete connector"
						>
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M14.74 9l-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 01-2.244 2.077H8.084a2.25 2.25 0 01-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 00-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 013.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 00-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 00-7.5 0" />
							</svg>
						</button>
						{/if}

						<!-- Expand/collapse -->
						<button
							onclick={() => toggleExpand(connector.id)}
							class="shrink-0 rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
							title={expanded ? 'Collapse' : 'Show channels'}
						>
							<svg class="h-4 w-4 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" />
							</svg>
						</button>
					</div>

					<!-- Expanded: channels -->
					{#if expanded}
						{@const channels = channelMap[connector.id] ?? []}
						<div class="border-t border-border px-4 pb-4">
							{#if channels.length > 0}
								<table class="w-full text-sm mt-3">
									<thead>
										<tr class="text-xs text-muted-foreground border-b border-border/50">
											<th class="text-left font-medium pb-2 pr-4">Channel</th>
											<th class="text-left font-medium pb-2 pr-4">Type</th>
											<th class="text-left font-medium pb-2 pr-4">Agent</th>
											<th class="text-left font-medium pb-2 pr-4">Config</th>
											<th class="text-right font-medium pb-2"></th>
										</tr>
									</thead>
									<tbody>
										{#each channels as channel (channel.id)}
											{@const ctBadge = channelTypeBadge(channel.channel_type)}
											<tr class="border-b border-border/30 last:border-0">
												<td class="py-2.5 pr-4 font-medium">{channel.name}</td>
												<td class="py-2.5 pr-4">
													<span class="rounded-full px-2 py-0.5 text-[11px] font-medium {ctBadge.classes}">{ctBadge.text}</span>
												</td>
												<td class="py-2.5 pr-4">
													{#if channel.agent_id}
														<span class="text-foreground">{agentName(channel.agent_id)}</span>
													{:else}
														<span class="text-muted-foreground/60 italic">Unbound</span>
													{/if}
												</td>
												<td class="py-2.5 pr-4">
													<code class="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded font-mono">{truncateJson(channel.config)}</code>
												</td>
												<td class="py-2.5 text-right">
													{#if confirmDeleteChannel === channel.id}
													<span class="flex items-center gap-1 text-[10px]">
														<button onclick={() => deleteChannel(connector.id, channel.id)} class="text-destructive hover:underline">delete</button>
														<button onclick={() => (confirmDeleteChannel = null)} class="text-muted-foreground hover:underline">cancel</button>
													</span>
												{:else}
												<button
													onclick={() => (confirmDeleteChannel = channel.id)}
													class="rounded-md p-1 text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
													title="Delete channel"
												>
													<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
														<path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
													</svg>
													</button>
												{/if}
												</td>
											</tr>
										{/each}
									</tbody>
								</table>
							{:else}
								<p class="text-xs text-muted-foreground mt-3">No channels configured</p>
							{/if}

							<button
								onclick={() => openAddChannel(connector.id)}
								class="mt-3 rounded-md border border-dashed border-border px-3 py-1.5 text-xs text-muted-foreground hover:text-foreground hover:border-muted-foreground/50 transition-colors"
							>
								+ Add Channel
							</button>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<!-- Add Connector Modal -->
{#if showAddConnector}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm" onclick={(e) => { if (e.target === e.currentTarget) showAddConnector = false; }}>
		<div class="mx-4 w-full max-w-lg rounded-xl border border-border bg-card p-6 shadow-2xl" onclick={(e) => e.stopPropagation()}>
			{#if addStep === 'type'}
				<!-- Step 1: Type selection -->
				<h2 class="text-lg font-bold mb-1">Add Connector</h2>
				<p class="text-sm text-muted-foreground mb-5">Choose a connector type</p>
				<div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
					{#each connectorTypes as ct}
						<button
							onclick={() => selectType(ct.id)}
							class="flex flex-col items-center gap-2 rounded-lg border border-border p-4 hover:border-primary/50 hover:bg-accent/50 transition-colors text-center"
						>
							<svg class="h-6 w-6 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d={ct.icon} />
							</svg>
							<span class="text-xs font-medium">{ct.label}</span>
						</button>
					{/each}
				</div>
				<div class="flex justify-end mt-5">
					<button onclick={() => (showAddConnector = false)} class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent transition-colors">
						Cancel
					</button>
				</div>

			{:else}
				<!-- Step 2: Configuration -->
				<div class="flex items-center gap-2 mb-4">
					<button onclick={() => (addStep = 'type')} class="rounded-md p-1 text-muted-foreground hover:text-foreground hover:bg-accent transition-colors">
						<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
							<path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" />
						</svg>
					</button>
					<div>
						<h2 class="text-lg font-bold">Configure {typeLabel(addForm.connector_type)}</h2>
						<p class="text-sm text-muted-foreground">Set up the connector details</p>
					</div>
				</div>

				<div class="space-y-4">
					<!-- Name -->
					<div>
						<label for="conn-name" class="block text-xs font-medium text-muted-foreground mb-1.5">Name</label>
						<input
							id="conn-name"
							type="text"
							placeholder="e.g. my-webhook"
							bind:value={addForm.name}
							class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
						/>
					</div>

					<!-- Type-specific config -->
					{#if addForm.connector_type === 'webhook'}
						<div>
							<label for="webhook-url" class="block text-xs font-medium text-muted-foreground mb-1.5">Outgoing URL (optional)</label>
							<input
								id="webhook-url"
								type="url"
								placeholder="https://example.com/webhook"
								value={addForm.config.url ?? ''}
								oninput={(e) => { addForm.config = { ...addForm.config, url: (e.target as HTMLInputElement).value }; }}
								class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
							/>
							<p class="text-xs text-muted-foreground mt-1">Incoming webhooks receive a URL automatically after creation.</p>
						</div>
					{:else if addForm.connector_type === 'telegram'}
						<div>
							<label for="tg-token" class="block text-xs font-medium text-muted-foreground mb-1.5">Bot Token</label>
							<input
								id="tg-token"
								type="password"
								placeholder="123456:ABC-DEF..."
								value={addForm.config.bot_token ?? ''}
								oninput={(e) => { addForm.config = { ...addForm.config, bot_token: (e.target as HTMLInputElement).value }; }}
								class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring font-mono"
							/>
							<p class="text-xs text-muted-foreground mt-1">Get a token from <span class="text-primary">@BotFather</span> on Telegram.</p>
						</div>
					{:else if addForm.connector_type === 'file_watcher'}
						<div>
							<label for="fw-paths" class="block text-xs font-medium text-muted-foreground mb-1.5">Watch Paths</label>
							<input
								id="fw-paths"
								type="text"
								placeholder="/home/user/docs, /tmp/inbox"
								value={addForm.config.paths ?? ''}
								oninput={(e) => { addForm.config = { ...addForm.config, paths: (e.target as HTMLInputElement).value }; }}
								class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
							/>
							<p class="text-xs text-muted-foreground mt-1">Comma-separated list of directories to watch.</p>
						</div>
						<label class="flex items-center gap-2 text-sm">
							<input
								type="checkbox"
								checked={!!addForm.config.recursive}
								onchange={(e) => { addForm.config = { ...addForm.config, recursive: (e.target as HTMLInputElement).checked }; }}
								class="rounded border-input bg-background"
							/>
							<span class="text-muted-foreground">Watch subdirectories recursively</span>
						</label>
					{:else if comingSoonTypes.includes(addForm.connector_type)}
						<div class="rounded-lg border border-amber-800/40 bg-amber-950/20 p-4 text-center space-y-2">
							<p class="text-sm text-amber-300">Not yet implemented</p>
							<p class="text-xs text-muted-foreground">{typeLabel(addForm.connector_type)} connector is not functional. Messages will be logged but not actually sent or received. Use Webhook, Telegram, or File Watcher for real integrations.</p>
						</div>
					{/if}

					<!-- Error -->
					{#if addError}
						<p class="text-xs text-red-400">{addError}</p>
					{/if}

					<!-- Actions -->
					<div class="flex justify-end gap-2 pt-2">
						<button onclick={() => (showAddConnector = false)} class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent transition-colors">
							Cancel
						</button>
						<button
							onclick={saveConnector}
							disabled={addSaving || !addForm.name.trim()}
							class="rounded-md bg-primary px-4 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
						>
							{#if addSaving}Saving...{:else}Create Connector{/if}
						</button>
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}

<!-- Add Channel Modal -->
{#if showAddChannel}
	{@const parentConnector = connectorList.find(c => c.id === showAddChannel)}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm" onclick={(e) => { if (e.target === e.currentTarget) showAddChannel = null; }}>
		<div class="mx-4 w-full max-w-md rounded-xl border border-border bg-card p-6 shadow-2xl" onclick={(e) => e.stopPropagation()}>
			<h2 class="text-lg font-bold mb-1">Add Channel</h2>
			<p class="text-sm text-muted-foreground mb-5">
				{#if parentConnector}
					Add a channel to <span class="text-foreground font-medium">{parentConnector.name}</span>
				{:else}
					Configure a new channel
				{/if}
			</p>

			<div class="space-y-4">
				<!-- Name -->
				<div>
					<label for="ch-name" class="block text-xs font-medium text-muted-foreground mb-1.5">Channel Name</label>
					<input
						id="ch-name"
						type="text"
						placeholder="e.g. alerts, notifications"
						bind:value={channelForm.name}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
					/>
				</div>

				<!-- Type -->
				<div>
					<label for="ch-type" class="block text-xs font-medium text-muted-foreground mb-1.5">Channel Type</label>
					<select
						id="ch-type"
						bind:value={channelForm.channel_type}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						<option value="source">Source (inbound)</option>
						<option value="sink">Sink (outbound)</option>
						<option value="both">Both</option>
					</select>
				</div>

				<!-- Agent Binding -->
				<div>
					<label for="ch-agent" class="block text-xs font-medium text-muted-foreground mb-1.5">Bind to Agent</label>
					<select
						id="ch-agent"
						bind:value={channelForm.agent_id}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						<option value="">None</option>
						{#each agentList as agent}
							<option value={agent.id}>{agent.config?.display_name || agent.name}</option>
						{/each}
					</select>
				</div>

				<!-- Type-specific channel config -->
				{#if parentConnector?.connector_type === 'telegram'}
					<div>
						<label for="ch-chat-id" class="block text-xs font-medium text-muted-foreground mb-1.5">Chat ID</label>
						<input
							id="ch-chat-id"
							type="text"
							placeholder="-1001234567890"
							value={channelForm.config.chat_id ?? ''}
							oninput={(e) => { channelForm.config = { ...channelForm.config, chat_id: (e.target as HTMLInputElement).value }; }}
							class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
						/>
					</div>
				{:else if parentConnector?.connector_type === 'file_watcher'}
					<div>
						<label for="ch-pattern" class="block text-xs font-medium text-muted-foreground mb-1.5">Path or Glob Pattern</label>
						<input
							id="ch-pattern"
							type="text"
							placeholder="*.csv or /tmp/inbox/reports"
							value={channelForm.config.pattern ?? ''}
							oninput={(e) => { channelForm.config = { ...channelForm.config, pattern: (e.target as HTMLInputElement).value }; }}
							class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
						/>
					</div>
				{:else if parentConnector?.connector_type === 'webhook'}
					<div>
						<label for="ch-url" class="block text-xs font-medium text-muted-foreground mb-1.5">Outgoing URL</label>
						<input
							id="ch-url"
							type="url"
							placeholder="https://example.com/hook"
							value={channelForm.config.url ?? ''}
							oninput={(e) => { channelForm.config = { ...channelForm.config, url: (e.target as HTMLInputElement).value }; }}
							class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
						/>
					</div>
				{/if}

				<!-- Error -->
				{#if channelError}
					<p class="text-xs text-red-400">{channelError}</p>
				{/if}

				<!-- Actions -->
				<div class="flex justify-end gap-2 pt-2">
					<button onclick={() => (showAddChannel = null)} class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent transition-colors">
						Cancel
					</button>
					<button
						onclick={saveChannel}
						disabled={channelSaving || !channelForm.name.trim()}
						class="rounded-md bg-primary px-4 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
					>
						{#if channelSaving}Saving...{:else}Create Channel{/if}
					</button>
				</div>
			</div>
		</div>
	</div>
{/if}
