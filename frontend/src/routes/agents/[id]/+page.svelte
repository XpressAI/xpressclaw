<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { agents, setup } from '$lib/api';
	import type { Agent, LiveConfig } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	import ProfileTab from './ProfileTab.svelte';
	import PromptsTab from './PromptsTab.svelte';
	import WorkspaceTab from './WorkspaceTab.svelte';
	import ToolsTab from './ToolsTab.svelte';
	import SkillsTab from './SkillsTab.svelte';
	import ProceduresTab from './ProceduresTab.svelte';
	import BudgetTab from './BudgetTab.svelte';
	import MemoryTab from './MemoryTab.svelte';
	import TasksTab from './TasksTab.svelte';
	import SchedulesTab from './SchedulesTab.svelte';
	import ChannelsTab from './ChannelsTab.svelte';

	let agent = $state<Agent | null>(null);
	let error = $state<string | null>(null);
	let agentConfig = $state<LiveConfig['agents'][0] | null>(null);
	let activeTab = $state('profile');
	let needsRestart = $state(false);
	let saveMessage = $state('');
	let showDeleteConfirm = $state(false);
	let deleting = $state(false);
	let saving = $state(false);
	// Incremented to signal the active tab to save
	let saveSignal = $state(0);

	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let loadedAgentId = '';
	let unsubPage: (() => void) | null = null;

	const tabs = [
		{ id: 'profile', label: 'Profile' },
		{ id: 'prompts', label: 'Prompts' },
		{ id: 'workspace', label: 'Workspace' },
		{ id: 'tools', label: 'Tools' },
		{ id: 'skills', label: 'Skills' },
		{ id: 'procedures', label: 'Procedures' },
		{ id: 'budget', label: 'Budget' },
		{ id: 'memory', label: 'Memory' },
		{ id: 'tasks', label: 'Tasks' },
		{ id: 'schedules', label: 'Schedules' },
		{ id: 'channels', label: 'Channels' },
	];

	onMount(() => {
		unsubPage = page.subscribe(($p) => {
			const id = $p.params.id;
			if (id && id !== loadedAgentId) {
				loadedAgentId = id;
				loadAgent(id);
			}
		});
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		if (unsubPage) unsubPage();
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
			await agents.stop(agent.id);
			agent = await agents.start(agent.id);
			needsRestart = false;
		} catch (e) { alert(String(e)); }
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

	function agentDisplayName(): string {
		if (agentConfig?.display_name) return agentConfig.display_name;
		const name = agent?.name ?? '';
		return name.charAt(0).toUpperCase() + name.slice(1);
	}

	function saveCurrentTab() {
		saveSignal++;
	}

	async function handleSave(data: Parameters<typeof agents.updateConfig>[1]) {
		if (!agent) return;
		saving = true;
		saveMessage = '';
		try {
			const result = await agents.updateConfig(agent.id, data);
			if (result.needs_restart) {
				needsRestart = true;
				saveMessage = 'Saved. Restart for changes to take effect.';
			} else {
				saveMessage = 'Saved.';
			}
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
	<div class="shrink-0 px-6 py-4 space-y-3 border-b border-border">
		<div class="flex items-center gap-2 text-sm text-muted-foreground">
			<a href="/agents" class="hover:text-foreground">Agents</a>
			<span>/</span>
			<span class="text-foreground">{agentDisplayName()}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
		{:else if agent}
			<div class="flex items-start justify-between">
				<div class="flex items-center gap-3">
					<!-- Avatar -->
					<div class="w-10 h-10 rounded-full bg-primary/20 flex items-center justify-center text-sm font-semibold text-primary">
						{(agentConfig?.display_name || agent.name).charAt(0).toUpperCase()}
					</div>
					<div>
						<h1 class="text-xl font-bold">{agentDisplayName()}</h1>
						<p class="text-sm text-muted-foreground">
							<span class="{statusColor(agent.status)}">{agent.status}</span>
							{#if agent.restart_count > 0 && agent.desired_status === 'running' && agent.status !== 'running'}
								<span class="text-amber-500">(restarting, attempt {agent.restart_count})</span>
							{/if}
							&middot; {agent.backend}
							{#if agentConfig?.role_title}
								&middot; {agentConfig.role_title}
							{/if}
						</p>
					</div>
				</div>
				<div class="flex items-center gap-2">
					{#if saveMessage}
						<span class="text-xs {saveMessage.startsWith('Error') ? 'text-destructive' : 'text-emerald-500'}">{saveMessage}</span>
					{/if}
					{#if needsRestart}
						<button onclick={handleRestart}
							class="rounded-md bg-amber-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-amber-700 transition-colors">
							Restart
						</button>
					{/if}
					{#if agent.desired_status === 'running'}
						<button onclick={handleStop}
							class="rounded-md border border-border bg-secondary px-3 py-1.5 text-sm font-medium hover:bg-accent transition-colors">
							Stop
						</button>
					{:else}
						<button onclick={handleStart}
							class="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors">
							Start
						</button>
					{/if}
					<button onclick={() => { showDeleteConfirm = true; }}
						class="rounded-md border border-destructive/50 px-3 py-1.5 text-sm font-medium text-destructive hover:bg-destructive/10 transition-colors">
						Delete
					</button>
				</div>
			</div>

			{#if needsRestart && saveMessage}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-2 text-xs text-amber-600">{saveMessage}</div>
			{/if}

			{#if agent.error_message}
				<div class="rounded-lg border border-destructive/50 bg-destructive/5 p-2 text-xs text-destructive">{agent.error_message}</div>
			{/if}
		{/if}
	</div>

	<!-- Tab bar -->
	{#if agent}
		<div class="shrink-0 border-b border-border px-4">
			<div class="flex gap-0 -mb-px overflow-x-auto scrollbar-hide">
				{#each tabs as tab}
					<button
						onclick={() => activeTab = tab.id}
						class="px-3 py-2 text-xs whitespace-nowrap transition-colors border-b-2 {activeTab === tab.id
							? 'border-primary text-foreground font-medium'
							: 'border-transparent text-muted-foreground hover:text-foreground hover:border-border'}">
						{tab.label}
					</button>
				{/each}
			</div>
		</div>

		<!-- Tab content -->
		<div class="flex-1 overflow-y-auto px-6 py-4">
			{#if activeTab === 'profile'}
				<ProfileTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'prompts'}
				<PromptsTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'workspace'}
				<WorkspaceTab {agentConfig} agentId={agent.id} onSave={handleSave} />
			{:else if activeTab === 'tools'}
				<ToolsTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'skills'}
				<SkillsTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'procedures'}
				<ProceduresTab agentId={agent.id} />
			{:else if activeTab === 'budget'}
				<BudgetTab {agentConfig} agentId={agent.id} onSave={handleSave} {saveSignal} />
			{:else if activeTab === 'memory'}
				<MemoryTab agentId={agent.id} />
			{:else if activeTab === 'tasks'}
				<TasksTab agentId={agent.id} />
			{:else if activeTab === 'schedules'}
				<SchedulesTab agentId={agent.id} />
			{:else if activeTab === 'channels'}
				<ChannelsTab />
			{/if}
		</div>

		<!-- Persistent save bar for config tabs -->
		{#if ['profile', 'prompts', 'tools', 'skills', 'budget', 'workspace'].includes(activeTab)}
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
			<h2 class="text-lg font-semibold">Delete {agentDisplayName()}?</h2>
			<p class="text-sm text-muted-foreground">
				This will stop the agent, remove its configuration, and delete all associated data. This action cannot be undone.
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

<style>
	.scrollbar-hide {
		-ms-overflow-style: none;
		scrollbar-width: none;
	}
	.scrollbar-hide::-webkit-scrollbar {
		display: none;
	}
</style>
