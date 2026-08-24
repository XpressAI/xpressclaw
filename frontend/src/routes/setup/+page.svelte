<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { setup } from '$lib/api';
	import { openExternal } from '$lib/utils';
	import DirectoryPicker from '$lib/components/DirectoryPicker.svelte';
	import type { AcpAgentCatalogEntry, DockerStatus, ProjectEnvironmentSuggestion } from '$lib/api';

	const customRunner: AcpAgentCatalogEntry = {
		kind: 'custom',
		name: 'Other ACP harness',
		mark: '+',
		description: 'Any containerized coding harness that speaks ACP over stdio.',
		command: [],
		login_command: '',
		install_url: '',
		image: '',
		host_image: '',
		installed: false,
		configured: false,
		status: 'not_installed',
		executable: null
	};

	let isAddSession = $derived(
		['add-session', 'add-agent'].includes($page.url.searchParams.get('mode') ?? '')
	);
	let targetProjectId = $derived($page.url.searchParams.get('project_id')?.trim() || '');
	let cancelPath = $derived(targetProjectId ? `/projects/${encodeURIComponent(targetProjectId)}` : '/agents');
	let agentCatalog = $state<AcpAgentCatalogEntry[]>([]);
	let agentCatalogLoading = $state(true);
	let runnerOptions = $derived([...agentCatalog, customRunner]);
	let runnerKind = $state('codex');
	let runnerImage = $state('ghcr.io/xpressai/xpressclaw-runner-codex:latest');
	let runnerModel = $state('');
	let runnerCommand = $state('');
	let subscriptionAuth = $state(true);
	let sshAgentForwarding = $state(false);
	let sshAgentAvailable = $state(false);
	let sshAgentSocket = $state('');
	let containerEngine = $state<'none' | 'host'>('none');
	let workspaceMode = $state<'existing' | 'managed'>('existing');
	let workspacePath = $state('');
	let projectName = $state('');
	let hostOs = $state('');
	let workspaceFolders = $state<string[]>([]);
	let newFolderPath = $state('');
	let folderPicker = $state<'workspace' | 'additional' | null>(null);
	let environmentSuggestions = $state<ProjectEnvironmentSuggestion[]>([]);
	let environmentLoading = $state(false);
	let inspectedWorkspace = $state('');
	let gitUsesSsh = $state(false);
	let startupCommandText = $state('');
	let dockerStatus = $state<DockerStatus | null>(null);
	let dockerLoading = $state(true);
	let saving = $state(false);
	let saveError = $state('');
	let lastSuggestedAgentName = '';
	let contextLabel = $derived(projectName.trim()
		|| (workspaceMode === 'managed'
			? 'New agent'
			: workspacePath.split(/[\\/]/).filter(Boolean).pop() || runnerKind));

	onMount(async () => {
		// Keep old bookmarks working while making the native-session URL canonical.
		if ($page.url.searchParams.get('mode') === 'add-agent') {
			await goto('/setup?mode=add-session', { replaceState: true });
		}

		const [systemResult, catalogResult] = await Promise.allSettled([
			setup.systemInfo(),
			setup.agentCatalog()
		]);
		if (systemResult.status === 'fulfilled') {
			workspacePath = systemResult.value.working_directory ?? '';
			hostOs = systemResult.value.os;
			sshAgentAvailable = systemResult.value.ssh_agent_available;
			sshAgentSocket = systemResult.value.ssh_agent_socket ?? '';
			suggestAgentName(workspacePath);
		}
		if (catalogResult.status === 'fulfilled') {
			agentCatalog = catalogResult.value.agents;
			const selected = agentCatalog.find((agent) => agent.kind === runnerKind);
			if (selected) runnerImage = selected.image;
		}
		agentCatalogLoading = false;
		if (workspacePath) await inspectEnvironment();
		await recheckDocker();
	});

	function suggestedAgentName(path: string): string {
		const folder = path.split(/[\\/]/).filter(Boolean).pop();
		return folder ? `${folder}-agent` : 'new-agent';
	}

	function suggestAgentName(path: string) {
		const suggestion = suggestedAgentName(path);
		if (!projectName.trim() || projectName === lastSuggestedAgentName) projectName = suggestion;
		lastSuggestedAgentName = suggestion;
	}

	function selectRunner(kind: string) {
		const runner = runnerOptions.find((option) => option.kind === kind);
		if (!runner) return;
		runnerKind = runner.kind;
		runnerImage = containerEngine === 'host' ? runner.host_image : runner.image;
		runnerCommand = '';
		runnerModel = '';
		subscriptionAuth = runner.kind !== 'custom';
	}

	function setContainerEngine(enabled: boolean) {
		const defaults = new Set<string>([
			...runnerOptions.flatMap((runner) => [
				runner.image,
				runner.host_image,
				runner.image.replace('ghcr.io/xpressai/', ''),
				runner.host_image.replace('ghcr.io/xpressai/', '')
			])
		]);
		const replaceImage = !runnerImage.trim() || defaults.has(runnerImage.trim());
		containerEngine = enabled ? 'host' : 'none';
		const runner = runnerOptions.find((option) => option.kind === runnerKind);
		if (runner && runner.kind !== 'custom' && replaceImage) {
			runnerImage = containerEngine === 'host' ? runner.host_image : runner.image;
		}
	}

	async function copyLoginCommand(command: string) {
		try {
			await navigator.clipboard.writeText(command);
		} catch {
			// The command remains visible and selectable if clipboard
			// permissions are unavailable.
		}
	}

	function startupCommands(): string[] {
		return startupCommandText.split('\n').map((line) => line.trim()).filter(Boolean);
	}

	function toggleStartupCommand(suggestion: ProjectEnvironmentSuggestion, enabled: boolean) {
		if (!suggestion.command) return;
		let commands = startupCommands().filter((command) => command !== suggestion.command);
		if (enabled) {
			commands = [...commands, suggestion.command];
			if (suggestion.requires_host_engine && containerEngine !== 'host') {
				setContainerEngine(true);
			}
		}
		startupCommandText = commands.join('\n');
	}

	async function inspectEnvironment() {
		const path = workspacePath.trim();
		if (!path || environmentLoading) return;
		environmentLoading = true;
		try {
			const result = await setup.projectEnvironment(path);
			environmentSuggestions = result.suggestions;
			inspectedWorkspace = result.workspace;
			gitUsesSsh = result.git_uses_ssh;
		} catch {
			environmentSuggestions = [];
			inspectedWorkspace = path;
			gitUsesSsh = false;
		} finally {
			environmentLoading = false;
		}
	}

	function addFolder() {
		const folder = newFolderPath.trim();
		if (!folder || workspaceFolders.includes(folder)) return;
		workspaceFolders = [...workspaceFolders, folder];
		newFolderPath = '';
	}

	async function recheckDocker() {
		dockerLoading = true;
		try {
			dockerStatus = await setup.checkDocker();
		} catch {
			dockerStatus = {
				available: false,
				installed: false,
				can_start: false,
				runtime: null,
				version: null,
				socket: null,
				rootless: null,
				error: 'Could not check the container runtime'
			};
		}
		dockerLoading = false;
	}

	function additionalVolumes(): string[] {
		return workspaceFolders.map((folder) => {
			if (containerEngine === 'host' && hostOs !== 'windows') return `${folder}:${folder}:z`;
			const basename = folder.split(/[\\/]/).filter(Boolean).pop() || 'shared';
			return `${folder}:/workspace/${basename}:z`;
		});
	}

	async function createSession() {
		if (!projectName.trim() || (workspaceMode === 'existing' && !workspacePath.trim()) || !runnerImage.trim() || (runnerKind === 'custom' && !runnerCommand.trim()) || saving) return;
		saving = true;
		saveError = '';

		const session = {
			project_id: targetProjectId || undefined,
			backend: runnerKind,
			runner_kind: runnerKind,
			runner_image: runnerImage.trim(),
			runner_workspace: workspaceMode === 'existing' ? workspacePath.trim() : undefined,
			workspace_mode: workspaceMode,
			project_name: projectName.trim(),
			runner_model: runnerModel.trim() || undefined,
			runner_command: runnerCommand.split('\n').map((line) => line.trim()).filter(Boolean),
			startup_commands: startupCommands(),
			subscription_auth: subscriptionAuth,
			ssh_agent_forwarding: sshAgentForwarding,
			runner_container_engine: containerEngine,
			volumes: additionalVolumes()
		};

		try {
			if (isAddSession) {
				const created = await setup.addSession(session);
				await goto(`/agents/${created.session_id}`);
			} else {
				await setup.complete({
					agents: [session],
					isolation: 'docker'
				});
				await goto('/');
			}
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Could not create the agent';
		} finally {
			saving = false;
		}
	}
