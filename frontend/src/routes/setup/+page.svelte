<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { setup } from '$lib/api';
	import { openExternal } from '$lib/utils';
	import type { DockerStatus, AgentPreset } from '$lib/api';

	// Flow: 0=session profile, 1=native runner, 2=workspace, 3=environment, 4=complete
	let step = $state(0);
	const steps = ['Session', 'Runner', 'Workspace', 'Environment', 'Complete'];

	// Mode: 'setup' (full onboarding) or 'add-agent' (from agents page)
	let mode = $derived($page.url.searchParams.get('mode') === 'add-agent' ? 'add-agent' : 'setup');

	// -- Step 0: Agent --
	let presets = $state<AgentPreset[]>([]);
	let agentName = $state('');
	let selectedPreset = $state<AgentPreset | null>(null);
	let customRole = $state('');
	let agentRoleTitle = $state('');
	let agentResponsibilities = $state('');

	// -- Step 1: Native runner --
	let llmProvider = $state('codex');
	let runnerImage = $state('xpressclaw-runner-codex:latest');
	let subscriptionAuth = $state(true);
	const runnerImages: Record<string, string> = {
		codex: 'xpressclaw-runner-codex:latest',
		claude: 'xpressclaw-runner-claude:latest',
		opencode: 'xpressclaw-runner-opencode:latest',
		custom: ''
	};

	// -- Step 2: Workspace resources --
	// Workspace folders to mount into /workspace/{basename}
	let workspaceFolders = $state<string[]>([]);
	let newFolderPath = $state('');
	let composingFolder = $state(false);

	// -- Step 3: Docker --
	let dockerStatus = $state<DockerStatus | null>(null);
	let dockerLoading = $state(true);

	// -- Step 4: Complete --
	let saving = $state(false);
	let saveError = $state('');

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

	});

	function selectPreset(preset: AgentPreset) {
		selectedPreset = preset;
		if (!agentName) agentName = preset.id;
		customRole = preset.role;

		if (preset.backend.includes('claude')) selectRunner('claude');
	}

	function selectRunner(kind: string) {
		llmProvider = kind;
		runnerImage = runnerImages[kind] ?? '';
	}


	async function goToStep(target: number) {
		if (target === 3) recheckDocker();
		step = target;
	}

	async function recheckDocker() {
		dockerLoading = true;
		try { dockerStatus = await setup.checkDocker(); } catch {
			dockerStatus = { available: false, error: 'Failed to check' };
		}
		dockerLoading = false;
	}

	function canProceedLlm(): boolean {
		return ['codex', 'claude', 'opencode', 'custom'].includes(llmProvider) && !!runnerImage.trim();
	}

	async function completeSetup() {
		saving = true;
		saveError = '';
		try {
			// Build volumes from workspace folders
			const volumes = workspaceFolders
				.filter(f => f.trim())
				.map(f => {
					const basename = f.trim().split('/').filter(Boolean).pop() || 'workspace';
					return `${f.trim()}:/workspace/${basename}`;
				});

			if (mode === 'add-agent') {
				// Add agent to existing config without replacing other agents
				await setup.addAgent({
					name: agentName,
					preset: selectedPreset?.id,
					role: customRole || undefined,
					backend: llmProvider,
					runner_kind: llmProvider,
					runner_image: runnerImage,
					subscription_auth: subscriptionAuth,
					volumes: volumes.length > 0 ? volumes : undefined,
				});
				step = 4;
			} else {
				// Full setup: replace entire config
				await setup.complete({
					llm: {
						provider: '',
					},
					agents: [{
						name: agentName,
						preset: selectedPreset?.id,
						role: customRole || undefined,
						role_title: agentRoleTitle || undefined,
						responsibilities: agentResponsibilities || undefined,
						backend: llmProvider,
						runner_kind: llmProvider,
						runner_image: runnerImage,
						subscription_auth: subscriptionAuth,
						volumes: volumes.length > 0 ? volumes : undefined,
					}],
					isolation: 'docker'
				});
				step = 4;
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
					{mode === 'add-agent' ? 'Add Session' : 'Choose a Session Profile'}
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
					</div>
				</button>
			{/each}
		</div>

		{#if selectedPreset}
			<div class="space-y-3 rounded-lg border border-border p-4">
				<div>
					<label for="agent-name" class="block text-xs font-medium text-foreground mb-1">Session Name</label>
					<input
						id="agent-name"
						type="text"
						bind:value={agentName}
						placeholder="atlas"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label for="agent-role-title" class="block text-xs font-medium text-foreground mb-1">
						Role Title <span class="text-muted-foreground font-normal">(e.g. Personal Assistant, Code Reviewer)</span>
					</label>
					<input
						id="agent-role-title"
						type="text"
						bind:value={agentRoleTitle}
						placeholder="e.g. Developer, Personal Assistant"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label for="agent-responsibilities" class="block text-xs font-medium text-foreground mb-1">
						Responsibilities <span class="text-muted-foreground font-normal">(what should its native runner do?)</span>
					</label>
					<textarea
						id="agent-responsibilities"
						bind:value={agentResponsibilities}
						rows="2"
						placeholder="e.g. Manages code reviews, writes tests, fixes bugs"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					></textarea>
				</div>
				<div>
					<label for="agent-role" class="block text-xs font-medium text-foreground mb-1">
						System Prompt <span class="text-muted-foreground font-normal">(advanced)</span>
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

	<!-- Step 1: Native Runner -->
	{:else if step === 1}
		<h2 class="text-lg font-semibold text-foreground mb-1">Native Runner</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Choose the agent product that will own the reasoning loop for this session.
		</p>

		<div class="space-y-2 mb-4">
				<button
					onclick={() => selectRunner('codex')}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'codex' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">C</div>
					<div class="flex-1">
						<div class="text-sm font-medium text-foreground">Codex</div>
						<div class="text-xs text-muted-foreground">Use Codex CLI with your eligible ChatGPT subscription login.</div>
					</div>
				</button>
				<button
					onclick={() => selectRunner('claude')}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'claude' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">A</div>
					<div>
						<div class="text-sm font-medium text-foreground">Claude Code</div>
						<div class="text-xs text-muted-foreground">Use Claude Code with your eligible Claude subscription login.</div>
					</div>
				</button>
				<button
					onclick={() => selectRunner('opencode')}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'opencode' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">O</div>
					<div>
						<div class="text-sm font-medium text-foreground">OpenCode</div>
						<div class="text-xs text-muted-foreground">Use OpenCode's native runner and JSON event stream.</div>
					</div>
				</button>
				<button
					onclick={() => selectRunner('custom')}
					class="w-full flex items-start gap-3 rounded-lg border p-3 text-left transition-colors {llmProvider === 'custom' ? 'border-primary bg-primary/5' : 'border-border hover:border-primary/40'}"
				>
					<div class="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-sm">+</div>
					<div>
						<div class="text-sm font-medium text-foreground">Custom CLI</div>
						<div class="text-xs text-muted-foreground">Supply a compatible image and configure its command after setup.</div>
					</div>
				</button>
		</div>

		<div class="mt-4 space-y-4 rounded-lg border border-border p-4">
			<div>
				<label for="runner-image" class="block text-xs font-medium text-foreground mb-1">Worker image</label>
				<input id="runner-image" type="text" bind:value={runnerImage}
					class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
				<p class="mt-1 text-xs text-muted-foreground">Each built-in image contains only the selected native CLI. Extend it when the runner itself needs additional tools.</p>
			</div>
			<label class="flex items-start gap-3 cursor-pointer">
				<input type="checkbox" bind:checked={subscriptionAuth} class="mt-0.5 rounded border-border" />
				<div>
					<div class="text-sm font-medium text-foreground">Use host subscription login</div>
					<div class="text-xs text-muted-foreground">Reuse the selected CLI's standard login directory inside short-lived workers. Use trusted images only.</div>
				</div>
			</label>
		</div>

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
		<h2 class="text-lg font-semibold text-foreground mb-1">Workspace Resources</h2>
		<p class="text-sm text-muted-foreground mb-6">
			Choose the folders that short-lived workers may read and write.
		</p>

		<!-- Workspace Folders -->
		<div class="mb-6">
			<h3 class="text-sm font-medium text-foreground mb-2">Workspace Folders</h3>
			<p class="text-xs text-muted-foreground mb-3">
				Each folder is mounted below <code class="bg-muted px-1 rounded">/workspace/</code> in addition to the main project workspace.
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
					onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter' && !e.isComposing && !composingFolder && e.keyCode !== 229 && newFolderPath.trim()) { workspaceFolders = [...workspaceFolders, newFolderPath.trim()]; newFolderPath = ''; } }}
					oncompositionstart={() => (composingFolder = true)}
					oncompositionend={() => setTimeout(() => (composingFolder = false), 0)}
					class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<button onclick={() => { if (newFolderPath.trim()) { workspaceFolders = [...workspaceFolders, newFolderPath.trim()]; newFolderPath = ''; } }}
					disabled={!newFolderPath.trim()}
					class="rounded-md border border-border px-3 py-2 text-sm hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
			</div>
		</div>

		<div class="rounded-lg border border-border bg-muted/30 p-4 text-xs leading-relaxed text-muted-foreground">
			The runner image already includes Git and GitHub CLI. With host login enabled, workers receive your Git identity and GitHub CLI authentication read-only. SSH keys require an explicit volume configured after setup.
		</div>

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
			Native workers run in short-lived Docker containers for security isolation.
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
					<button onclick={() => openExternal('https://docs.docker.com/get-docker/')}
						class="inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Docker Desktop &#8599;</button>
					<button onclick={() => openExternal('https://podman.io/getting-started/installation')}
						class="inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Podman &#8599;</button>
					<button onclick={recheckDocker}
						class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent">Retry</button>
				</div>
			</div>
			<p class="rounded-lg border border-border bg-muted/30 p-4 text-xs leading-relaxed text-muted-foreground">
				Docker or Podman is required because every native work attempt runs in an isolated, short-lived container.
			</p>
		{/if}

		<div class="mt-6 flex items-center justify-between">
			<button onclick={() => goToStep(2)} class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Back</button>
			<div class="flex items-center gap-3">
				{#if mode === 'add-agent'}
					<button onclick={() => goto('/agents')} class="rounded-md px-4 py-2 text-sm hover:bg-accent">Cancel</button>
				{/if}
				<button onclick={completeSetup} disabled={saving || !dockerStatus?.available}
					class="rounded-md bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed">
					{#if saving}Saving...{:else}Complete Setup{/if}
				</button>
			</div>
		</div>

		{#if saveError}<p class="mt-2 text-xs text-red-500">{saveError}</p>{/if}

	<!-- Step 4: Complete -->
	{:else if step === 4}
		<div class="text-center py-8">
			<div class="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-emerald-500/20 text-emerald-500 text-3xl">&#10003;</div>
			<h2 class="text-lg font-semibold text-foreground mb-2">
				{mode === 'add-agent' ? 'Session Added!' : 'Setup Complete!'}
			</h2>
			<p class="text-sm text-muted-foreground mb-6">
				Your session <strong>{agentName}</strong> is ready to queue native work.
			</p>
			<button onclick={() => goto('/agents')}
				class="rounded-md bg-primary px-6 py-2 text-sm text-primary-foreground hover:bg-primary/90">
				{mode === 'add-agent' ? 'Back to Sessions' : 'Open Sessions'}
			</button>
		</div>
	{/if}
</div>
