<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import { agents, setup } from '$lib/api';
	import type { Agent, LiveConfig } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let agent = $state<Agent | null>(null);
	let error = $state<string | null>(null);
	let agentConfig = $state<LiveConfig['agents'][0] | null>(null);

	// Editable state
	let editRole = $state('');
	let editVolumes = $state<string[]>([]);
	let newVolumePath = $state('');
	let saving = $state(false);
	let saveMessage = $state('');
	let needsRestart = $state(false);

	// Tool toggles
	let fetchEnabled = $state(false);
	let gitEnabled = $state(false);
	let githubEnabled = $state(false);

	onMount(async () => {
		try {
			agent = await agents.get($page.params.id!);
			// Load live config to get tools, volumes, role
			const config = await setup.getConfig();
			agentConfig = config.agents.find(a => a.name === agent!.name) ?? null;
			if (agentConfig) {
				editRole = agentConfig.role;
				editVolumes = [...(agentConfig.volumes || [])];
				fetchEnabled = agentConfig.tools.includes('fetch');
				gitEnabled = agentConfig.tools.includes('git');
				githubEnabled = agentConfig.tools.includes('github');
			}
		} catch (e) {
			error = String(e);
		}
	});

	async function handleStart() {
		if (!agent) return;
		try { agent = await agents.start(agent.id); needsRestart = false; } catch (e) { alert(String(e)); }
	}

	async function handleStop() {
		if (!agent) return;
		try { agent = await agents.stop(agent.id); } catch (e) { alert(String(e)); }
	}

	async function handleRestart() {
		if (!agent) return;
		try {
			agent = await agents.stop(agent.id);
			agent = await agents.start(agent.id);
			needsRestart = false;
		} catch (e) { alert(String(e)); }
	}

	async function saveConfig() {
		if (!agent) return;
		saving = true;
		saveMessage = '';
		try {
			const tools = ['filesystem', 'shell', 'memory'];
			if (fetchEnabled) tools.push('fetch');
			if (gitEnabled) tools.push('git');
			if (githubEnabled) tools.push('github');

			const result = await agents.updateConfig(agent.id, {
				role: editRole,
				volumes: editVolumes,
				tools,
			});
			if (result.needs_restart) {
				needsRestart = true;
				saveMessage = 'Saved. Restart the agent for changes to take effect.';
			} else {
				saveMessage = 'Saved.';
			}
			// Update local state
			if (agentConfig) {
				agentConfig = { ...agentConfig, ...result.agent };
			}
		} catch (e) {
			saveMessage = `Error: ${e}`;
		}
		saving = false;
	}

	function addVolume() {
		if (newVolumePath.trim()) {
			const path = newVolumePath.trim();
			const basename = path.split('/').filter(Boolean).pop() || 'workspace';
			editVolumes = [...editVolumes, `${path}:/workspace/${basename}`];
			newVolumePath = '';
		}
	}

	function removeVolume(idx: number) {
		editVolumes = editVolumes.filter((_, i) => i !== idx);
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center gap-2 text-sm text-muted-foreground">
		<a href="/agents" class="hover:text-foreground">Agents</a>
		<span>/</span>
		<span class="text-foreground">{agent?.name ?? '...'}</span>
	</div>

	{#if error}
		<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
	{:else if agent}
		<!-- Header -->
		<div class="flex items-start justify-between">
			<div>
				<h1 class="text-2xl font-bold">{agent.name}</h1>
				<p class="text-sm text-muted-foreground mt-1">
					<span class="{statusColor(agent.status)}">{agent.status}</span>
					&middot; {agent.backend}
				</p>
			</div>
			<div class="flex gap-2">
				{#if agent.status === 'running'}
					{#if needsRestart}
						<button onclick={handleRestart}
							class="rounded-md bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700 transition-colors">
							Restart
						</button>
					{/if}
					<button onclick={handleStop}
						class="rounded-md border border-border bg-secondary px-4 py-2 text-sm font-medium hover:bg-accent transition-colors">
						Stop
					</button>
				{:else}
					<button onclick={handleStart}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors">
						Start
					</button>
				{/if}
			</div>
		</div>

		{#if needsRestart && saveMessage}
			<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm text-amber-600">
				{saveMessage}
			</div>
		{/if}

		{#if agent.error_message}
			<div class="rounded-lg border border-destructive/50 bg-card p-4 space-y-2">
				<h2 class="text-sm font-semibold text-destructive">Error</h2>
				<pre class="text-xs text-muted-foreground whitespace-pre-wrap">{agent.error_message}</pre>
			</div>
		{/if}

		<div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
			<!-- Left: Details -->
			<div class="space-y-4">
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Details</h2>
					<dl class="space-y-2 text-sm">
						<div class="flex justify-between">
							<dt class="text-muted-foreground">ID</dt>
							<dd class="font-mono text-xs">{agent.id}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Backend</dt>
							<dd>{agent.backend}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Created</dt>
							<dd>{timeAgo(agent.created_at)}</dd>
						</div>
						{#if agent.started_at}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Started</dt>
								<dd>{timeAgo(agent.started_at)}</dd>
							</div>
						{/if}
						{#if agent.container_id}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Container</dt>
								<dd class="font-mono text-xs truncate max-w-[200px]">{agent.container_id}</dd>
							</div>
						{/if}
					</dl>
				</div>
			</div>

			<!-- Right: Configuration -->
			<div class="lg:col-span-2 space-y-4">
				<!-- System Prompt -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">System Prompt</h2>
					<textarea bind:value={editRole} rows="5"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"></textarea>
				</div>

				<!-- Workspace Folders -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Workspace Folders</h2>
					<p class="text-xs text-muted-foreground">
						Folders from your machine mounted into the agent's container at <code class="bg-muted px-1 rounded">/workspace/</code>.
					</p>
					{#if editVolumes.length > 0}
						<div class="space-y-2">
							{#each editVolumes as vol, i}
								{@const parts = vol.split(':')}
								<div class="flex items-center gap-2 rounded-md border border-border px-3 py-2">
									<span class="flex-1 text-sm font-mono truncate">{parts[0]}</span>
									<span class="text-xs text-muted-foreground">{parts[1] || ''}</span>
									<button onclick={() => removeVolume(i)}
										class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground">&#x2715;</button>
								</div>
							{/each}
						</div>
					{/if}
					<div class="flex gap-2">
						<input type="text" bind:value={newVolumePath} placeholder="~/projects/my-app"
							onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') addVolume(); }}
							class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						<button onclick={addVolume} disabled={!newVolumePath.trim()}
							class="rounded-md border border-border px-3 py-2 text-sm hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
					</div>
				</div>

				<!-- Tools -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Tools</h2>
					<div class="flex gap-2 mb-2">
						<span class="inline-flex items-center gap-1 rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">
							Filesystem
						</span>
						<span class="inline-flex items-center gap-1 rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">
							Shell
						</span>
						<span class="text-xs text-muted-foreground self-center">always included</span>
					</div>
					<div class="space-y-2">
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={fetchEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">Internet Access (Fetch)</span>
								<span class="text-xs text-muted-foreground ml-1">Fetch web pages and APIs</span>
							</div>
						</label>
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={gitEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">Git</span>
								<span class="text-xs text-muted-foreground ml-1">Interact with repositories</span>
							</div>
						</label>
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={githubEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">GitHub</span>
								<span class="text-xs text-muted-foreground ml-1">Issues, PRs, repos</span>
							</div>
						</label>
					</div>
				</div>

				<!-- Save -->
				<div class="flex items-center gap-3">
					<button onclick={saveConfig} disabled={saving}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
						{saving ? 'Saving...' : 'Save Changes'}
					</button>
					{#if saveMessage && !needsRestart}
						<span class="text-xs text-emerald-500">{saveMessage}</span>
					{/if}
				</div>
			</div>
		</div>
	{:else}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{/if}
</div>
