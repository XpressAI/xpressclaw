<script lang="ts">
	import { onMount } from 'svelte';
	import type { LiveConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: { tools?: string[] }) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();

	// Tool toggles
	let fetchEnabled = $state(false);
	let gitEnabled = $state(false);
	let githubEnabled = $state(false);
	let websearchEnabled = $state(false);

	// MCP servers
	interface McpServer {
		name: string;
		type: string;
		command?: string;
		args?: string[];
		url?: string;
		env?: Record<string, string>;
		headers?: Record<string, string>;
	}
	let mcpServers = $state<McpServer[]>([]);

	// Modal state
	let showConfigModal = $state(false);
	let configTarget = $state<'fetch' | 'git' | 'github' | 'websearch' | 'mcp' | null>(null);
	let configTitle = $state('');

	// Tool-specific config (stored as env vars or notes for now)
	let fetchAllowList = $state('*');
	let fetchBlockList = $state('');
	let gitSshKey = $state('');
	let githubToken = $state('');

	// MCP server editor
	let editingMcp = $state<McpServer | null>(null);
	let mcpName = $state('');
	let mcpType = $state<'stdio' | 'url'>('stdio');
	let mcpCommand = $state('');
	let mcpArgs = $state('');
	let mcpUrl = $state('');
	let mcpEnvPairs = $state<{key: string; value: string}[]>([]);
	let mcpSaving = $state(false);

	// MCP catalog
	let showCatalog = $state(false);
	let catalogQuery = $state('');
	let catalogResults = $state<{name: string; description: string; url?: string}[]>([]);
	let catalogLoading = $state(false);

	const toolConfigs: Record<string, {label: string; desc: string}> = {
		fetch: { label: 'Internet Access', desc: 'Configure allowed and blocked sites' },
		git: { label: 'Git', desc: 'Configure SSH key for repository access' },
		github: { label: 'GitHub', desc: 'Configure personal access token' },
		websearch: { label: 'Web Search', desc: 'Search the web via DuckDuckGo' },
	};

	$effect(() => {
		if (agentConfig) {
			fetchEnabled = agentConfig.tools.includes('fetch');
			gitEnabled = agentConfig.tools.includes('git');
			githubEnabled = agentConfig.tools.includes('github');
			websearchEnabled = agentConfig.tools.includes('websearch');
		}
	});

	onMount(loadMcpServers);

	async function loadMcpServers() {
		try {
			const resp = await fetch('/api/setup/mcp-servers');
			const data = await resp.json();
			mcpServers = data.servers || [];
		} catch {}
	}

	function getToggle(key: string): boolean {
		switch (key) {
			case 'fetch': return fetchEnabled;
			case 'git': return gitEnabled;
			case 'github': return githubEnabled;
			case 'websearch': return websearchEnabled;
			default: return false;
		}
	}

	function setToggle(key: string, val: boolean) {
		switch (key) {
			case 'fetch': fetchEnabled = val; break;
			case 'git': gitEnabled = val; break;
			case 'github': githubEnabled = val; break;
			case 'websearch': websearchEnabled = val; break;
		}
	}

	function openToolConfig(key: string) {
		configTarget = key as any;
		configTitle = toolConfigs[key]?.label ?? key;
		showConfigModal = true;
	}

	function openMcpConfig(server?: McpServer) {
		configTarget = 'mcp';
		if (server) {
			editingMcp = server;
			configTitle = `Configure ${server.name}`;
			mcpName = server.name;
			mcpType = server.url ? 'url' : 'stdio';
			mcpCommand = server.command ?? '';
			mcpArgs = (server.args ?? []).join(' ');
			mcpUrl = server.url ?? '';
			mcpEnvPairs = Object.entries(server.env ?? {}).map(([key, value]) => ({ key, value }));
		} else {
			editingMcp = null;
			configTitle = 'Add MCP Server';
			mcpName = '';
			mcpType = 'stdio';
			mcpCommand = '';
			mcpArgs = '';
			mcpUrl = '';
			mcpEnvPairs = [];
		}
		showConfigModal = true;
	}

	function addEnvPair() {
		mcpEnvPairs = [...mcpEnvPairs, { key: '', value: '' }];
	}

	function removeEnvPair(idx: number) {
		mcpEnvPairs = mcpEnvPairs.filter((_, i) => i !== idx);
	}

	async function saveMcpServer() {
		if (!mcpName.trim()) return;
		mcpSaving = true;
		const env: Record<string, string> = {};
		for (const pair of mcpEnvPairs) {
			if (pair.key.trim()) env[pair.key.trim()] = pair.value;
		}
		try {
			await fetch('/api/setup/mcp-servers', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					name: mcpName.trim(),
					type: mcpType === 'url' ? 'sse' : 'stdio',
					command: mcpType === 'stdio' ? mcpCommand.trim() || undefined : undefined,
					args: mcpType === 'stdio' ? mcpArgs.split(/\s+/).filter(Boolean) : [],
					url: mcpType === 'url' ? mcpUrl.trim() || undefined : undefined,
					env,
				}),
			});
			await loadMcpServers();
			showConfigModal = false;
		} catch (e) {
			alert(`Failed to save: ${e}`);
		}
		mcpSaving = false;
	}

	async function deleteMcpServer(name: string) {
		if (!confirm(`Remove MCP server "${name}"?`)) return;
		try {
			await fetch(`/api/setup/mcp-servers/${encodeURIComponent(name)}`, { method: 'DELETE' });
			await loadMcpServers();
		} catch (e) {
			alert(`Failed to delete: ${e}`);
		}
	}

	async function searchCatalog() {
		if (!catalogQuery.trim()) return;
		catalogLoading = true;
		try {
			const resp = await fetch(`https://registry.modelcontextprotocol.io/v0/servers?limit=20&q=${encodeURIComponent(catalogQuery)}`);
			const data = await resp.json();
			const seen = new Set<string>();
			catalogResults = (data.servers || [])
				.filter((s: any) => {
					const name = s.server?.name;
					const isLatest = s._meta?.['io.modelcontextprotocol.registry/official']?.isLatest;
					if (!name || seen.has(name) || !isLatest) return false;
					seen.add(name);
					return true;
				})
				.map((s: any) => ({
					name: s.server.name,
					description: s.server.description || '',
					url: s.server.remotes?.[0]?.url,
				}));
		} catch { catalogResults = []; }
		catalogLoading = false;
	}

	function addFromCatalog(server: typeof catalogResults[0]) {
		showCatalog = false;
		mcpName = server.name.replace(/\//g, '-');
		if (server.url) {
			mcpType = 'url';
			mcpUrl = server.url;
		} else {
			mcpType = 'stdio';
		}
		mcpCommand = '';
		mcpArgs = '';
		mcpEnvPairs = [];
		editingMcp = null;
		configTarget = 'mcp';
		configTitle = 'Add MCP Server';
		showConfigModal = true;
	}

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			handleSave();
		}
	});

	function handleSave() {
		const tools = ['filesystem', 'shell', 'memory'];
		if (fetchEnabled) tools.push('fetch');
		if (gitEnabled) tools.push('git');
		if (githubEnabled) tools.push('github');
		if (websearchEnabled) tools.push('websearch');
		onSave({ tools });
	}

	const builtinMcpNames = ['shell', 'filesystem', 'xpressclaw'];
