<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { setup, agents as agentsApi } from '$lib/api';
	import type {
		DockerStatus,
		SystemInfo,
		OllamaInfo,
		ModelRecommendation,
		AgentPreset,
		DownloadStatus
	} from '$lib/api';

	// New flow: 0=agent, 1=llm, 2=connectors, 3=docker, 4=complete
	let step = $state(0);
	const steps = ['Agent', 'LLM', 'Connectors', 'Environment', 'Complete'];

	// Mode: 'setup' (full onboarding) or 'add-agent' (from agents page)
	let mode = $derived($page.url.searchParams.get('mode') === 'add-agent' ? 'add-agent' : 'setup');

	// -- Step 0: Agent --
	let presets = $state<AgentPreset[]>([]);
	let agentName = $state('');
	let selectedPreset = $state<AgentPreset | null>(null);
	let customRole = $state('');

	// -- Step 1: LLM --
	let systemInfo = $state<SystemInfo | null>(null);
	let ollamaInfo = $state<OllamaInfo | null>(null);
	let modelRec = $state<ModelRecommendation | null>(null);
	let llmProvider = $state('local');
	let llmApiKey = $state('');
	let llmBaseUrl = $state('');
	let llmLocalModel = $state('');
	let llmLocalBaseUrl = $state('');
	let keyValidating = $state(false);
	let keyValid = $state<boolean | null>(null);
	let keyError = $state('');
	let llmLoading = $state(false);

	// -- Step 2: Connectors --
	let mcpServers = $state<Record<string, { type: string; command?: string; args?: string[]; env?: Record<string, string>; url?: string }>>({});
	let showAddMcp = $state(false);
	let newMcpName = $state('');
	let newMcpCommand = $state('');
	let newMcpArgs = $state('');

	const mcpPresets = [
		{ name: 'Shell', id: 'shell', command: 'npx', args: '-y @mako10k/mcp-shell-server', envKey: '' },
		{ name: 'Filesystem', id: 'filesystem', command: 'npx', args: '-y @modelcontextprotocol/server-filesystem /workspace', envKey: '' },
		{ name: 'Git', id: 'git', command: 'npx', args: '-y @modelcontextprotocol/server-git', envKey: '' },
		{ name: 'GitHub', id: 'github', command: 'npx', args: '-y @modelcontextprotocol/server-github', envKey: 'GITHUB_PERSONAL_ACCESS_TOKEN' },
		{ name: 'Brave Search', id: 'brave-search', command: 'npx', args: '-y @modelcontextprotocol/server-brave-search', envKey: 'BRAVE_API_KEY' },
		{ name: 'Slack', id: 'slack', command: 'npx', args: '-y @modelcontextprotocol/server-slack', envKey: 'SLACK_BOT_TOKEN' },
		{ name: 'Fetch', id: 'fetch', command: 'npx', args: '-y @modelcontextprotocol/server-fetch', envKey: '' },
	];

	// -- Step 3: Docker --
	let dockerStatus = $state<DockerStatus | null>(null);
	let dockerLoading = $state(true);
	let containerless = $state(false);

	// -- Step 4: Complete --
	let saving = $state(false);
	let saveError = $state('');
	let downloading = $state(false);
	let downloadProgress = $state<DownloadStatus | null>(null);
	let downloadPollTimer: ReturnType<typeof setInterval> | null = null;
	let startingAgents = $state(false);

	const presetIcons: Record<string, string> = {
		brain: '&#x1f9e0;',
		code: '&#x1f4bb;',
		search: '&#x1f50d;',
		calendar: '&#x1f4c5;'
	};

	onMount(async () => {
		// Load presets immediately (first step)
		try { presets = await setup.presets(); } catch {}

		// Check Docker in background
		try { dockerStatus = await setup.checkDocker(); } catch {
			dockerStatus = { available: false, error: 'Failed to check' };
		}
		dockerLoading = false;

		// If Docker is available, auto-enable it
		if (dockerStatus?.available) containerless = false;
	});

	function selectPreset(preset: AgentPreset) {
		selectedPreset = preset;
		if (!agentName) agentName = preset.id;
		customRole = preset.role;

		// Pre-fill MCP servers from preset
		if (preset.default_mcp_servers && Object.keys(preset.default_mcp_servers).length > 0) {
			mcpServers = { ...preset.default_mcp_servers };
		}

		// Pre-fill LLM from preset recommendation
		if (preset.recommended_llm === 'local') {
			llmProvider = 'local';
		}
	}

	async function loadLlmInfo() {
		if (systemInfo) return; // already loaded
		llmLoading = true;
		try {
			const [sys, ollama, rec] = await Promise.all([
				setup.systemInfo(),
				setup.checkOllama(),
				setup.recommendModel()
			]);
			systemInfo = sys;
			ollamaInfo = ollama;
			modelRec = rec;
			if (rec?.model && !llmLocalModel) llmLocalModel = rec.model;
		} catch {}
		llmLoading = false;
	}

	async function validateApiKey() {
		if (!llmApiKey.trim()) return;
		keyValidating = true;
		keyValid = null;
		keyError = '';
		try {
			const result = await setup.validateKey(llmProvider, llmApiKey,
				llmProvider === 'openai' && llmBaseUrl ? llmBaseUrl : undefined);
			keyValid = result.valid;
			if (!result.valid) keyError = result.error || 'Invalid API key';
		} catch (e) {
			keyValid = false;
			keyError = e instanceof Error ? e.message : 'Validation failed';
		}
		keyValidating = false;
	}

	function addMcpPreset(preset: typeof mcpPresets[0]) {
		mcpServers[preset.id] = {
			type: 'stdio', command: preset.command,
			args: preset.args.split(' '),
			env: preset.envKey ? { [preset.envKey]: '' } : {}
		};
		mcpServers = { ...mcpServers };
	}

	function removeMcpServer(id: string) {
		const next = { ...mcpServers };
		delete next[id];
		mcpServers = next;
	}

	function addCustomMcp() {
		if (!newMcpName.trim()) return;
		mcpServers[newMcpName] = {
			type: 'stdio', command: newMcpCommand || undefined,
			args: newMcpArgs ? newMcpArgs.split(' ') : undefined
		};
		mcpServers = { ...mcpServers };
		newMcpName = ''; newMcpCommand = ''; newMcpArgs = '';
		showAddMcp = false;
	}

	async function goToStep(target: number) {
		if (target === 1) await loadLlmInfo();
		step = target;
	}

	function canProceedLlm(): boolean {
		if (llmProvider === 'local' || llmProvider === 'ollama') return !!llmLocalModel;
		if (llmProvider === 'openai' || llmProvider === 'anthropic') return !!llmApiKey && keyValid === true;
		return false;
	}

	function formatBytes(bytes: number): string {
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return (bytes / Math.pow(1024, i)).toFixed(1) + ' ' + units[i];
	}

	function startDownloadPolling() {
		downloading = true;
		step = 4;
		downloadPollTimer = setInterval(async () => {
			try {
				downloadProgress = await setup.downloadStatus();
				if (downloadProgress.status === 'Complete') {
					stopDownloadPolling(); downloading = false; autoStartAgents();
				} else if (downloadProgress.status === 'Error') {
					stopDownloadPolling(); downloading = false;
					saveError = downloadProgress.error || 'Download failed';
				}
			} catch {}
		}, 500);
	}

	function stopDownloadPolling() {
		if (downloadPollTimer) { clearInterval(downloadPollTimer); downloadPollTimer = null; }
	}

	async function autoStartAgents() {
		try {
			startingAgents = true;
			const allAgents = await agentsApi.list();
			for (const agent of allAgents) {
				if (agent.status !== 'running') await agentsApi.start(agent.id).catch(() => {});
			}
		} catch {}
		startingAgents = false;
	}

	async function completeSetup() {
		saving = true;
		saveError = '';
		try {
			const isLocal = llmProvider === 'local' || llmProvider === 'ollama';
			const useEmbedded = isLocal && (!ollamaInfo?.available || !llmLocalBaseUrl);

			const result = await setup.complete({
				llm: {
					provider: llmProvider,
					api_key: (llmProvider === 'openai' || llmProvider === 'anthropic') ? llmApiKey : undefined,
					base_url: llmProvider === 'openai' && llmBaseUrl ? llmBaseUrl : undefined,
					local_model: isLocal ? llmLocalModel : undefined,
					local_base_url: isLocal && llmLocalBaseUrl ? llmLocalBaseUrl : undefined,
					use_embedded: useEmbedded
				},
				agents: [{
					name: agentName,
					preset: selectedPreset?.id,
					role: customRole || undefined,
				}],
				mcp_servers: Object.keys(mcpServers).length > 0 ? mcpServers : undefined,
				isolation: containerless ? 'none' : 'docker'
			});

			if (result.downloading) {
				startDownloadPolling();
			} else {
				step = 4;
				autoStartAgents();
			}
		} catch (e) {
			saveError = e instanceof Error ? e.message : 'Failed to save configuration';
		}
		saving = false;
	}
