<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, projects, request, type Agent, type Project } from '$lib/api';

	type Binding = { id: string; localProjectId: string; localAgentId: string; projectId: string; agentName: string; active: boolean };
	type Settings = { base_url: string; name: string; enabled: boolean; paired: boolean; bindings: Binding[]; pairing: null | { verification_url: string; user_code: string; expires_at: string; interval: number } };
	let config = $state<Settings | null>(null);
	let baseUrl = $state('https://platform.ap.xpressai.cloud');
	let name = $state('My XpressClaw');
	let localProjects = $state<Project[]>([]);
	let localAgents = $state<Agent[]>([]);
	let remoteProjects = $state<{ id: string; name: string }[]>([]);
	let localProject = $state('');
	let localAgent = $state('');
	let remoteProject = $state('');
	let agentName = $state('');
	let busy = $state(false);
	let error = $state('');
	let notice = $state('');
	let confirmDisconnect = $state(false);
	let stopped = false;
	const endpoint = '/api/settings/connect/';

	async function load() {
		const value = await request<Settings>(endpoint);
		if (stopped) return;
		config = value;
		if (value.base_url) baseUrl = value.base_url;
		if (value.name) name = value.name;
		if (value.enabled) remoteProjects = await request('/api/settings/connect/projects');
	}

	async function action(work: () => Promise<void>) {
		if (busy) return;
		busy = true;
		error = '';
		notice = '';
		try { await work(); }
		catch (e) { error = e instanceof Error ? e.message : 'Connect request failed'; }
		finally { busy = false; }
	}

	async function pair() {
		await action(async () => {
			config = await request('/api/settings/connect/pair', { method: 'POST', body: JSON.stringify({ base_url: baseUrl, name }) });
		});
	}

	async function checkPairing() {
		await action(async () => {
			config = await request('/api/settings/connect/pair/poll', { method: 'POST', body: '{}' });
			if (config?.enabled) {
				remoteProjects = await request('/api/settings/connect/projects');
				notice = 'Account connected. Choose a Project and Agent to publish.';
			} else notice = 'Waiting for approval in Xpress AI.';
		});
	}

	async function publishAgent() {
		await action(async () => {
			config = await request('/api/settings/connect/bindings', { method: 'POST', body: JSON.stringify({ localProjectId: localProject, localAgentId: localAgent, projectId: remoteProject, agentName }) });
			notice = 'Agent published to Xpress AI.';
		});
	}

	async function disconnect() {
		await action(async () => {
			const result = await request<{ remote_revoked: boolean }>(endpoint, { method: 'DELETE' });
			await load();
			confirmDisconnect = false;
			notice = result.remote_revoked ? 'Disconnected.' : 'Disconnected locally. Revoke this instance in Xpress AI Settings → Connect when the platform is reachable.';
		});
	}

	onMount(() => {
		void action(async () => {
			await load();
			[localProjects, localAgents] = await Promise.all([projects.list(), agents.list()]);
		});
		return () => { stopped = true; };
	});
</script>