</script>

<div class="rounded-2xl border border-border bg-card shadow-sm">
	<div class="flex items-start justify-between border-b border-border px-4 py-5 sm:px-6">
		<div>
			<p class="text-xs font-medium uppercase tracking-wider text-primary">{isAddSession ? 'Agent' : 'Project & Agent'}</p>
			<h2 class="mt-1 text-xl font-semibold text-foreground">
				{isAddSession ? 'Create an agent' : 'Create your first Project and Agent'}
			</h2>
			<p class="mt-1 text-sm text-muted-foreground">
				{isAddSession
					? 'Name its durable context, choose a coding harness, and point it at a workspace.'
					: 'Choose a repository or managed workspace and an Agent to begin. You can add more to this instance later.'}
			</p>
		</div>
		{#if isAddSession}
			<button
				type="button"
				onclick={() => goto(cancelPath)}
				aria-label="Cancel"
				class="ml-4 rounded-md p-2 text-xl leading-none text-muted-foreground hover:bg-accent hover:text-foreground"
			>&times;</button>
		{/if}
	</div>

	<form class="space-y-7 p-4 sm:p-6" onsubmit={(event) => { event.preventDefault(); createSession(); }}>
		<section>
			<div class="mb-3">
				<h3 class="text-sm font-medium text-foreground">Harness</h3>
				<p class="mt-0.5 text-xs text-muted-foreground">Detected tools are shown first. Sign in on this computer once; XpressClaw reuses that login inside the isolated ACP runner.</p>
			</div>
			{#if agentCatalogLoading}
				<div class="mb-3 flex items-center gap-2 text-xs text-muted-foreground">
					<span class="h-3.5 w-3.5 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
					Detecting installed harnesses...
				</div>
			{/if}
			<div class="grid gap-3 sm:grid-cols-2">
				{#each runnerOptions as runner}
					<div
						class="rounded-xl border transition-colors {runnerKind === runner.kind
							? 'border-primary bg-primary/5 ring-1 ring-primary/20'
							: 'border-border hover:border-primary/40 hover:bg-accent/30'}"
					>
						<button type="button" onclick={() => selectRunner(runner.kind)} class="w-full p-4 text-left">
							<span class="flex items-start justify-between gap-3">
								<span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-muted text-xs font-semibold text-foreground">{runner.mark}</span>
								{#if runner.kind !== 'custom'}
									<span class="rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide {runner.status === 'ready'
										? 'bg-emerald-500/15 text-emerald-600'
										: runner.status === 'sign_in'
											? 'bg-amber-500/15 text-amber-600'
											: 'bg-muted text-muted-foreground'}">
										{runner.status === 'ready' ? 'Ready' : runner.status === 'sign_in' ? 'Sign in' : 'Not installed'}
									</span>
								{/if}
							</span>
							<span class="mt-3 block text-sm font-medium text-foreground">{runner.name}</span>
							<span class="mt-1 block text-xs leading-relaxed text-muted-foreground">{runner.description}</span>
						</button>
						{#if runner.kind !== 'custom' && runner.status !== 'ready'}
							<div class="flex items-center justify-between gap-2 border-t border-border/70 px-4 py-2.5">
								{#if runner.status === 'sign_in'}
									<code class="min-w-0 flex-1 truncate text-[11px] text-muted-foreground">{runner.login_command}</code>
									<button type="button" onclick={() => copyLoginCommand(runner.login_command)} class="shrink-0 text-xs font-medium text-primary hover:underline">Copy</button>
								{:else}
									<span class="text-[11px] text-muted-foreground">Install and sign in on this computer</span>
									<button type="button" onclick={() => openExternal(runner.install_url)} class="shrink-0 text-xs font-medium text-primary hover:underline">Install</button>
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		</section>

		<section>
			<h3 class="mb-2 text-sm font-medium text-foreground">Agent context</h3>
			<label for="project-name" class="mb-1.5 block text-xs font-medium text-foreground">Agent name</label>
			<input
				id="project-name"
				type="text"
				bind:value={projectName}
				placeholder={workspaceMode === 'existing' ? 'Derived from workspace folder' : 'New agent'}
				class="mb-4 w-full rounded-lg border border-input bg-background px-3.5 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<h4 class="mb-2 text-xs font-medium text-foreground">Workspace</h4>
			<div class="mb-3 grid grid-cols-2 rounded-lg border border-border bg-muted/30 p-1">
				<button type="button" onclick={() => (workspaceMode = 'existing')} class="rounded-md px-3 py-2 text-xs font-medium {workspaceMode === 'existing' ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'}">Existing folder</button>
				<button type="button" onclick={() => (workspaceMode = 'managed')} class="rounded-md px-3 py-2 text-xs font-medium {workspaceMode === 'managed' ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'}">Start without a folder</button>
			</div>
			{#if workspaceMode === 'existing'}
				<label for="workspace-path" class="mb-1.5 block text-xs font-medium text-foreground">Repository or workspace folder</label>
				<div class="flex gap-2">
					<input
						id="workspace-path"
						type="text"
						bind:value={workspacePath}
						onchange={() => suggestAgentName(workspacePath)}
						placeholder="/home/me/projects/my-app"
						class="min-w-0 flex-1 rounded-lg border border-input bg-background px-3.5 py-2.5 font-mono text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					/>
					<button type="button" onclick={() => (folderPicker = 'workspace')} class="rounded-lg border border-border px-3.5 py-2.5 text-sm hover:bg-accent">Browse…</button>
				</div>
				<p class="mt-1.5 text-xs text-muted-foreground">
					This is the Agent's workspace, not another XpressClaw instance. The folder must exist on the control-plane machine.
					{containerEngine === 'host'
						? ' It is mounted at the same absolute path so Compose bind mounts resolve correctly.'
						: ' It is mounted at /workspace.'}
					This agent appears as <strong>{contextLabel}</strong> in the UI.
				</p>
				{#if gitUsesSsh}
					<p class="mt-2 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
						This repository has an SSH remote. Enable host SSH-agent access below if it is not covered by XpressClaw's scoped GitHub credential.
					</p>
				{/if}
			{:else}
				<p class="mt-1.5 text-xs leading-relaxed text-muted-foreground">
					XpressClaw creates an empty persistent workspace. Your first message can ask the agent's harness to clone a GitHub repository or create a project from scratch.
				</p>
			{/if}
		</section>

		{#if workspaceMode === 'existing'}
			<section class="rounded-xl border border-border bg-muted/20 p-4">
				<div class="flex items-start justify-between gap-3">
					<div>
						<h3 class="text-sm font-medium text-foreground">Environment setup</h3>
						<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">XpressClaw detects standard project manifests and suggests optional setup commands. Nothing runs unless you select it.</p>
					</div>
					<button type="button" onclick={inspectEnvironment} disabled={!workspacePath.trim() || environmentLoading} class="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent disabled:opacity-50">
						{environmentLoading ? 'Inspecting…' : inspectedWorkspace === workspacePath ? 'Rescan' : 'Inspect'}
					</button>
				</div>
				{#if environmentSuggestions.length > 0}
					<div class="mt-3 space-y-2">
						{#each environmentSuggestions as suggestion}
							<label class="flex items-start gap-3 rounded-lg border border-border bg-background px-3 py-2.5 {suggestion.command ? 'cursor-pointer' : ''}">
								<input
									type="checkbox"
									class="mt-0.5 rounded border-border"
									disabled={!suggestion.command}
									checked={Boolean(suggestion.command && startupCommands().includes(suggestion.command))}
									onchange={(event) => toggleStartupCommand(suggestion, event.currentTarget.checked)}
								/>
								<span class="min-w-0 flex-1">
									<span class="flex flex-wrap items-center gap-2 text-xs font-medium text-foreground">
										{suggestion.name}
										<code class="font-normal text-muted-foreground">{suggestion.detected_file}</code>
										{#if suggestion.requires_host_engine}<span class="rounded-full bg-amber-500/15 px-1.5 py-0.5 text-[9px] uppercase text-amber-600">Host engine</span>{/if}
									</span>
									<span class="mt-0.5 block text-[11px] leading-relaxed text-muted-foreground">{suggestion.description}</span>
									{#if suggestion.command}<code class="mt-1 block truncate text-[11px] text-primary">{suggestion.command}</code>{/if}
								</span>
							</label>
						{/each}
					</div>
					<p class="mt-2 text-[11px] text-muted-foreground">Selected commands run in the workspace before every short-lived ACP task. Keep them idempotent.</p>
				{:else if inspectedWorkspace && !environmentLoading}
					<p class="mt-3 rounded-lg border border-dashed border-border px-3 py-3 text-center text-xs text-muted-foreground">No supported environment manifests were detected.</p>
				{/if}
			</section>
		{/if}

		<section class="rounded-xl border border-border bg-muted/20 p-4">
			<label class="flex cursor-pointer items-start gap-3">
				<input
					type="checkbox"
					checked={containerEngine === 'host'}
					onchange={(event) => setContainerEngine(event.currentTarget.checked)}
					class="mt-0.5 rounded border-border"
				/>
				<span>
					<span class="block text-sm font-medium text-foreground">Give the harness host Docker or Podman access</span>
					<span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">Use Docker Compose, Buildx, and the host engine's image cache from inside the runner.</span>
				</span>
			</label>
			{#if containerEngine === 'host'}
				<p class="mt-3 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
					The harness can control host containers, images, volumes, and any paths the engine can mount. Enable this only for harnesses and images you trust.
				</p>
			{/if}
		</section>

		<section class="rounded-xl border border-border bg-muted/20 p-4">
			<label class="flex cursor-pointer items-start gap-3">
				<input type="checkbox" bind:checked={sshAgentForwarding} class="mt-0.5 rounded border-border" />
				<span>
					<span class="block text-sm font-medium text-foreground">Use my host SSH agent</span>
					<span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">Let Git use keys already unlocked on this computer. XpressClaw forwards only the agent socket plus SSH config and known-host entries; it never mounts private-key files.</span>
				</span>
			</label>
			{#if sshAgentAvailable}
				<p class="mt-2 text-[11px] text-muted-foreground">Detected <code>{sshAgentSocket}</code>.</p>
			{:else}
				<p class="mt-3 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
					No live host SSH agent was detected. Start <code>ssh-agent</code>, load a key with <code>ssh-add</code>, then restart XpressClaw from that desktop session.
				</p>
			{/if}
			{#if sshAgentForwarding}
				<p class="mt-3 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
					The harness can authenticate or sign with any key loaded in your SSH agent. Enable this only for harnesses and tasks you trust.
				</p>
			{/if}
		</section>

		<section class="rounded-xl border border-border bg-muted/20 p-4">
			{#if runnerKind === 'custom'}
				<div>
					<p class="text-sm font-medium text-foreground">Authentication is image-defined</p>
					<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">Add any required credential directories as mounts below. XpressClaw only knows the standard login locations for its built-in harnesses.</p>
				</div>
			{:else}
			<label class="flex cursor-pointer items-start gap-3">
				<input type="checkbox" bind:checked={subscriptionAuth} class="mt-0.5 rounded border-border" />
				<span>
					<span class="block text-sm font-medium text-foreground">Use my existing {runnerOptions.find((runner) => runner.kind === runnerKind)?.name} login</span>
					<span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">Mount the harness's standard login directory so its subscription and native sessions can continue across tasks. Only enable this for images you trust.</span>
				</span>
			</label>
			{/if}
		</section>

		<details class="group rounded-xl border border-border">
			<summary class="cursor-pointer list-none px-4 py-3 text-sm font-medium text-foreground">
				<span class="inline-flex items-center gap-2">
					<span class="text-muted-foreground transition-transform group-open:rotate-90">&#9656;</span>
					Advanced container options
				</span>
			</summary>
			<div class="space-y-5 border-t border-border px-4 py-4">
				<div>
					<label for="runner-image" class="mb-1 block text-xs font-medium text-foreground">Runner image</label>
					<input
						id="runner-image"
						type="text"
						bind:value={runnerImage}
						class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
					/>
					<p class="mt-1 text-xs text-muted-foreground">{containerEngine === 'host' ? 'The built-in Docker variant adds Docker CLI, Compose, and Buildx.' : 'Use the minimal harness image or a compatible derivative.'}</p>
				</div>

				<div>
					<label for="runner-model" class="mb-1 block text-xs font-medium text-foreground">Model</label>
					<input
						id="runner-model"
						type="text"
						bind:value={runnerModel}
						placeholder="Harness default"
						class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
					/>
					<p class="mt-1 text-xs text-muted-foreground">Optional ACP model value ID. Invalid values are rejected with the choices advertised by the harness.</p>
				</div>

				<div>
					<label for="runner-command" class="mb-1 block text-xs font-medium text-foreground">ACP server command {runnerKind === 'custom' ? '(required)' : '(optional override)'}</label>
					<textarea
						id="runner-command"
						bind:value={runnerCommand}
						rows="4"
						placeholder={'my-agent\nacp\n--cwd\n{workspace}'}
						class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
					></textarea>
					<p class="mt-1 text-xs text-muted-foreground">One argument per line. The process must speak ACP over stdin/stdout. Available placeholder: <code>{'{workspace}'}</code>.</p>
				</div>

				<div>
					<label for="startup-commands" class="mb-1 block text-xs font-medium text-foreground">Workspace startup commands</label>
					<textarea
						id="startup-commands"
						bind:value={startupCommandText}
						rows="3"
						placeholder={'npm ci\ndocker compose up -d'}
						class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
					></textarea>
					<p class="mt-1 text-xs text-muted-foreground">One shell command per line, run before each ACP task. Commands have the same workspace and container permissions as the harness.</p>
				</div>

				<div>
					<label for="additional-folder" class="mb-1 block text-xs font-medium text-foreground">Additional folder mounts</label>
					{#if workspaceFolders.length > 0}
						<div class="mb-2 space-y-2">
							{#each workspaceFolders as folder, index}
								<div class="flex items-center gap-2 rounded-md border border-border bg-background px-3 py-2">
									<span class="min-w-0 flex-1 truncate font-mono text-xs">{folder}</span>
									<button
										type="button"
										onclick={() => { workspaceFolders = workspaceFolders.filter((_, itemIndex) => itemIndex !== index); }}
										aria-label={`Remove ${folder}`}
										class="text-muted-foreground hover:text-destructive"
									>&times;</button>
								</div>
							{/each}
						</div>
					{/if}
					<div class="flex gap-2">
						<input
							id="additional-folder"
							type="text"
							bind:value={newFolderPath}
							onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); addFolder(); } }}
							placeholder="/home/me/projects/shared"
							class="min-w-0 flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
						/>
						<button type="button" onclick={addFolder} disabled={!newFolderPath.trim()}
							class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50">Add</button>
						<button type="button" onclick={() => (folderPicker = 'additional')}
							class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent">Browse…</button>
					</div>
				</div>
			</div>
		</details>

		<section>
			{#if dockerLoading}
				<div class="flex items-center gap-3 rounded-lg border border-border px-4 py-3">
					<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
					<span class="text-xs text-muted-foreground">Checking Docker or Podman...</span>
				</div>
			{:else if dockerStatus?.available}
				<div class="flex items-center gap-3 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-4 py-3">
					<span class="flex h-6 w-6 items-center justify-center rounded-full bg-emerald-500/15 text-xs text-emerald-500">&#10003;</span>
					<div class="min-w-0">
						<p class="text-sm font-medium text-foreground">{dockerStatus.runtime === 'podman' ? 'Podman' : 'Docker'} ready</p>
						<p class="truncate text-xs text-muted-foreground">
							{dockerStatus.rootless ? 'Rootless · ' : ''}{dockerStatus.socket ?? 'Automatic endpoint'}{dockerStatus.version ? ` · ${dockerStatus.version}` : ''}
						</p>
					</div>
				</div>
			{:else}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 px-4 py-3">
					<div class="flex items-start justify-between gap-4">
						<div>
							<p class="text-sm font-medium text-foreground">Container runtime not available</p>
							<p class="mt-0.5 text-xs text-muted-foreground">You can save the agent, but work will wait until Docker or Podman is running.</p>
						</div>
						<button type="button" onclick={recheckDocker} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent">Retry</button>
					</div>
					<div class="mt-3 flex gap-3 text-xs">
						<button type="button" onclick={() => openExternal('https://docs.docker.com/get-docker/')} class="text-primary hover:underline">Docker setup</button>
						<button type="button" onclick={() => openExternal('https://podman.io/getting-started/installation')} class="text-primary hover:underline">Podman setup</button>
					</div>
				</div>
			{/if}
		</section>

		{#if saveError}
			<p class="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">{saveError}</p>
		{/if}

		<div class="flex items-center justify-between border-t border-border pt-5">
			{#if isAddSession}
				<button type="button" onclick={() => goto(cancelPath)} class="rounded-lg px-4 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-foreground">Cancel</button>
			{:else}
				<span class="text-xs text-muted-foreground">No API key required when using a subscription login.</span>
			{/if}
			<button
				type="submit"
				disabled={saving || !projectName.trim() || (workspaceMode === 'existing' && !workspacePath.trim()) || !runnerImage.trim() || (runnerKind === 'custom' && !runnerCommand.trim())}
				class="rounded-lg bg-primary px-5 py-2.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
			>
				{saving ? 'Creating...' : (isAddSession ? 'Create agent' : 'Finish setup')}
			</button>
		</div>
	</form>
</div>

{#if folderPicker}
	<DirectoryPicker
		title={folderPicker === 'workspace' ? 'Choose workspace folder' : 'Choose additional folder'}
		initialPath={folderPicker === 'workspace' ? workspacePath : newFolderPath || workspacePath}
		onclose={() => (folderPicker = null)}
		onselect={(path) => {
			if (folderPicker === 'workspace') {
				workspacePath = path;
				suggestAgentName(path);
				void inspectEnvironment();
			} else {
				newFolderPath = path;
				addFolder();
			}
			folderPicker = null;
		}}
	/>
{/if}
