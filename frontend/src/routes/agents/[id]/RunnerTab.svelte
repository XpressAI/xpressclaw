<script lang="ts">
	import { onMount } from 'svelte';
	import { mcpServers, sessions, setup } from '$lib/api';
	import { canonicalHarnessKind } from '$lib/utils';
	import DirectoryPicker from '$lib/components/DirectoryPicker.svelte';
	import type { AcpAgentCatalogEntry, AcpConfigOption, AcpModeState, LiveConfig, McpServerDefinition, McpVerificationResult, NativeRunnerConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: { runner: NativeRunnerConfig; volumes: string[] }) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();
	let kind = $state('auto');
	let image = $state('');
	let workspace = $state('');
	let projectName = $state('');
	let model = $state('');
	let subscriptionAuth = $state(true);
	let sshAgentForwarding = $state(false);
	let sshAgentAvailable = $state(false);
	let sshAgentSocket = $state('');
	let containerEngine = $state<'none' | 'host'>('none');
	let commandText = $state('');
	let configOptions = $state<AcpConfigOption[]>([]);
	let sessionConfig = $state<Record<string, string | boolean>>({});
	let selectedMcpServers = $state<string[]>([]);
	let serverCatalog = $state<McpServerDefinition[]>([]);
	let environmentText = $state('');
	let startupCommandsText = $state('');
	let volumesText = $state('');
	let showWorkspacePicker = $state(false);
	let agentCatalog = $state<AcpAgentCatalogEntry[]>([]);
	let addingMcp = $state(false);
	let mcpName = $state('');
	let mcpType = $state<'stdio' | 'http' | 'sse'>('stdio');
	let mcpCommandOrUrl = $state('');
	let mcpArgs = $state('');
	let mcpEnvironment = $state('');
	let mcpHeaders = $state('');
	let editingMcpName = $state<string | null>(null);
	let confirmDeleteMcp = $state<string | null>(null);
	let mcpError = $state('');
	let verifyingMcp = $state<string | null>(null);
	let mcpVerification = $state<Record<string, McpVerificationResult>>({});
	let modelOptions = $derived(selectChoices(configOptions.find((option) => option.category === 'model' || option.id === 'model')));
	const fallbackAgents = [
		{ kind: 'codex', name: 'Codex', image: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest' },
		{ kind: 'claude', name: 'Claude Agent', image: 'ghcr.io/xpressai/xpressclaw-runner-claude:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-claude-docker:latest' },
		{ kind: 'opencode', name: 'OpenCode', image: 'ghcr.io/xpressai/xpressclaw-runner-opencode:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-opencode-docker:latest' },
		{ kind: 'deepseek-harness', name: 'DeepSeek Harness', image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness-docker:latest' }
	];
	let agentOptions = $derived(agentCatalog.length > 0 ? agentCatalog : fallbackAgents);

	function defaultImage(): string {
		const agent = agentOptions.find((option) => option.kind === kind);
		if (!agent) return '';
		return containerEngine === 'host' ? agent.host_image : agent.image;
	}

	function isBuiltInImage(candidate: string): boolean {
		return agentOptions.some((agent) => {
			const localImage = agent.image.replace('ghcr.io/xpressai/', '');
			const localHostImage = agent.host_image.replace('ghcr.io/xpressai/', '');
			return [agent.image, agent.host_image, localImage, localHostImage].includes(candidate);
		});
	}

	function selectChoices(option: AcpConfigOption | undefined): { value: string; name: string; description?: string | null }[] {
		if (!option || !Array.isArray(option.options)) return [];
		return option.options.flatMap((entry) => 'options' in entry ? entry.options : [entry]);
	}

	function selectDefaultImage() {
		image = defaultImage();
		if (kind === 'custom') subscriptionAuth = false;
	}

	function setContainerEngine(enabled: boolean) {
		const replaceImage = !image.trim() || isBuiltInImage(image.trim());
		containerEngine = enabled ? 'host' : 'none';
		if (kind !== 'custom' && replaceImage) image = defaultImage();
	}

	function validConfigOptions(value: unknown): AcpConfigOption[] {
		if (!Array.isArray(value)) return [];
		return value.filter((option): option is AcpConfigOption => {
			if (typeof option !== 'object' || option === null) return false;
			const item = option as Record<string, unknown>;
			return typeof item.id === 'string' && typeof item.name === 'string'
				&& (item.type === 'select' || item.type === 'boolean');
		});
	}

	function modeOption(value: unknown): AcpConfigOption | null {
		if (typeof value !== 'object' || value === null) return null;
		const modes = value as unknown as AcpModeState;
		if (typeof modes.currentModeId !== 'string' || !Array.isArray(modes.availableModes)) return null;
		return {
			id: 'mode', name: 'Mode', category: 'mode', type: 'select',
			currentValue: modes.currentModeId,
			options: modes.availableModes.map((mode) => ({ value: mode.id, name: mode.name, description: mode.description }))
		};
	}

	function applyAdvertisedControls(events: Awaited<ReturnType<typeof sessions.events>>) {
		const advertised = [...events].reverse().find((event) => event.event_type === 'session_config_options');
		const options = validConfigOptions(advertised?.payload.config_options);
		if (!options.some((option) => option.category === 'mode' || option.id === 'mode')) {
			const legacyMode = modeOption(advertised?.payload.modes);
			if (legacyMode) options.unshift(legacyMode);
		}
		configOptions = options;
	}

	onMount(async () => {
		try {
			const [events, catalog, agents, systemInfo] = await Promise.all([
				sessions.events(agentId).catch(() => []),
				mcpServers.list().catch(() => ({ servers: [] })),
				setup.agentCatalog().catch(() => ({ agents: [] })),
				setup.systemInfo().catch(() => null)
			]);
			applyAdvertisedControls(events);
			serverCatalog = catalog.servers;
			agentCatalog = agents.agents;
			sshAgentAvailable = systemInfo?.ssh_agent_available ?? false;
			sshAgentSocket = systemInfo?.ssh_agent_socket ?? '';
		} catch {
			configOptions = [];
			serverCatalog = [];
		}
	});

	let initializedAgentId = '';
	let initializedConfig: LiveConfig['agents'][0] | null = null;
	$effect(() => {
		const config = agentConfig;
		if (!config?.runner || (config === initializedConfig && agentId === initializedAgentId)) return;
		initializedConfig = config;
		initializedAgentId = agentId;

		const configuredKind = config.runner.kind;
		kind = configuredKind === 'auto'
			? canonicalHarnessKind(config.backend)
			: canonicalHarnessKind(configuredKind);
		containerEngine = config.runner.container_engine ?? 'none';
		image = isBuiltInImage(config.runner.image) ? defaultImage() : config.runner.image;
		workspace = config.runner.workspace ?? '';
		projectName = config.runner.project_name ?? '';
		model = config.runner.model ?? '';
		sessionConfig = { ...config.runner.session_config };
		selectedMcpServers = [...config.runner.mcp_servers];
		environmentText = Object.entries(config.runner.environment).map(([key, value]) => `${key}=${value}`).join('\n');
		startupCommandsText = (config.runner.startup_commands ?? []).join('\n');
		volumesText = config.volumes.join('\n');
		subscriptionAuth = config.runner.subscription_auth;
		sshAgentForwarding = config.runner.ssh_agent_forwarding ?? false;
		commandText = config.runner.command.join('\n');
	});

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			onSave({
				runner: {
					kind,
					image: image.trim() || defaultImage(),
					workspace: workspace.trim() || null,
					project_name: projectName.trim() || null,
					// Generic ACP model controls supersede the pre-discovery
					// compatibility preference once the harness advertises them.
					model: modelOptions.length > 0 ? null : (model.trim() || null),
					session_config: sessionConfig,
					mcp_servers: selectedMcpServers,
					environment: Object.fromEntries(environmentText.split('\n').map((line) => line.trim()).filter(Boolean).map((line) => {
						const separator = line.indexOf('=');
						return separator === -1 ? [line, ''] : [line.slice(0, separator).trim(), line.slice(separator + 1)];
					}).filter(([key]) => key)),
					startup_commands: startupCommandsText.split('\n').map((line) => line.trim()).filter(Boolean),
					command: commandText.split('\n').map((line) => line.trim()).filter(Boolean),
					subscription_auth: subscriptionAuth,
					ssh_agent_forwarding: sshAgentForwarding,
					container_engine: containerEngine
				},
				volumes: volumesText.split('\n').map((line) => line.trim()).filter(Boolean)
			});
		}
	});

	function toggleMcp(name: string) {
		selectedMcpServers = selectedMcpServers.includes(name)
			? selectedMcpServers.filter((item) => item !== name)
			: [...selectedMcpServers, name];
	}

	function parseKeyValueLines(value: string): Record<string, string> {
		return Object.fromEntries(value.split('\n').map((line) => line.trim()).filter(Boolean).map((line) => {
			const separator = line.indexOf('=');
			return separator === -1 ? [line, ''] : [line.slice(0, separator).trim(), line.slice(separator + 1)];
		}).filter(([key]) => key));
	}

	function formatKeyValueLines(value: Record<string, string> | string[] | undefined): string {
		if (!value || Array.isArray(value)) return '';
		return Object.entries(value).map(([key, entry]) => `${key}=${entry}`).join('\n');
	}

	function resetMcpForm() {
		addingMcp = false;
		editingMcpName = null;
		mcpName = '';
		mcpType = 'stdio';
		mcpCommandOrUrl = '';
		mcpArgs = '';
		mcpEnvironment = '';
		mcpHeaders = '';
		mcpError = '';
	}

	function openNewMcp() {
		resetMcpForm();
		addingMcp = true;
	}

	function editMcpServer(server: McpServerDefinition) {
		editingMcpName = server.name;
		mcpName = server.name;
		mcpType = server.type === 'http' || server.type === 'sse' ? server.type : 'stdio';
		mcpCommandOrUrl = server.type === 'stdio' ? (server.command ?? '') : (server.url ?? '');
		mcpArgs = server.args.join('\n');
		mcpEnvironment = formatKeyValueLines(server.env);
		mcpHeaders = formatKeyValueLines(server.headers);
		mcpError = '';
		addingMcp = true;
	}

	async function saveMcpServer() {
		mcpError = '';
		if (!mcpName.trim() || !mcpCommandOrUrl.trim()) return;
		try {
			const definition: McpServerDefinition = {
				name: mcpName.trim(), type: mcpType,
				command: mcpType === 'stdio' ? mcpCommandOrUrl.trim() : null,
				url: mcpType === 'stdio' ? null : mcpCommandOrUrl.trim(),
				args: mcpType === 'stdio' ? mcpArgs.split('\n').map((line) => line.trim()).filter(Boolean) : [],
				env: mcpType === 'stdio' ? parseKeyValueLines(mcpEnvironment) : {},
				headers: mcpType === 'stdio' ? {} : parseKeyValueLines(mcpHeaders)
			};
			await mcpServers.upsert(definition);
			mcpVerification = Object.fromEntries(Object.entries(mcpVerification).filter(([key]) => key !== definition.name));
			serverCatalog = (await mcpServers.list()).servers;
			if (!selectedMcpServers.includes(definition.name)) selectedMcpServers = [...selectedMcpServers, definition.name];
			resetMcpForm();
		} catch (error) {
			mcpError = String(error);
		}
	}

	async function deleteMcpServer(name: string) {
		if (confirmDeleteMcp !== name) {
			confirmDeleteMcp = name;
			return;
		}
		mcpError = '';
		try {
			await mcpServers.delete(name);
			selectedMcpServers = selectedMcpServers.filter((item) => item !== name);
			mcpVerification = Object.fromEntries(Object.entries(mcpVerification).filter(([key]) => key !== name));
			serverCatalog = (await mcpServers.list()).servers;
			confirmDeleteMcp = null;
			if (editingMcpName === name) resetMcpForm();
		} catch (error) {
			mcpError = String(error);
		}
	}

	async function verifyMcpServer(server: McpServerDefinition) {
		if (verifyingMcp) return;
		verifyingMcp = server.name;
		mcpVerification = Object.fromEntries(Object.entries(mcpVerification).filter(([key]) => key !== server.name));
		try {
			mcpVerification = {
				...mcpVerification,
				[server.name]: await mcpServers.verify(server.name, agentId)
			};
		} catch (error) {
			mcpVerification = {
				...mcpVerification,
				[server.name]: {
					ok: false,
					status: 'verification_failed',
					message: error instanceof Error ? error.message : 'Could not verify MCP server',
					suggestion: 'Check the saved server configuration and try again.'
				}
			};
		} finally {
			verifyingMcp = null;
		}
	}

	function verificationTone(result: McpVerificationResult): string {
		if (result.ok) return 'border-emerald-500/25 bg-emerald-500/10 text-emerald-600';
		if (result.status === 'authentication_required' || result.status === 'command_path_incorrect') {
			return 'border-amber-500/25 bg-amber-500/10 text-amber-600';
		}
		return 'border-destructive/25 bg-destructive/5 text-destructive';
	}