<div class="p-6 space-y-6 max-w-3xl">
	<div>
		<h1 class="text-2xl font-bold">Xpress AI Connect</h1>
		<p class="text-sm text-muted-foreground mt-1">Publish local Agents to your Xpress AI projects. Their work runs in this instance’s containers.</p>
	</div>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if notice}<p role="status" class="text-sm">{notice}</p>{/if}
	{#if !config}
		<p class="text-muted-foreground">Loading connection settings…</p>
	{:else if !config.paired}
		<form onsubmit={(event) => { event.preventDefault(); void pair(); }} class="rounded-lg border border-border bg-card p-4 space-y-4">
			<label class="block text-sm">Platform URL<input bind:value={baseUrl} type="url" required disabled={busy} class="mt-1 w-full rounded border border-input bg-background p-2" /></label>
			<label class="block text-sm">Instance name<input bind:value={name} maxlength="128" required disabled={busy} class="mt-1 w-full rounded border border-input bg-background p-2" /></label>
			<button type="submit" disabled={busy} class="rounded bg-primary px-4 py-2 text-primary-foreground disabled:opacity-50">{config.pairing ? 'Start new pairing' : 'Connect account'}</button>
		</form>
		{#if config.pairing}
			<div class="rounded-lg border border-border bg-card p-4 space-y-3">
				<p>Open Xpress AI, verify this instance, and choose the projects it may access.</p>
				<p class="font-mono break-all">{config.pairing.user_code}</p>
				<a href={config.pairing.verification_url} target="_blank" rel="noopener noreferrer" class="text-primary underline">Open Xpress AI to approve</a>
				<div><button onclick={checkPairing} disabled={busy} class="rounded border border-input px-4 py-2">I’ve approved the connection</button></div>
			</div>
		{/if}
	{:else}
		<div class="rounded-lg border border-border bg-card p-4 space-y-2">
			<p class="font-medium">{config.name}</p>
			<p class="text-sm text-muted-foreground">Paired with {config.base_url}</p>
			<p class="text-sm">Only published Agents can receive work. Harness credentials and workspace files stay local unless an Agent publishes a result.</p>
			<button onclick={() => confirmDisconnect = true} disabled={busy} class="rounded border border-input px-3 py-2 text-sm">Disconnect instance</button>
			{#if confirmDisconnect}
				<div class="rounded border border-border p-3 space-y-2">
					<p>Stop accepting platform work and disconnect every published Agent on this instance?</p>
					<button onclick={disconnect} disabled={busy} class="rounded bg-destructive px-3 py-2 text-destructive-foreground">Disconnect</button>
					<button onclick={() => confirmDisconnect = false} disabled={busy} class="ml-2 rounded border px-3 py-2">Keep connected</button>
				</div>
			{/if}
		</div>
		<form onsubmit={(event) => { event.preventDefault(); void publishAgent(); }} class="rounded-lg border border-border bg-card p-4 space-y-4">
			<h2 class="font-semibold">Publish an Agent</h2>
			<label class="block text-sm">Local Project<select bind:value={localProject} onchange={() => localAgent = ''} required disabled={busy} class="mt-1 w-full rounded border border-input bg-background p-2"><option value="">Select a Project</option>{#each localProjects as project (project.id)}<option value={project.id}>{project.name}</option>{/each}</select></label>
			<label class="block text-sm">Local Agent<select bind:value={localAgent} required disabled={busy || !localProject} class="mt-1 w-full rounded border border-input bg-background p-2"><option value="">Select an Agent</option>{#each localAgents.filter(agent => agent.project_id === localProject) as agent (agent.id)}<option value={agent.id}>{agent.name}</option>{/each}</select></label>
			<label class="block text-sm">Xpress AI project<select bind:value={remoteProject} required disabled={busy} class="mt-1 w-full rounded border border-input bg-background p-2"><option value="">Select a project</option>{#each remoteProjects as project (project.id)}<option value={project.id}>{project.name}</option>{/each}</select></label>
			<label class="block text-sm">Agent name in Xpress AI<input bind:value={agentName} pattern="[a-z0-9][a-z0-9-]*" maxlength="63" required disabled={busy} class="mt-1 w-full rounded border border-input bg-background p-2" /><span class="text-muted-foreground">Use lowercase letters, digits, and hyphens.</span></label>
			<button type="submit" disabled={busy} class="rounded bg-primary px-4 py-2 text-primary-foreground disabled:opacity-50">Publish Agent</button>
		</form>
		<div class="space-y-3">
			<h2 class="font-semibold">Published Agents</h2>
			{#each config.bindings as binding (binding.id)}
				<div class="rounded-lg border border-border p-4 flex items-center justify-between gap-4">
					<div><p class="font-medium">{binding.agentName}</p><p class="text-sm text-muted-foreground">{binding.localAgentId} → {remoteProjects.find(p => p.id === binding.projectId)?.name ?? binding.projectId}</p><p class="text-xs">{binding.active ? 'Published' : 'Disconnected'}</p></div>
					{#if binding.active}<button disabled={busy} onclick={() => action(async () => { config = await request(`/api/settings/connect/bindings/${encodeURIComponent(binding.id)}`, { method: 'DELETE' }); })} class="rounded border border-input px-3 py-2 text-sm">Disconnect Agent</button>{/if}
				</div>
			{:else}<p class="text-sm text-muted-foreground">No Agents published yet.</p>{/each}
		</div>
	{/if}
</div>
