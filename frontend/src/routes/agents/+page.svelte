<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { agents } from '$lib/api';
	import type { Agent } from '$lib/api';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import { PROJECT_CONTEXT_MENU_ITEMS } from '$lib/contextMenu';
	import { openWorkspaceWindow } from '$lib/openWorkspaceWindow';
	import { harnessMark, statusColor, timeAgo } from '$lib/utils';
	import { projectPath, type ProjectSection } from '$lib/workspace';

	let agentList = $state<Agent[]>([]);
	let loading = $state(true);
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let projectMenu = $state<{ agent: Agent; x: number; y: number } | null>(null);

	onMount(async () => {
		agentList = await agents.list().catch(() => []);
		loading = false;
		// Poll every 5s so reconciler progress is visible
		pollTimer = setInterval(async () => {
			agentList = await agents.list().catch(() => agentList);
		}, 5000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	function showProjectMenu(event: MouseEvent, agent: Agent) {
		event.preventDefault();
		event.stopPropagation();
		projectMenu = { agent, x: event.clientX, y: event.clientY };
	}

	function selectProjectMenuItem(agent: Agent, action: string) {
		if (action === 'open-new-window') {
			void openWorkspaceWindow(projectPath(agent.id), agent.title || agent.name).catch((error) => {
				console.error('failed to open project window', error);
				window.alert(error instanceof Error ? error.message : 'Could not open the window.');
			});
			return;
		}

		const sections: Record<string, ProjectSection> = {
			'open-tasks': 'tasks',
			'open-schedules': 'schedules',
			'open-runner': 'runner',
			'open-workspace': 'workspace',
		};
		const section = sections[action];
		if (section) goto(projectPath(agent.id, section), { keepFocus: true, noScroll: true });
	}

</script>

<div class="space-y-6 p-4 sm:p-6">
	<div class="flex items-center justify-between gap-3">
		<div>
			<h1 class="text-2xl font-bold">Projects</h1>
			<p class="text-sm text-muted-foreground mt-1">{agentList.length} workspace{agentList.length === 1 ? '' : 's'} connected</p>
		</div>
		<a href="/setup?mode=add-session"
			class="shrink-0 rounded-md bg-primary px-3 py-2 text-sm text-primary-foreground hover:bg-primary/90 sm:px-4">
			+ Add <span class="hidden sm:inline">Project</span>
		</a>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if agentList.length === 0}
		<div class="rounded-lg border border-border bg-card p-8 text-center">
			<p class="text-muted-foreground">No projects configured.</p>
			<p class="text-sm text-muted-foreground mt-2">Connect a workspace to a native coding agent.</p>
			<a href="/setup?mode=add-session" class="mt-4 inline-flex rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Create project</a>
		</div>
	{:else}
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each agentList as agent}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div oncontextmenu={(event) => showProjectMenu(event, agent)} class="rounded-lg border border-border bg-card p-4 space-y-3">
					<div class="flex items-start justify-between">
						<div class="flex items-center gap-3">
							<span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-muted text-sm font-semibold">{harnessMark(agent.backend)}</span>
							<div>
								<a href="/agents/{agent.id}" class="text-sm font-semibold hover:underline">{agent.title || agent.name}</a>
								<div class="text-xs text-muted-foreground mt-0.5">{agent.backend}</div>
							</div>
						</div>
						<span class="inline-flex items-center gap-1.5 text-xs {statusColor(agent.status)}">
							<span class="h-1.5 w-1.5 rounded-full {agent.status === 'running' ? 'bg-blue-400 animate-pulse' : agent.status === 'queued' ? 'bg-amber-400' : agent.status === 'waiting_for_input' ? 'bg-orange-400 animate-pulse' : ['error', 'failed', 'blocked'].includes(agent.status) ? 'bg-red-400' : 'bg-emerald-400'}"></span>
							{agent.status === 'waiting_for_input' ? 'waiting for you' : agent.status.replaceAll('_', ' ')}
						</span>
					</div>

					{#if agent.error_message}
						<div class="text-xs text-destructive bg-destructive/10 rounded px-2 py-1">{agent.error_message}</div>
					{/if}

					<div class="text-xs text-muted-foreground">{agent.status === 'waiting_for_input' ? 'Waiting for your reply' : agent.status === 'running' ? 'Agent is working' : agent.status === 'queued' ? 'Work is queued' : 'Ready for work'} &middot; created {timeAgo(agent.created_at)}</div>

					<div class="flex gap-2">
						<a
							href="/agents/{agent.id}"
							class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
						>
							Open project
						</a>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

{#if projectMenu}
	<ContextMenu
		x={projectMenu.x}
		y={projectMenu.y}
		label={`${projectMenu.agent.title || projectMenu.agent.name} actions`}
		items={PROJECT_CONTEXT_MENU_ITEMS}
		onselect={(action) => selectProjectMenuItem(projectMenu!.agent, action)}
		onclose={() => (projectMenu = null)}
	/>
{/if}