</script>

<div class="mx-auto max-w-3xl space-y-6">
	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">ACP harness</h2>
		<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
			XpressClaw talks to this coding harness over the Agent Client Protocol. New tasks branch from this agent's active conversation when the harness supports it, while follow-ups stay with their task; the harness keeps ownership of its reasoning, tools, and subagents.
		</p>

		<div class="mt-5 grid gap-4 sm:grid-cols-2">
			<div class={modelOptions.length > 0 ? 'sm:col-span-2' : ''}>
				<label for="runner-kind" class="mb-1 block text-xs font-medium text-muted-foreground">Harness</label>
				<select id="runner-kind" bind:value={kind} onchange={selectDefaultImage} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
					{#each agentOptions as agent}
						<option value={agent.kind}>{agent.name}</option>
					{/each}
					<option value="custom">Other ACP harness</option>
				</select>
			</div>
			{#if modelOptions.length === 0}<div>
				<label for="runner-model" class="mb-1 block text-xs font-medium text-muted-foreground">Model</label>
				<input id="runner-model" list="runner-model-options" bind:value={model} placeholder="Harness default" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
				<datalist id="runner-model-options">
					{#each modelOptions as option}
						<option value={option.value}>{option.name}</option>
					{/each}
				</datalist>
				<p class="mt-1 text-[11px] text-muted-foreground">
					{modelOptions.length > 0 ? `${modelOptions.length} choices reported by the harness. ` : 'Choices appear after the harness starts its first session. '}
					Leave blank to use the harness default.
				</p>
			</div>{/if}
		</div>

		<div class="mt-4">
			<label for="runner-workspace" class="mb-1 block text-xs font-medium text-muted-foreground">Workspace folder</label>
			<div class="flex gap-2">
				<input id="runner-workspace" bind:value={workspace} placeholder="~/projects/my-app" class="min-w-0 flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
				<button type="button" onclick={() => (showWorkspacePicker = true)} class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent">Browse…</button>
			</div>
			<p class="mt-1 text-[11px] text-muted-foreground">
				{containerEngine === 'host'
					? 'Mounted read-write at the same absolute path so Docker Compose bind mounts resolve on the host.'
					: 'Mounted read-write at /workspace.'}
			</p>
		</div>

		<div class="mt-4">
			<label for="runner-project-name" class="mb-1 block text-xs font-medium text-muted-foreground">Agent name</label>
			<input id="runner-project-name" bind:value={projectName} placeholder="Derived from workspace folder" class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring" />
		</div>

		<div class="mt-4">
			<label for="runner-image" class="mb-1 block text-xs font-medium text-muted-foreground">Container image</label>
			<input id="runner-image" bind:value={image} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
			<p class="mt-1 text-[11px] text-muted-foreground">Published images are pulled on demand. Extend one or provide a custom image whose command starts an ACP server over stdio.</p>
		</div>
	</div>

	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">Harness capabilities</h2>
		<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
			These controls come directly from this ACP harness. They become available after its first turn and are applied before every new task turn.
		</p>
		{#if configOptions.length === 0}
			<div class="mt-4 rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">
				Run one task to discover this harness's modes, models, reasoning levels, and custom controls.
			</div>
		{:else}
			<div class="mt-4 grid gap-4 sm:grid-cols-2">
				{#each configOptions as option}
					<div>
						<label for={`config-${option.id}`} class="mb-1 block text-xs font-medium text-muted-foreground">{option.name}</label>
						{#if option.type === 'boolean'}
							<label class="flex min-h-9 items-center gap-2 rounded-md border border-input bg-background px-3 text-sm">
								<input id={`config-${option.id}`} type="checkbox"
									checked={Boolean(sessionConfig[option.id] ?? option.currentValue)}
									onchange={(event) => sessionConfig = { ...sessionConfig, [option.id]: event.currentTarget.checked }} />
								Enabled
							</label>
						{:else}
							<select id={`config-${option.id}`}
								value={String(sessionConfig[option.id] ?? option.currentValue)}
								onchange={(event) => sessionConfig = { ...sessionConfig, [option.id]: event.currentTarget.value }}
								class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
								{#each selectChoices(option) as choice}
									<option value={choice.value}>{choice.name}</option>
								{/each}
							</select>
						{/if}
						{#if option.description}<p class="mt-1 text-[11px] text-muted-foreground">{option.description}</p>{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>

	<div class="ai-card p-5">
		<div class="flex items-start justify-between gap-3">
			<div>
				<h2 class="text-sm font-semibold">MCP servers</h2>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">Attach only the servers this harness needs. Stdio commands are paths inside its image; HTTP and SSE servers can be remote.</p>
			</div>
			<button type="button" onclick={openNewMcp} class="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">+ Server</button>
		</div>

		{#if serverCatalog.length === 0 && !addingMcp}
			<div class="mt-4 rounded-md border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground">No MCP servers configured.</div>
		{:else}
			<div class="mt-4 space-y-2">
				{#each serverCatalog as server}
					<div class="rounded-md border border-border/70 px-3 py-2.5 hover:bg-accent/30" data-mcp-server={server.name}>
						<div class="flex items-start gap-3">
							<input type="checkbox" class="mt-0.5 h-4 w-4 rounded border-input" checked={selectedMcpServers.includes(server.name)} onchange={() => toggleMcp(server.name)} />
							<span class="min-w-0 flex-1">
								<span class="block text-sm font-medium">{server.name} <span class="ml-1 text-[10px] font-normal uppercase text-muted-foreground">{server.type}</span></span>
								<span class="block truncate font-mono text-[11px] text-muted-foreground">{server.command || server.url}</span>
							</span>
							<button type="button" onclick={() => verifyMcpServer(server)} disabled={verifyingMcp !== null} class="text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-50">
								{verifyingMcp === server.name ? 'Verifying…' : 'Verify'}
							</button>
							<button type="button" onclick={() => editMcpServer(server)} class="text-[11px] text-muted-foreground hover:text-foreground">Edit</button>
							<button type="button" onclick={() => deleteMcpServer(server.name)} class="text-[11px] {confirmDeleteMcp === server.name ? 'text-destructive' : 'text-muted-foreground hover:text-destructive'}">
								{confirmDeleteMcp === server.name ? 'Confirm' : 'Delete'}
							</button>
						</div>
						{#if mcpVerification[server.name]}
							{@const result = mcpVerification[server.name]}
							<p aria-live="polite" class="mt-2 rounded-md border px-3 py-2 text-[11px] leading-relaxed {verificationTone(result)}">
								{result.message}{#if result.suggestion} {result.suggestion}{/if}
							</p>
						{/if}
					</div>
				{/each}
			</div>
		{/if}

		{#if addingMcp}
			<div class="mt-4 space-y-3 rounded-lg border border-border bg-background/50 p-3">
				<div class="grid gap-3 sm:grid-cols-3">
					<input bind:value={mcpName} disabled={editingMcpName !== null} placeholder="Server name" class="rounded-md border border-input bg-background px-3 py-2 text-sm disabled:opacity-60" />
					<select bind:value={mcpType} class="rounded-md border border-input bg-background px-3 py-2 text-sm"><option value="stdio">stdio</option><option value="http">HTTP</option><option value="sse">SSE</option></select>
					<input bind:value={mcpCommandOrUrl} placeholder={mcpType === 'stdio' ? '/absolute/path/in/container' : 'https://server.example/mcp'} class="rounded-md border border-input bg-background px-3 py-2 font-mono text-xs" />
				</div>
				{#if mcpType === 'stdio'}
					<textarea bind:value={mcpArgs} rows="3" placeholder="One argument per line" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea>
					<textarea bind:value={mcpEnvironment} rows="3" placeholder={'Environment, one KEY=value per line\nAPI_TOKEN=...'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea>
				{:else}
					<textarea bind:value={mcpHeaders} rows="3" placeholder={'HTTP headers, one Name=value per line\nAuthorization=Bearer ...'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea>
				{/if}
				<p class="text-[11px] text-muted-foreground">Values are stored in the local XpressClaw configuration and supplied only to harnesses that enable this server.</p>
				{#if mcpError}<p class="text-xs text-destructive">{mcpError}</p>{/if}
				<div class="flex justify-end gap-2"><button type="button" onclick={resetMcpForm} class="rounded-md border border-border px-3 py-1.5 text-xs">Cancel</button><button type="button" onclick={saveMcpServer} disabled={!mcpName.trim() || !mcpCommandOrUrl.trim()} class="rounded-md bg-primary px-3 py-1.5 text-xs text-primary-foreground disabled:opacity-40">{editingMcpName ? 'Save' : 'Add and enable'}</button></div>
			</div>
		{/if}
	</div>

	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">Skills, plugins, hooks, and native configuration</h2>
		<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
			With host login enabled, the harness's normal configuration directory is mounted read-write, so its installed skills, plugins, hooks, custom agents, and settings load normally. Project-local configuration is loaded from the workspace. Add explicit mounts for any other configuration directories.
		</p>
		<label for="customization-volumes" class="mt-4 mb-1 block text-xs font-medium text-muted-foreground">Additional mounts</label>
		<textarea id="customization-volumes" bind:value={volumesText} rows="4" placeholder={'~/my-claude-plugin:/home/node/.claude/plugins/my-plugin:ro,z\n~/shared-skills:/home/node/.codex/skills:ro'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
		<p class="mt-1 text-[11px] text-muted-foreground">One <code>host-path:container-path[:options]</code> mount per line. Options are <code>ro</code>, <code>rw</code>, <code>z</code> (shared SELinux label), or <code>Z</code> (private label), separated by commas. These are trusted harness inputs and may execute code through hooks or plugins.</p>

		<label for="harness-environment" class="mt-4 mb-1 block text-xs font-medium text-muted-foreground">Harness environment</label>
		<textarea id="harness-environment" bind:value={environmentText} rows="4" placeholder={'CODEX_CONFIG={"features":{"example":true}}\nCLAUDE_CONFIG_DIR=/home/node/.claude'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
		<p class="mt-1 text-[11px] text-muted-foreground">One <code>NAME=value</code> entry per line. Values are stored in the local XpressClaw configuration.</p>

		<label for="startup-commands" class="mt-4 mb-1 block text-xs font-medium text-muted-foreground">Workspace startup commands</label>
		<textarea id="startup-commands" bind:value={startupCommandsText} rows="4" placeholder={'npm ci\ndocker compose up -d'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
		<p class="mt-1 text-[11px] text-muted-foreground">One idempotent shell command per line. Commands run in the workspace before every short-lived ACP task.</p>
	</div>

	<div class="ai-card p-5">
		<div class="flex items-start gap-3">
			<input id="ssh-agent-forwarding" type="checkbox" bind:checked={sshAgentForwarding} class="mt-0.5 h-4 w-4 rounded border-input" />
			<div>
				<label for="ssh-agent-forwarding" class="text-sm font-medium">Use my host SSH agent</label>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					Forward the host SSH-agent socket so Git can use keys you already unlocked for an existing clone. XpressClaw also exposes the host SSH config and known-host entries when present, but never mounts private-key files.
				</p>
				{#if sshAgentAvailable}
					<p class="mt-2 text-[11px] text-muted-foreground">Detected <code>{sshAgentSocket}</code>.</p>
				{:else}
					<p class="mt-2 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
						No live host SSH agent was detected. Start <code>ssh-agent</code>, load a key with <code>ssh-add</code>, then restart XpressClaw from that desktop session.
					</p>
				{/if}
				{#if sshAgentForwarding}
					<p class="mt-2 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
						The harness can authenticate or sign with any key loaded in that SSH agent for the life of this retained environment. Enable this only for harnesses and tasks you trust.
					</p>
				{/if}
			</div>
		</div>
	</div>

	<div class="ai-card p-5">
		<div class="flex items-start gap-3">
			<input
				id="container-engine"
				type="checkbox"
				checked={containerEngine === 'host'}
				onchange={(event) => setContainerEngine(event.currentTarget.checked)}
				class="mt-0.5 h-4 w-4 rounded border-input"
			/>
			<div>
				<label for="container-engine" class="text-sm font-medium">Host Docker or Podman access</label>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					Mount the control plane's Docker-compatible socket and use a runner variant containing Docker CLI, Compose, and Buildx.
				</p>
				{#if containerEngine === 'host'}
					<p class="mt-2 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
						This grants the harness control of host containers, images, volumes, and any paths the engine can mount. Use only with harnesses and images you trust.
					</p>
				{/if}
			</div>
		</div>
	</div>

	<div class="ai-card p-5">
		<div class="flex items-start gap-3">
			<input id="subscription-auth" type="checkbox" bind:checked={subscriptionAuth} disabled={kind === 'custom'} class="mt-0.5 h-4 w-4 rounded border-input disabled:opacity-50" />
			<div>
				<label for="subscription-auth" class="text-sm font-medium">Use host login and harness configuration</label>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					{kind === 'custom'
						? 'Custom harnesses manage their own authentication; add any required directories in the mounts above.'
						: "Mount the selected harness's standard configuration directory into each worker. This includes its subscription login plus installed skills, plugins, hooks, custom agents, and user settings. Git SSH authentication is configured separately above."}
				</p>
				{#if subscriptionAuth}
					<p class="mt-2 rounded-md border border-amber-500/20 bg-amber-500/10 px-3 py-2 text-xs text-amber-600">
						The worker can refresh the mounted OAuth credentials. Use trusted images only.
					</p>
				{/if}
			</div>
		</div>
	</div>

	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">ACP server command</h2>
		<p class="mt-1 text-xs text-muted-foreground">{kind === 'custom' ? 'Required for custom harnesses.' : 'Optional override for the built-in adapter.'} Enter one argument per line. Available placeholder: <code>{'{workspace}'}</code>.</p>
		<textarea bind:value={commandText} rows="7" placeholder={'my-agent\nacp\n--cwd\n{workspace}'} class="mt-3 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
	</div>
</div>

{#if showWorkspacePicker}
	<DirectoryPicker
		title="Choose workspace folder"
		initialPath={workspace}
		onclose={() => (showWorkspacePicker = false)}
		onselect={(path) => {
			workspace = path;
			showWorkspacePicker = false;
		}}
	/>
{/if}