</script>

<!-- Step indicator -->
<div class="mb-6 flex justify-center gap-2">
	{#each steps as s, i}
		<div class="flex items-center gap-2">
			<div
				class="flex h-8 w-8 items-center justify-center rounded-full text-xs font-medium transition-colors {i === step
					? 'bg-primary text-primary-foreground'
					: i < step
						? 'bg-primary/20 text-primary'
						: 'bg-muted text-muted-foreground'}"
			>
				{#if i < step}&#10003;{:else}{i + 1}{/if}
			</div>
			{#if i < steps.length - 1}
				<div class="h-px w-6 {i < step ? 'bg-primary/40' : 'bg-border'}"></div>
			{/if}
		</div>
	{/each}
</div>

<div class="rounded-xl border border-border bg-card p-6">
	<!-- Step 0: Agent Preset -->
	{#if step === 0}
		<h2 class="text-lg font-semibold text-foreground mb-1">
			{mode === 'add-agent' ? 'Add Agent' : 'Choose Your Agent'}
		</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Pick a template to get started. You can customize everything in the next steps.
		</p>

		<div class="grid grid-cols-2 gap-3 mb-6">
			{#each presets as preset}
				<button
					onclick={() => selectPreset(preset)}
					class="flex items-start gap-3 rounded-lg border p-4 text-left transition-colors {selectedPreset?.id === preset.id
						? 'border-primary bg-primary/5'
						: 'border-border hover:border-primary/40'}"
				>
					<span class="text-2xl">{@html presetIcons[preset.icon] || '&#x2699;'}</span>
					<div>
						<div class="text-sm font-medium text-foreground">{preset.name}</div>
						<div class="text-xs text-muted-foreground">{preset.description}</div>
						{#if preset.default_tools.length > 0}
							<div class="mt-1 flex flex-wrap gap-1">
								{#each preset.default_tools as tool}
									<span class="text-xs bg-muted px-1.5 py-0.5 rounded">{tool}</span>
								{/each}
							</div>
						{/if}
					</div>
				</button>
			{/each}
		</div>

		{#if selectedPreset}
			<div class="space-y-3 rounded-lg border border-border p-4">
				<div>
					<label for="agent-name" class="block text-xs font-medium text-foreground mb-1">Agent Name</label>
					<input
						id="agent-name"
						type="text"
						bind:value={agentName}
						placeholder="atlas"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label for="agent-role" class="block text-xs font-medium text-foreground mb-1">
						System Prompt <span class="text-muted-foreground font-normal">(customizable)</span>
					</label>
					<textarea
						id="agent-role"
						bind:value={customRole}
						rows="4"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"
					></textarea>
				</div>
			</div>
		{/if}

		<div class="mt-6 flex justify-end">
			<button
				onclick={() => goToStep(1)}
				disabled={!selectedPreset || !agentName.trim()}
				class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
			>Continue</button>
		</div>

	<!-- Step 1: LLM Provider -->
	{:else if step === 1}
		<h2 class="text-lg font-semibold text-foreground mb-1">LLM Provider</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Choose how your agents will think. You can change this later.
		</p>

		{#if llmLoading}
			<div class="flex items-center gap-3 p-4">
				<div class="h-5 w-5 animate-spin rounded-full border-2 border-primary border-t-transparent"></div>
				<span class="text-sm text-muted-foreground">Detecting hardware...</span>
			</div>
		{:else}
			{#if systemInfo}
				<div class="mb-4 rounded-lg border border-border p-3 text-xs text-muted-foreground">
					<span class="font-medium text-foreground">{systemInfo.os} {systemInfo.arch}</span>
					&mdash; {systemInfo.total_memory_gb.toFixed(0)}GB RAM, {systemInfo.cpu_count} CPUs
					{#if systemInfo.gpu.available}, {systemInfo.gpu.name}{/if}
				</div>
			{/if}

			<div class="space-y-2 mb-4">
				<button
					onclick={() => { llmProvider = 'local'; keyValid = null; }}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'local' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">&#x1F4BB;</div>
					<div class="flex-1">
						<div class="text-sm font-medium text-foreground">Local</div>
						<div class="text-xs text-muted-foreground">
							Runs a model directly inside xpressclaw. Free and private.
							{#if modelRec}Recommended: {modelRec.model}{/if}
						</div>
					</div>
				</button>
				<button
					onclick={() => { llmProvider = 'openai'; keyValid = null; llmApiKey = ''; }}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'openai' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">&#x2601;</div>
					<div>
						<div class="text-sm font-medium text-foreground">OpenAI</div>
						<div class="text-xs text-muted-foreground">GPT-4o, GPT-5 series. Requires API key.</div>
					</div>
				</button>
				<button
					onclick={() => { llmProvider = 'anthropic'; keyValid = null; llmApiKey = ''; }}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'anthropic' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">&#x2728;</div>
					<div>
						<div class="text-sm font-medium text-foreground">Anthropic</div>
						<div class="text-xs text-muted-foreground">Claude Opus, Sonnet, Haiku. Requires API key.</div>
					</div>
				</button>
			</div>

			{#if llmProvider === 'local'}
				<div class="space-y-3 rounded-lg border border-border p-4">
					<div>
						<label for="local-model" class="block text-xs font-medium text-foreground mb-1">Model</label>
						<input id="local-model" type="text" bind:value={llmLocalModel} placeholder="qwen3.5:9b"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
					</div>
					{#if modelRec}<p class="text-xs text-muted-foreground">{modelRec.reason}</p>{/if}
					{#if modelRec?.all_options}
						<div class="space-y-1">
							<div class="text-xs font-medium text-muted-foreground">Available sizes:</div>
							<div class="grid grid-cols-2 gap-1">
								{#each modelRec.all_options as opt}
									<button onclick={() => llmLocalModel = opt.model} disabled={!opt.suitable}
										class="rounded px-2 py-1 text-xs text-left transition-colors {llmLocalModel === opt.model
											? 'bg-primary/10 border border-primary text-foreground'
											: opt.suitable ? 'border border-border hover:border-primary/40 text-foreground'
											: 'border border-border text-muted-foreground/40 cursor-not-allowed'}">
										{opt.display_name} <span class="text-muted-foreground">({opt.ram_required_gb}GB)</span>
									</button>
								{/each}
							</div>
						</div>
					{/if}
					<div>
						<label for="local-url" class="block text-xs font-medium text-foreground mb-1">
							Remote server <span class="text-muted-foreground font-normal">(optional)</span>
						</label>
						<input id="local-url" type="text" bind:value={llmLocalBaseUrl} placeholder="http://localhost:11434"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						<p class="mt-1 text-xs text-muted-foreground">Leave empty to run the model inside xpressclaw.</p>
					</div>
				</div>
			{:else if llmProvider === 'openai' || llmProvider === 'anthropic'}
				<div class="space-y-3 rounded-lg border border-border p-4">
					<div>
						<label for="api-key" class="block text-xs font-medium text-foreground mb-1">API Key</label>
						<div class="flex gap-2">
							<input id="api-key" type="password" bind:value={llmApiKey}
								placeholder={llmProvider === 'anthropic' ? 'sk-ant-...' : 'sk-...'}
								class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
							<button onclick={validateApiKey} disabled={!llmApiKey.trim() || keyValidating}
								class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent disabled:opacity-50">
								{keyValidating ? 'Checking...' : 'Validate'}
							</button>
						</div>
						{#if keyValid === true}<p class="mt-1 text-xs text-emerald-500">API key is valid</p>{/if}
						{#if keyValid === false}<p class="mt-1 text-xs text-red-500">{keyError}</p>{/if}
					</div>
					{#if llmProvider === 'openai'}
						<div>
							<label for="openai-url" class="block text-xs font-medium text-foreground mb-1">
								Base URL <span class="text-muted-foreground font-normal">(optional)</span>
							</label>
							<input id="openai-url" type="text" bind:value={llmBaseUrl} placeholder="https://api.openai.com"
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
					{/if}
				</div>
			{/if}
		{/if}

		<div class="mt-6 flex justify-between">
			<button onclick={() => goToStep(0)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			<button onclick={() => goToStep(2)} disabled={!canProceedLlm()}
				class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed">Continue</button>
		</div>

	<!-- Step 2: Connectors -->
	{:else if step === 2}
		<h2 class="text-lg font-semibold text-foreground mb-1">Connectors</h2>
		<p class="text-sm text-muted-foreground mb-6">
			MCP tool servers for your agent. Pre-filled from the template.
		</p>

		<div class="mb-4">
			<div class="text-xs font-medium text-muted-foreground mb-2">Quick add:</div>
			<div class="flex flex-wrap gap-2">
				{#each mcpPresets as preset}
					{@const added = preset.id in mcpServers}
					<button onclick={() => added ? removeMcpServer(preset.id) : addMcpPreset(preset)}
						class="rounded-md border px-3 py-1.5 text-xs transition-colors {added
							? 'border-primary bg-primary/10 text-primary' : 'border-border hover:border-primary/40'}">
						{preset.name} {added ? '✓' : '+'}
					</button>
				{/each}
			</div>
		</div>

		{#if Object.keys(mcpServers).length > 0}
			<div class="space-y-2 mb-4">
				{#each Object.entries(mcpServers) as [id, server]}
					<div class="flex items-center justify-between rounded-lg border border-border p-3">
						<div>
							<div class="text-sm font-medium text-foreground">{id}</div>
							<div class="text-xs text-muted-foreground">{server.command} {server.args?.join(' ') || ''}</div>
						</div>
						<button onclick={() => removeMcpServer(id)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground">&#x2715;</button>
					</div>
				{/each}
			</div>
		{:else}
			<div class="rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground mb-4">
				No connectors added yet. This is optional.
			</div>
		{/if}

		{#if showAddMcp}
			<div class="rounded-lg border border-border p-3 space-y-2 mb-4">
				<input type="text" bind:value={newMcpName} placeholder="Server name"
					class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<input type="text" bind:value={newMcpCommand} placeholder="Command (e.g., npx)"
					class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<input type="text" bind:value={newMcpArgs} placeholder="Arguments (space-separated)"
					class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<div class="flex gap-2">
					<button onclick={addCustomMcp} class="rounded-md bg-primary px-3 py-1 text-xs text-primary-foreground hover:bg-primary/90">Add</button>
					<button onclick={() => showAddMcp = false} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Cancel</button>
				</div>
			</div>
		{:else}
			<button onclick={() => showAddMcp = true} class="text-xs text-muted-foreground hover:text-foreground">+ Add custom connector</button>
		{/if}

		<div class="mt-6 flex justify-between">
			<button onclick={() => goToStep(1)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			<button onclick={() => goToStep(3)} class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90">Continue</button>
		</div>

	<!-- Step 3: Docker / Environment -->
	{:else if step === 3}
		<h2 class="text-lg font-semibold text-foreground mb-1">Environment</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Agents can run in Docker containers for security isolation.
		</p>

		{#if dockerLoading}
			<div class="flex items-center gap-3 rounded-lg border border-border p-4">
				<div class="h-5 w-5 animate-spin rounded-full border-2 border-primary border-t-transparent"></div>
				<span class="text-sm text-muted-foreground">Checking for Docker...</span>
			</div>
		{:else if dockerStatus?.available}
			<div class="flex items-center gap-3 rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-4">
				<div class="flex h-8 w-8 items-center justify-center rounded-full bg-emerald-500/20 text-emerald-500">&#10003;</div>
				<div>
					<div class="text-sm font-medium text-foreground">Docker is running</div>
					<div class="text-xs text-muted-foreground">Container isolation is available</div>
				</div>
			</div>
		{:else}
			<div class="flex items-center gap-3 rounded-lg border border-amber-500/30 bg-amber-500/5 p-4 mb-4">
				<div class="flex h-8 w-8 items-center justify-center rounded-full bg-amber-500/20 text-amber-500">!</div>
				<div>
					<div class="text-sm font-medium text-foreground">Docker is not available</div>
					<div class="text-xs text-muted-foreground">{dockerStatus?.error || ''}</div>
				</div>
			</div>
			<div class="space-y-2 text-sm mb-4">
				<div class="flex gap-2">
					<a href="https://docs.docker.com/get-docker/" target="_blank" rel="noopener noreferrer"
						class="inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Docker Desktop &#8599;</a>
					<a href="https://podman.io/getting-started/installation" target="_blank" rel="noopener noreferrer"
						class="inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Podman &#8599;</a>
					<button onclick={async () => { dockerLoading = true; dockerStatus = await setup.checkDocker(); dockerLoading = false; }}
						class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Retry</button>
				</div>
			</div>
			<label class="flex items-start gap-3 cursor-pointer rounded-lg border border-border p-4">
				<input type="checkbox" bind:checked={containerless} class="mt-0.5 rounded border-border" />
				<div>
					<div class="text-sm font-medium text-foreground">Continue without containers</div>
					<div class="text-xs text-muted-foreground">
						Only use this on a dedicated machine or VM where security isolation is not needed.
					</div>
				</div>
			</label>
		{/if}

		<div class="mt-6 flex justify-between">
			<button onclick={() => goToStep(2)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			<button onclick={completeSetup} disabled={saving || (!dockerStatus?.available && !containerless)}
				class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed">
				{#if saving}Saving...{:else}Complete Setup{/if}
			</button>
		</div>

		{#if saveError}<p class="mt-2 text-xs text-red-500">{saveError}</p>{/if}

	<!-- Step 4: Complete -->
	{:else if step === 4}
		{#if downloading}
			<div class="text-center py-8">
				<h2 class="text-lg font-semibold text-foreground mb-2">Downloading Model</h2>
				<p class="text-sm text-muted-foreground mb-4">{downloadProgress?.filename || 'Preparing...'}</p>
				<div class="w-full max-w-md mx-auto bg-muted rounded-full h-3 mb-2">
					<div class="bg-primary h-3 rounded-full transition-all duration-300"
						style="width: {downloadProgress && downloadProgress.total_bytes > 0
							? Math.min(100, downloadProgress.downloaded_bytes / downloadProgress.total_bytes * 100) : 0}%"></div>
				</div>
				<p class="text-xs text-muted-foreground">
					{formatBytes(downloadProgress?.downloaded_bytes ?? 0)} / {formatBytes(downloadProgress?.total_bytes ?? 0)}
				</p>
				{#if saveError}<p class="mt-4 text-xs text-red-500">{saveError}</p>{/if}
			</div>
		{:else}
			<div class="text-center py-8">
				<div class="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-emerald-500/20 text-emerald-500 text-3xl">&#10003;</div>
				<h2 class="text-lg font-semibold text-foreground mb-2">
					{mode === 'add-agent' ? 'Agent Added!' : 'Setup Complete!'}
				</h2>
				<p class="text-sm text-muted-foreground mb-6">
					{#if startingAgents}Starting agents...
					{:else}Your agent <strong>{agentName}</strong> is ready to go!{/if}
				</p>
				<button onclick={() => goto(mode === 'add-agent' ? '/agents' : '/')}
					class="rounded-md bg-primary px-6 py-2 text-sm text-primary-foreground hover:bg-primary/90">
					{mode === 'add-agent' ? 'Back to Agents' : 'Start Chatting'}
				</button>
			</div>
		{/if}
	{/if}
</div>
