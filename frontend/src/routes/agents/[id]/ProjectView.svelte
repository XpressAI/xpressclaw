<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { agents, setup } from '$lib/api';
	import type { Agent, LiveConfig } from '$lib/api';
	import { agentRuntimeSummary, agentRuntimeTitle, harnessMark, statusColor } from '$lib/utils';
	import { projectPath, type ProjectSection } from '$lib/workspace';

	import SessionTab from './SessionTab.svelte';
	import RunnerTab from './RunnerTab.svelte';
	import WorkspaceTab from './WorkspaceTab.svelte';
	import TasksTab from './TasksTab.svelte';
	import SchedulesTab from './SchedulesTab.svelte';
	import FilesTab from './FilesTab.svelte';

	let { agentId, section = 'session', route = '' }: { agentId: string; section?: ProjectSection; route?: string } = $props();

	let agent = $state<Agent | null>(null);
	let error = $state<string | null>(null);
	let agentConfig = $state<LiveConfig['agents'][0] | null>(null);
	let activeTab = $derived(section);
	let saveMessage = $state('');
	let showDeleteConfirm = $state(false);
	let deleting = $state(false);
	let saving = $state(false);
	// Incremented to signal the active tab to save
	let saveSignal = $state(0);

	let pollTimer: ReturnType<typeof setInterval> | null = null;

	const tabs: { id: ProjectSection; label: string }[] = [
		{ id: 'session', label: 'Work' },
		{ id: 'tasks', label: 'Tasks' },
		{ id: 'schedules', label: 'Automations' },
		{ id: 'files', label: 'Files' },
		{ id: 'runner', label: 'Harness' },
		{ id: 'workspace', label: 'Environment' },
	];

	onMount(() => {
		loadAgent(agentId);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function loadAgent(id: string) {
		if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
		error = null;

		try {
			agent = await agents.get(id);
			const config = await setup.getConfig();
			agentConfig = config.agents.find(a => a.name === agent!.name) ?? null;
		} catch (e) {
			error = String(e);
		}

		// Poll status every 5s — update fields in-place to avoid XCLAW-48
		pollTimer = setInterval(async () => {
			try {
				const fresh = await agents.get(id);
				if (agent) {
					agent.status = fresh.status;
					agent.desired_status = fresh.desired_status;
					agent.observed_status = fresh.observed_status;
					agent.container_id = fresh.container_id;
					agent.error_message = fresh.error_message;
					agent.restart_count = fresh.restart_count;
					agent.started_at = fresh.started_at;
					agent.stopped_at = fresh.stopped_at;
				} else {
					agent = fresh;
				}
			} catch {}
		}, 5000);
	}

	async function handleDelete() {
		if (!agent || deleting) return;
		deleting = true;
		try {
			await agents.delete(agent.id);
			window.location.href = '/agents';
		} catch (e) {
			deleting = false;
			showDeleteConfirm = false;
			alert(String(e));
		}
	}

	function sessionTitle(): string {
		return agent?.title || agent?.name || '';
	}

	function saveCurrentTab() {
		saveSignal++;
	}

	function selectTab(tab: ProjectSection) {
		goto(projectPath(agentId, tab), { keepFocus: true, noScroll: true });
	}

	async function handleSave(data: Parameters<typeof agents.updateConfig>[1]) {
		if (!agent) return;
		saving = true;
		saveMessage = '';
		try {
			const result = await agents.updateConfig(agent.id, data);
			saveMessage = 'Saved. New attempts will use this configuration.';
			if (agentConfig) {
				agentConfig = { ...agentConfig, ...result.agent };
			}
			setTimeout(() => { saveMessage = ''; }, 3000);
		} catch (e) {
			saveMessage = `Error: ${e}`;
		}
		saving = false;
	}
</script>

<div class="flex flex-col h-full">
	<!-- Header -->
	<div class="shrink-0 space-y-3 border-b border-border px-4 py-3 sm:px-6 sm:py-4">
		<div class="flex items-center gap-2 text-sm text-muted-foreground">
			<a href="/agents" class="hover:text-foreground">Agents</a>
			<span>/</span>
			<span class="text-foreground">{sessionTitle()}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
		{:else if agent}
			<div class="flex items-start justify-between gap-3">
				<div class="flex min-w-0 items-center gap-3">
					<div class="w-10 h-10 rounded-full bg-primary/20 flex items-center justify-center text-sm font-semibold text-primary">
						{harnessMark(agent.backend)}
					</div>
					<div class="min-w-0">
						<h1 class="truncate text-lg font-bold sm:text-xl">{sessionTitle()}</h1>
						<p class="truncate text-sm text-muted-foreground" title={agentRuntimeTitle(agent)}>
							<span class="{statusColor(agent.status)}">{agent.status === 'waiting_for_input' ? 'waiting for you' : agent.status.replaceAll('_', ' ')}</span>
							&middot; {agentRuntimeSummary(agent)}
						</p>
					</div>
				</div>
				<div class="flex items-center gap-2">
					{#if saveMessage}
						<span class="text-xs {saveMessage.startsWith('Error') ? 'text-destructive' : 'text-emerald-500'}">{saveMessage}</span>
					{/if}
					<button onclick={() => { showDeleteConfirm = true; }} aria-label="Delete agent"
						class="rounded-md border border-destructive/50 px-3 py-1.5 text-sm font-medium text-destructive hover:bg-destructive/10 transition-colors">
						<span class="hidden sm:inline">Delete</span><span class="sm:hidden">×</span>
					</button>
				</div>
			</div>

			{#if agent.error_message}
				<div class="rounded-lg border border-destructive/50 bg-destructive/5 p-2 text-xs text-destructive">{agent.error_message}</div>
			{/if}
		{/if}
	</div>

	<!-- Tab bar -->
	{#if agent}
		<div class="shrink-0 border-b border-border px-4">
			<div class="flex gap-0 -mb-px overflow-x-auto scrollbar-hide" role="tablist" aria-label="Agent sections">
				{#each tabs as tab}
					<button
						type="button"
						role="tab"
						aria-selected={activeTab === tab.id}
						onclick={() => selectTab(tab.id)}
						class="px-3 py-2 text-xs whitespace-nowrap transition-colors border-b-2 {activeTab === tab.id
							? 'border-primary text-foreground font-medium'
							: 'border-transparent text-muted-foreground hover:text-foreground hover:border-border'}">
						{tab.label}
					</button>
				{/each}
			</div>
		</div>

		<!-- Tab content -->
		<div class="flex-1 min-h-0 {activeTab === 'files' ? 'overflow-hidden' : 'overflow-y-auto px-3 py-4 sm:px-6'}">
			{#if activeTab === 'session'}
				<SessionTab agentId={agent.id} />
			{:else if activeTab === 'runner'}
				<RunnerTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'workspace'}
				<WorkspaceTab agentId={agent.id} {agentConfig} onSave={handleSave} />
			{:else if activeTab === 'tasks'}
				<TasksTab agentId={agent.id} />
			{:else if activeTab === 'schedules'}
				<SchedulesTab agentId={agent.id} />
			{:else if activeTab === 'files'}
				<FilesTab agentId={agent.id} {route} />
			{/if}
		</div>

		<!-- Persistent save bar for config tabs -->
		{#if activeTab === 'runner'}
			<div class="shrink-0 border-t border-border bg-background px-6 py-3 flex items-center justify-end gap-3">
				{#if saveMessage}
					<span class="text-xs {saveMessage.startsWith('Error') ? 'text-destructive' : 'text-emerald-500'}">{saveMessage}</span>
				{/if}
				<button onclick={saveCurrentTab} disabled={saving}
					class="rounded-md bg-primary px-6 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
					{saving ? 'Saving...' : 'Save Changes'}
				</button>
			</div>
		{/if}
	{:else if !error}
		<div class="flex-1 flex items-center justify-center text-sm text-muted-foreground">Loading...</div>
	{/if}
</div>

{#if showDeleteConfirm && agent}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
		<div class="rounded-lg border border-border bg-card p-6 space-y-4 max-w-md mx-4">
			<h2 class="text-lg font-semibold">Delete {sessionTitle()}?</h2>
			<p class="text-sm text-muted-foreground">
				This removes the agent, its harness configuration, and its workspace association. Existing task history may still reference it. This action cannot be undone.
			</p>
			<div class="flex justify-end gap-2">
				<button onclick={() => { showDeleteConfirm = false; }}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</button>
				<button onclick={handleDelete} disabled={deleting}
					class="rounded-md bg-destructive px-4 py-2 text-sm text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50">
					{deleting ? 'Deleting...' : 'Delete'}
				</button>
			</div>
		</div>
	</div>
{/if}
