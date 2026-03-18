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
	const steps = ['Agent', 'LLM', 'Workspace', 'Environment', 'Complete'];

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

	// -- Step 2: Workspace & Tools --
	let mcpServers = $state<Record<string, { type: string; command?: string; args?: string[]; env?: Record<string, string>; url?: string }>>({});
	let showAddMcp = $state(false);
	let newMcpName = $state('');
	let newMcpCommand = $state('');
	let newMcpArgs = $state('');

	// Workspace folders to mount into /workspace/{basename}
	let workspaceFolders = $state<string[]>([]);
	let newFolderPath = $state('');

	// Optional tool toggles + config
	let fetchEnabled = $state(false);
	let fetchMode = $state<'allow' | 'block'>('block');
	let fetchPatterns = $state('');
	let gitEnabled = $state(false);
	let gitSshKeyPath = $state('');
	let githubEnabled = $state(false);
	let githubPat = $state('');

	// Legacy preset list for custom MCP servers only
	const mcpPresets: { name: string; id: string; command: string; args: string; envKey: string }[] = [];

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

		// Pre-fill optional tools from preset
		const tools = preset.default_tools || [];
		const servers = preset.default_mcp_servers || {};
		fetchEnabled = tools.includes('fetch') || 'fetch' in servers;
		gitEnabled = tools.includes('git') || 'git' in servers;
		githubEnabled = tools.includes('github') || 'github' in servers;

		// Keep non-default MCP servers (custom ones)
		const defaultKeys = new Set(['shell', 'filesystem', 'fetch', 'git', 'github']);
		const custom: typeof mcpServers = {};
		for (const [k, v] of Object.entries(servers)) {
			if (!defaultKeys.has(k)) custom[k] = v;
		}
		mcpServers = custom;

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

			// Build MCP servers from tool toggles + any custom servers
			const allMcpServers = { ...mcpServers };
			if (fetchEnabled) {
				allMcpServers['fetch'] = {
					type: 'stdio', command: 'npx',
					args: ['-y', '@modelcontextprotocol/server-fetch'],
					env: fetchPatterns.trim() ? {
						[fetchMode === 'allow' ? 'FETCH_ALLOWED_URLS' : 'FETCH_BLOCKED_URLS']: fetchPatterns.trim()
					} : {}
				};
			}
			if (gitEnabled) {
				allMcpServers['git'] = {
					type: 'stdio', command: 'npx',
					args: ['-y', '@modelcontextprotocol/server-git'],
					env: gitSshKeyPath.trim() ? { SSH_KEY_PATH: gitSshKeyPath.trim() } : {}
				};
			}
			if (githubEnabled && githubPat.trim()) {
				allMcpServers['github'] = {
					type: 'stdio', command: 'docker',
					args: ['run', '-i', '--rm', '-e', 'GITHUB_PERSONAL_ACCESS_TOKEN', 'ghcr.io/github/github-mcp-server'],
					env: { GITHUB_PERSONAL_ACCESS_TOKEN: githubPat.trim() }
				};
			}

			// Build volumes from workspace folders
			const volumes = workspaceFolders
				.filter(f => f.trim())
				.map(f => {
					const basename = f.trim().split('/').filter(Boolean).pop() || 'workspace';
					return `${f.trim()}:/workspace/${basename}`;
				});

			// Build tools list from enabled toggles
			const tools = ['filesystem', 'shell', 'memory'];
			if (fetchEnabled) tools.push('fetch');
			if (gitEnabled) tools.push('git');
			if (githubEnabled) tools.push('github');

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
					tools,
					volumes: volumes.length > 0 ? volumes : undefined,
				}],
				mcp_servers: Object.keys(allMcpServers).length > 0 ? allMcpServers : undefined,
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
			console.error('Setup failed:', e);
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
		<div class="flex items-start justify-between mb-1">
			<div>
				<h2 class="text-lg font-semibold text-foreground">
					{mode === 'add-agent' ? 'Add Agent' : 'Choose Your Agent'}
				</h2>
				<p class="text-sm text-muted-foreground mt-1">
					Pick a template to get started. You can customize everything in the next steps.
				</p>
			</div>
			{#if mode === 'add-agent'}
				<button onclick={() => goto('/agents')} class="rounded-md p-2 text-muted-foreground hover:bg-accent hover:text-foreground">
					<span class="text-xl">&times;</span>
				</button>
			{/if}
		</div>

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
			{#if mode === 'add-agent'}
				<button onclick={() => goto('/agents')} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</button>
			{:else}
				<button onclick={() => goToStep(0)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			{/if}
			<button onclick={() => goToStep(2)} disabled={!canProceedLlm()}
				class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed">Continue</button>
		</div>

	<!-- Step 2: Workspace & Tools -->
	{:else if step === 2}
		<h2 class="text-lg font-semibold text-foreground mb-1">Workspace & Tools</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Share folders with your agent and configure optional tools.
		</p>

		<!-- Workspace Folders -->
		<div class="mb-6">
			<h3 class="text-sm font-medium text-foreground mb-2">Workspace Folders</h3>
			<p class="text-xs text-muted-foreground mb-3">
				Folders from your machine that the agent can read and write. Each folder is mounted at <code class="bg-muted px-1 rounded">/workspace/</code> in the container.
			</p>
			{#if workspaceFolders.length > 0}
				<div class="space-y-2 mb-3">
					{#each workspaceFolders as folder, i}
						<div class="flex items-center gap-2 rounded-lg border border-border px-3 py-2">
							<span class="flex-1 text-sm font-mono text-foreground truncate">{folder}</span>
							<span class="text-xs text-muted-foreground">/workspace/{folder.split('/').filter(Boolean).pop()}</span>
							<button onclick={() => { workspaceFolders = workspaceFolders.filter((_, j) => j !== i); }}
								class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground">&#x2715;</button>
						</div>
					{/each}
				</div>
			{/if}
			<div class="flex gap-2">
				<input type="text" bind:value={newFolderPath} placeholder="~/projects/my-app"
					onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter' && newFolderPath.trim()) { workspaceFolders = [...workspaceFolders, newFolderPath.trim()]; newFolderPath = ''; } }}
					class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<button onclick={() => { if (newFolderPath.trim()) { workspaceFolders = [...workspaceFolders, newFolderPath.trim()]; newFolderPath = ''; } }}
					disabled={!newFolderPath.trim()}
					class="rounded-md border border-border px-3 py-2 text-sm hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
			</div>
		</div>

		<!-- Default Tools -->
		<div class="mb-4">
			<h3 class="text-sm font-medium text-foreground mb-2">Default Tools</h3>
			<div class="flex gap-2">
				<span class="inline-flex items-center gap-1 rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">
					&#x1f4c1; Filesystem
				</span>
				<span class="inline-flex items-center gap-1 rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">
					&#x1f4bb; Shell
				</span>
			</div>
			<p class="text-xs text-muted-foreground mt-1">Always included for all agents.</p>
		</div>

		<!-- Optional Tools -->
		<div class="mb-4">
			<h3 class="text-sm font-medium text-foreground mb-3">Optional Tools</h3>
			<div class="space-y-2">
				<!-- Fetch -->
				<div class="rounded-lg border {fetchEnabled ? 'border-primary bg-primary/5' : 'border-border'} p-3">
					<label class="flex items-center gap-3 cursor-pointer">
						<input type="checkbox" bind:checked={fetchEnabled} class="rounded border-border" />
						<div>
							<div class="text-sm font-medium text-foreground">Internet Access (Fetch)</div>
							<div class="text-xs text-muted-foreground">Fetch web pages and APIs</div>
						</div>
					</label>
					{#if fetchEnabled}
						<div class="mt-3 ml-7 space-y-2">
							<div class="flex gap-2">
								<button onclick={() => fetchMode = 'block'}
									class="rounded-md border px-2 py-1 text-xs {fetchMode === 'block' ? 'border-primary bg-primary/10' : 'border-border'}">
									Block list
								</button>
								<button onclick={() => fetchMode = 'allow'}
									class="rounded-md border px-2 py-1 text-xs {fetchMode === 'allow' ? 'border-primary bg-primary/10' : 'border-border'}">
									Allow list
								</button>
							</div>
							<textarea bind:value={fetchPatterns} rows="2"
								placeholder={fetchMode === 'allow' ? 'Allowed URL patterns (one per line, e.g. *.github.com/*)' : 'Blocked URL patterns (one per line, leave empty to allow all)'}
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"></textarea>
						</div>
					{/if}
				</div>

				<!-- Git -->
				<div class="rounded-lg border {gitEnabled ? 'border-primary bg-primary/5' : 'border-border'} p-3">
					<label class="flex items-center gap-3 cursor-pointer">
						<input type="checkbox" bind:checked={gitEnabled} class="rounded border-border" />
						<div>
							<div class="text-sm font-medium text-foreground">Git</div>
							<div class="text-xs text-muted-foreground">Interact with Git repositories</div>
						</div>
					</label>
					{#if gitEnabled}
						<div class="mt-3 ml-7">
							<label for="git-ssh" class="block text-xs font-medium text-foreground mb-1">
								SSH Key Path <span class="text-muted-foreground font-normal">(optional)</span>
							</label>
							<input id="git-ssh" type="text" bind:value={gitSshKeyPath} placeholder="~/.ssh/id_ed25519"
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
					{/if}
				</div>

				<!-- GitHub -->
				<div class="rounded-lg border {githubEnabled ? 'border-primary bg-primary/5' : 'border-border'} p-3">
					<label class="flex items-center gap-3 cursor-pointer">
						<input type="checkbox" bind:checked={githubEnabled} class="rounded border-border" />
						<div>
							<div class="text-sm font-medium text-foreground">GitHub</div>
							<div class="text-xs text-muted-foreground">Issues, PRs, repos via GitHub MCP Server</div>
						</div>
					</label>
					{#if githubEnabled}
						<div class="mt-3 ml-7">
							<label for="github-pat" class="block text-xs font-medium text-foreground mb-1">Personal Access Token</label>
							<input id="github-pat" type="password" bind:value={githubPat} placeholder="ghp_..."
								class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
							<p class="mt-1 text-xs text-muted-foreground">
								Create a token at GitHub &rarr; Settings &rarr; Developer Settings &rarr; Personal Access Tokens
							</p>
						</div>
					{/if}
				</div>
			</div>
		</div>

		<p class="text-xs text-muted-foreground mb-4">You can add and configure tools later from agent settings.</p>

		<!-- Custom MCP connector (collapsed) -->
		{#if showAddMcp}
			<div class="rounded-lg border border-border p-3 space-y-2 mb-4">
				<div class="text-xs font-medium text-foreground mb-1">Custom MCP Server</div>
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
			<button onclick={() => showAddMcp = true} class="text-xs text-muted-foreground hover:text-foreground">+ Add custom MCP connector</button>
		{/if}

		{#if Object.keys(mcpServers).length > 0}
			<div class="mt-3 space-y-2">
				{#each Object.entries(mcpServers) as [id, server]}
					<div class="flex items-center justify-between rounded-lg border border-border p-2">
						<div class="text-xs text-foreground">{id} <span class="text-muted-foreground">({server.command} {server.args?.join(' ') || ''})</span></div>
						<button onclick={() => removeMcpServer(id)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground text-xs">&#x2715;</button>
					</div>
				{/each}
			</div>
		{/if}

		<div class="mt-6 flex justify-between">
			{#if mode === 'add-agent'}
				<button onclick={() => goto('/agents')} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</button>
			{:else}
				<button onclick={() => goToStep(1)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			{/if}
			<button onclick={() => goToStep(3)}
				class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90">Continue</button>
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

		<div class="mt-6 flex items-center justify-between">
			<button onclick={() => goToStep(2)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			<div class="flex items-center gap-3">
				{#if mode === 'add-agent'}
					<button onclick={() => goto('/agents')} class="rounded-md px-4 py-2 text-sm hover:bg-accent">Cancel</button>
				{/if}
				<button onclick={completeSetup} disabled={saving || (!dockerStatus?.available && !containerless)}
					class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed">
					{#if saving}Saving...{:else}Complete Setup{/if}
				</button>
			</div>
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