</script>

<div class="space-y-6">
	<!-- Always-on tools -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Built-in Tools</h2>
		<div class="flex flex-wrap gap-1.5">
			{#each ['Filesystem', 'Shell', 'Memory'] as tool}
				<span class="inline-flex items-center rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">{tool}</span>
			{/each}
		</div>
	</div>

	<!-- Optional tools -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Optional Tools</h2>
		<div class="space-y-2">
			{#each ['fetch', 'git', 'github', 'websearch'] as key}
				{@const checked = getToggle(key)}
				{@const cfg = toolConfigs[key]}
				<div class="flex items-center gap-3 rounded-md border border-border p-3 {checked ? 'border-primary/30 bg-primary/5' : ''}">
					<label class="flex items-center gap-3 cursor-pointer flex-1">
						<input type="checkbox" checked={checked} onchange={() => setToggle(key, !checked)} class="rounded border-border" />
						<div>
							<span class="text-sm font-medium text-foreground">{cfg.label}</span>
							<span class="text-xs text-muted-foreground ml-1">{cfg.desc}</span>
						</div>
					</label>
					<button onclick={() => openToolConfig(key)}
						class="rounded-md border border-border px-2.5 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground transition-colors">
						Configure
					</button>
				</div>
			{/each}
		</div>
	</div>

	<!-- MCP Servers -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<div class="flex items-center justify-between">
			<div>
				<h2 class="text-sm font-semibold">MCP Servers</h2>
				<p class="text-xs text-muted-foreground">Additional tools via the Model Context Protocol.</p>
			</div>
			<div class="flex gap-2">
				<button onclick={() => { showCatalog = true; catalogQuery = ''; catalogResults = []; }}
					class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent transition-colors">
					Browse Catalog
				</button>
				<button onclick={() => openMcpConfig()}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 transition-colors">
					Add Server
				</button>
			</div>
		</div>

		{#if mcpServers.length > 0}
			<div class="space-y-1">
				{#each mcpServers as server}
					<div class="flex items-center gap-2 rounded-md border border-border px-3 py-2">
						<span class="w-2 h-2 rounded-full bg-emerald-500 shrink-0"></span>
						<span class="text-sm font-mono flex-1 truncate">{server.name}</span>
						<span class="text-xs text-muted-foreground">{server.type}</span>
						{#if !builtinMcpNames.includes(server.name)}
							<button onclick={() => openMcpConfig(server)}
								class="rounded px-2 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground">
								Configure
							</button>
							<button onclick={() => deleteMcpServer(server.name)}
								class="rounded px-2 py-0.5 text-xs text-destructive hover:bg-destructive/10">
								Remove
							</button>
						{/if}
					</div>
				{/each}
			</div>
		{:else}
			<p class="text-xs text-muted-foreground italic">No additional MCP servers configured.</p>
		{/if}
	</div>
</div>

<!-- Tool / MCP Configure Modal -->
{#if showConfigModal}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onclick={() => showConfigModal = false}>
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="rounded-lg border border-border bg-card p-6 space-y-4 max-w-lg mx-4 w-full max-h-[80vh] overflow-y-auto" onclick={(e) => e.stopPropagation()}>
			<h2 class="text-lg font-semibold">{configTitle}</h2>

			{#if configTarget === 'fetch'}
				<div class="space-y-3">
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Allowed Sites</label>
						<input type="text" bind:value={fetchAllowList} placeholder="* (all sites)"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
						<p class="text-xs text-muted-foreground mt-1">Comma-separated domains, or * for all. e.g. github.com, docs.python.org</p>
					</div>
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Blocked Sites</label>
						<input type="text" bind:value={fetchBlockList} placeholder="none"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
						<p class="text-xs text-muted-foreground mt-1">Comma-separated domains to block. Takes precedence over allow list.</p>
					</div>
				</div>

			{:else if configTarget === 'git'}
				<div class="space-y-3">
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">SSH Private Key</label>
						<textarea bind:value={gitSshKey} rows="6" placeholder="-----BEGIN OPENSSH PRIVATE KEY-----&#10;..."
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"></textarea>
						<p class="text-xs text-muted-foreground mt-1">Private key for SSH-based git operations. Stored securely in the agent's container.</p>
					</div>
				</div>

			{:else if configTarget === 'github'}
				<div class="space-y-3">
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Personal Access Token</label>
						<input type="password" bind:value={githubToken} placeholder="ghp_..."
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
						<p class="text-xs text-muted-foreground mt-1">GitHub PAT with repo, issues, and pull request permissions.</p>
					</div>
				</div>

			{:else if configTarget === 'websearch'}
				<div>
					<p class="text-sm text-muted-foreground">Web Search uses DuckDuckGo and requires no configuration.</p>
				</div>

			{:else if configTarget === 'mcp'}
				<div class="space-y-3">
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Server Name</label>
						<input type="text" bind:value={mcpName} placeholder="my-server" disabled={!!editingMcp}
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring disabled:opacity-50" />
					</div>
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Type</label>
						<select bind:value={mcpType}
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring">
							<option value="stdio">Local (Command)</option>
							<option value="url">Remote (URL)</option>
						</select>
					</div>
					{#if mcpType === 'url'}
						<div>
							<label class="block text-xs font-medium text-muted-foreground mb-1">URL</label>
							<input type="text" bind:value={mcpUrl} placeholder="https://example.com/mcp"
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
					{:else}
						<div>
							<label class="block text-xs font-medium text-muted-foreground mb-1">Command</label>
							<input type="text" bind:value={mcpCommand} placeholder="uvx, npx, python3, etc."
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
						<div>
							<label class="block text-xs font-medium text-muted-foreground mb-1">Arguments</label>
							<input type="text" bind:value={mcpArgs} placeholder="mcp-atlassian"
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
							<p class="text-xs text-muted-foreground mt-1">Space-separated arguments</p>
						</div>
					{/if}

					<!-- Environment Variables -->
					<div>
						<div class="flex items-center justify-between mb-1">
							<label class="block text-xs font-medium text-muted-foreground">Environment Variables</label>
							<button onclick={addEnvPair} class="text-xs text-primary hover:underline">+ Add</button>
						</div>
						{#if mcpEnvPairs.length > 0}
							<div class="space-y-1.5">
								{#each mcpEnvPairs as pair, i}
									<div class="flex gap-1.5">
										<input type="text" bind:value={pair.key} placeholder="KEY"
											class="w-1/3 rounded-md border border-border bg-background px-2 py-1.5 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
										<input type="password" bind:value={pair.value} placeholder="value"
											class="flex-1 rounded-md border border-border bg-background px-2 py-1.5 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
										<button onclick={() => removeEnvPair(i)}
											class="rounded px-1.5 text-xs text-destructive hover:bg-destructive/10">&#x2715;</button>
									</div>
								{/each}
							</div>
						{:else}
							<p class="text-xs text-muted-foreground italic">No environment variables. Click "+ Add" for API keys, tokens, etc.</p>
						{/if}
					</div>
				</div>
			{/if}

			<div class="flex justify-end gap-2 pt-2">
				<button onclick={() => showConfigModal = false}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Close</button>
				{#if configTarget === 'mcp'}
					<button onclick={saveMcpServer} disabled={mcpSaving || !mcpName.trim()}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
						{mcpSaving ? 'Saving...' : editingMcp ? 'Update' : 'Add Server'}
					</button>
				{/if}
			</div>
		</div>
	</div>
{/if}

<!-- MCP Catalog Modal -->
{#if showCatalog}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onclick={() => showCatalog = false}>
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="rounded-lg border border-border bg-card p-6 space-y-4 max-w-lg mx-4 w-full max-h-[80vh] flex flex-col" onclick={(e) => e.stopPropagation()}>
			<div>
				<h2 class="text-lg font-semibold">MCP Server Catalog</h2>
				<p class="text-xs text-muted-foreground">Browse the Model Context Protocol registry</p>
			</div>
			<div class="flex gap-2">
				<input type="text" bind:value={catalogQuery} placeholder="Search servers... (e.g. github, jira, slack)"
					onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') searchCatalog(); }}
					class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<button onclick={searchCatalog} disabled={catalogLoading}
					class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
					{catalogLoading ? '...' : 'Search'}
				</button>
			</div>
			<div class="flex-1 overflow-y-auto space-y-2 min-h-0">
				{#if catalogResults.length > 0}
					{#each catalogResults as server}
						<div class="rounded-md border border-border p-3 space-y-1">
							<div class="flex items-start justify-between gap-2">
								<div class="flex-1 min-w-0">
									<span class="text-sm font-medium">{server.name}</span>
								</div>
								<button onclick={() => addFromCatalog(server)}
									class="rounded-md border border-border px-2.5 py-1 text-xs hover:bg-accent shrink-0">Add</button>
							</div>
							<p class="text-xs text-muted-foreground line-clamp-2">{server.description}</p>
						</div>
					{/each}
				{:else if !catalogLoading}
					<p class="text-sm text-muted-foreground text-center py-4">Search for MCP servers (e.g. "atlassian", "github", "slack")</p>
				{/if}
			</div>
			<div class="flex justify-end">
				<button onclick={() => showCatalog = false}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Close</button>
			</div>
		</div>
	</div>
{/if}
