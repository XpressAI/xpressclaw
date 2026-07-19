<script lang="ts">
	import type { Snippet } from 'svelte';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { agents, tasks as tasksApi, workflows as workflowsApi } from '$lib/api';
	import type { Agent, Task, Workflow } from '$lib/api';
	import { harnessMark, timeAgo } from '$lib/utils';
	import {
		createWorkspaceTab,
		describeWorkspacePath,
		sameWorkspaceTab,
		statusPriority,
		validWorkspacePane,
		workspaceId,
		workspacePath,
		type WorkspacePaneState,
		type WorkspaceTab,
		type WorkspaceTabKind,
	} from '$lib/workspace';
	import WorkspacePane from './WorkspacePane.svelte';

	let { children }: { children: Snippet } = $props();

	const WORKSPACE_STORAGE_KEY = 'xpressclaw.workspace.v1';
	const MAX_PANES = 4;
	const initialDescription = describeWorkspacePath($page.url.pathname);
	const initialTab: WorkspaceTab = { id: 'initial-tab', status: null, ...initialDescription };

	let panes = $state<WorkspacePaneState[]>([
		{ id: 'initial-pane', tabs: [initialTab], activeTabId: initialTab.id, width: 1 },
	]);
	let focusedPaneId = $state('initial-pane');
	let workspaceReady = $state(false);
	let lastSyncedPath = '';
	let workspaceEl = $state<HTMLDivElement>();
	let sidebarCollapsed = $state(false);
	let mobileMenuOpen = $state(false);
	let agentList = $state<Agent[]>([]);
	let taskList = $state<Task[]>([]);
	let workflowList = $state<Workflow[]>([]);

	let dockerAvailable = $state(true);
	let dockerInstalled = $state(true);
	let dockerCanStart = $state(false);
	let dockerStarting = $state(false);
	let serverConnected = $state(true);
	let wasDisconnected = false;
	let consecutiveFailures = 0;
	const FAILURES_BEFORE_DISCONNECT = 2;
	const HEALTH_TIMEOUT_MS = 8000;

	let focusedPane = $derived(panes.find((pane) => pane.id === focusedPaneId) ?? panes[0]);
	let focusedTab = $derived(focusedPane?.tabs.find((tab) => tab.id === focusedPane.activeTabId) ?? focusedPane?.tabs[0] ?? null);
	let openTabs = $derived(panes.flatMap((pane) => pane.tabs.map((tab) => ({ paneId: pane.id, tab }))));
	let attentionTasks = $derived(taskList
		.filter((task) => task.status === 'waiting_for_input' || task.status === 'blocked')
		.sort((left, right) => statusPriority(right.status) - statusPriority(left.status)
			|| Date.parse(right.updated_at) - Date.parse(left.updated_at)));
	let focusedProject = $derived((() => {
		if (!focusedTab) return null;
		if (focusedTab.kind === 'project') return agentList.find((agent) => agent.id === focusedTab.resourceId) ?? null;
		if (focusedTab.kind === 'task') {
			const task = taskList.find((candidate) => candidate.id === focusedTab.resourceId);
			return agentList.find((agent) => agent.id === task?.agent_id) ?? null;
		}
		return null;
	})());

	const utilityTabs: { kind: WorkspaceTabKind; label: string; href: string; icon: string }[] = [
		{ kind: 'projects', label: 'Projects', href: '/agents', icon: 'M18 18.72a9.094 9.094 0 003.741-.479 3 3 0 00-4.682-2.72m.94 3.198A5.995 5.995 0 0012 12.75a5.995 5.995 0 00-5.058 2.772M15 6.75a3 3 0 11-6 0 3 3 0 016 0z' },
		{ kind: 'tasks', label: 'Tasks', href: '/tasks', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4' },
		{ kind: 'workflows', label: 'Workflows', href: '/workflows', icon: 'M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5' },
		{ kind: 'settings', label: 'Settings', href: '/settings', icon: 'M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z M15 12a3 3 0 11-6 0 3 3 0 016 0z' },
	];

	$effect(() => {
		const pathname = $page.url.pathname;
		if (!workspaceReady || !workspacePath(pathname) || pathname === lastSyncedPath) return;
		lastSyncedPath = pathname;
		openPath(pathname, false);
	});

	function activeTabFor(pane: WorkspacePaneState): WorkspaceTab {
		return pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0];
	}

	function persistWorkspace() {
		if (!workspaceReady) return;
		try {
			localStorage.setItem(WORKSPACE_STORAGE_KEY, JSON.stringify({ panes, focusedPaneId }));
			localStorage.setItem('xpressclaw.sidebar.collapsed', String(sidebarCollapsed));
		} catch {}
	}

	function projectStatus(agent: Agent): string {
		const statuses = [agent.status, ...taskList.filter((task) => task.agent_id === agent.id && !['completed', 'cancelled'].includes(task.status)).map((task) => task.status)];
		return statuses.reduce((highest, status) => statusPriority(status) > statusPriority(highest) ? status : highest, agent.status);
	}

	function decorateTab(tab: WorkspaceTab): WorkspaceTab {
		const description = describeWorkspacePath(tab.path);
		if (description.kind === 'task') {
			const task = taskList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: task?.title ?? tab.title ?? 'Task', status: task?.status ?? tab.status };
		}
		if (description.kind === 'project') {
			const agent = agentList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: agent?.title || agent?.name || tab.title || 'Project', status: agent ? projectStatus(agent) : tab.status };
		}
		if (description.kind === 'workflow') {
			const workflow = workflowList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: workflow?.name ?? tab.title ?? 'Workflow', status: null };
		}
		return { ...tab, ...description, status: null };
	}

	function refreshTabMetadata() {
		panes = panes.map((pane) => ({ ...pane, tabs: pane.tabs.map(decorateTab) }));
		persistWorkspace();
	}

	function openPath(pathname: string, navigate = true) {
		if (!workspacePath(pathname)) return;
		let paneIndex = Math.max(0, panes.findIndex((pane) => pane.id === focusedPaneId));
		let tabIndex = panes[paneIndex]?.tabs.findIndex((tab) => sameWorkspaceTab(tab, pathname)) ?? -1;

		if (tabIndex < 0) {
			const existingPaneIndex = panes.findIndex((pane) => pane.tabs.some((tab) => sameWorkspaceTab(tab, pathname)));
			if (existingPaneIndex >= 0) {
				paneIndex = existingPaneIndex;
				tabIndex = panes[paneIndex].tabs.findIndex((tab) => sameWorkspaceTab(tab, pathname));
			}
		}

		if (tabIndex >= 0) {
			const pane = panes[paneIndex];
			const existing = pane.tabs[tabIndex];
			const updated = decorateTab({ ...existing, ...describeWorkspacePath(pathname) });
			panes = panes.map((candidate, index) => index === paneIndex ? {
				...candidate,
				tabs: candidate.tabs.map((tab) => tab.id === existing.id ? updated : tab),
				activeTabId: existing.id,
			} : candidate);
			focusedPaneId = pane.id;
		} else {
			const tab = decorateTab(createWorkspaceTab(pathname));
			const pane = panes[paneIndex];
			panes = panes.map((candidate, index) => index === paneIndex
				? { ...candidate, tabs: [...candidate.tabs, tab], activeTabId: tab.id }
				: candidate);
			focusedPaneId = pane.id;
		}

		persistWorkspace();
		if (navigate && $page.url.pathname !== pathname) {
			lastSyncedPath = pathname;
			goto(pathname, { keepFocus: true, noScroll: true });
		}
	}

	function activateTab(paneId: string, tab: WorkspaceTab) {
		panes = panes.map((pane) => pane.id === paneId ? { ...pane, activeTabId: tab.id } : pane);
		focusedPaneId = paneId;
		persistWorkspace();
		if ($page.url.pathname !== tab.path) {
			lastSyncedPath = tab.path;
			goto(tab.path, { keepFocus: true, noScroll: true });
		}
	}

	function focusPane(paneId: string) {
		if (focusedPaneId === paneId) return;
		focusedPaneId = paneId;
		const pane = panes.find((candidate) => candidate.id === paneId);
		const tab = pane ? activeTabFor(pane) : null;
		persistWorkspace();
		if (tab && $page.url.pathname !== tab.path) {
			lastSyncedPath = tab.path;
			goto(tab.path, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	function closeTab(paneId: string, tab: WorkspaceTab) {
		const paneIndex = panes.findIndex((pane) => pane.id === paneId);
		if (paneIndex < 0) return;
		const pane = panes[paneIndex];
		const tabIndex = pane.tabs.findIndex((candidate) => candidate.id === tab.id);
		const wasActive = pane.activeTabId === tab.id;
		const remaining = pane.tabs.filter((candidate) => candidate.id !== tab.id);

		if (remaining.length === 0 && panes.length > 1) {
			panes = panes.filter((candidate) => candidate.id !== paneId);
			const nextPane = panes[Math.min(paneIndex, panes.length - 1)];
			focusedPaneId = nextPane.id;
		} else if (remaining.length === 0) {
			const home = createWorkspaceTab('/');
			panes = [{ ...pane, tabs: [home], activeTabId: home.id }];
			focusedPaneId = pane.id;
		} else {
			const nextActive = wasActive ? remaining[Math.min(tabIndex, remaining.length - 1)].id : pane.activeTabId;
			panes = panes.map((candidate) => candidate.id === paneId ? { ...candidate, tabs: remaining, activeTabId: nextActive } : candidate);
		}

		persistWorkspace();
		const nextFocusedPane = panes.find((candidate) => candidate.id === focusedPaneId) ?? panes[0];
		const nextTab = activeTabFor(nextFocusedPane);
		if ($page.url.pathname !== nextTab.path) {
			lastSyncedPath = nextTab.path;
			goto(nextTab.path, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	function splitPane(paneId: string) {
		if (panes.length >= MAX_PANES) return;
		if (workspaceEl && workspaceEl.clientWidth / (panes.length + 1) < 380) return;
		const paneIndex = panes.findIndex((pane) => pane.id === paneId);
		if (paneIndex < 0) return;
		const pane = panes[paneIndex];
		const source = activeTabFor(pane);
		const clone = { ...source, id: workspaceId('tab') };
		const nextPane: WorkspacePaneState = { id: workspaceId('pane'), tabs: [clone], activeTabId: clone.id, width: 1 };
		panes = [
			...panes.slice(0, paneIndex),
			pane,
			nextPane,
			...panes.slice(paneIndex + 1),
		].map((candidate) => ({ ...candidate, width: 1 }));
		focusedPaneId = nextPane.id;
		persistWorkspace();
	}

	function startResize(index: number, event: PointerEvent) {
		if (!workspaceEl || index < 0 || index >= panes.length - 1) return;
		event.preventDefault();
		const wrappers = workspaceEl.querySelectorAll<HTMLElement>('[data-workspace-pane]');
		const leftElement = wrappers[index];
		const rightElement = wrappers[index + 1];
		if (!leftElement || !rightElement) return;
		const startX = event.clientX;
		const leftPixels = leftElement.getBoundingClientRect().width;
		const rightPixels = rightElement.getBoundingClientRect().width;
		const totalPixels = leftPixels + rightPixels;
		const combinedWeight = panes[index].width + panes[index + 1].width;
		document.body.style.cursor = 'col-resize';
		document.body.style.userSelect = 'none';

		const move = (moveEvent: PointerEvent) => {
			const nextLeftPixels = Math.min(totalPixels - 280, Math.max(280, leftPixels + moveEvent.clientX - startX));
			const leftWeight = combinedWeight * nextLeftPixels / totalPixels;
			panes = panes.map((pane, paneIndex) => {
				if (paneIndex === index) return { ...pane, width: leftWeight };
				if (paneIndex === index + 1) return { ...pane, width: combinedWeight - leftWeight };
				return pane;
			});
		};
		const stop = () => {
			document.removeEventListener('pointermove', move);
			document.removeEventListener('pointerup', stop);
			document.body.style.cursor = '';
			document.body.style.userSelect = '';
			persistWorkspace();
		};
		document.addEventListener('pointermove', move);
		document.addEventListener('pointerup', stop);
	}

	function tabCategory(kind: WorkspaceTabKind | undefined): 'projects' | 'tasks' | 'workflows' | 'settings' {
		if (kind === 'home' || kind === 'projects' || kind === 'project') return 'projects';
		if (kind === 'tasks' || kind === 'task' || kind === 'schedules') return 'tasks';
		if (kind === 'workflows' || kind === 'workflow' || kind === 'workflow-new') return 'workflows';
		return 'settings';
	}

	function statusDot(status: string): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'bg-blue-500 animate-pulse';
		if (status === 'queued' || status === 'pending') return 'bg-amber-400';
		return 'bg-emerald-500';
	}

	function statusLabel(status: string): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'Needs attention';
		if (status === 'waiting_for_input') return 'Waiting for you';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'Working';
		if (status === 'queued' || status === 'pending') return 'Queued';
		return 'Ready';
	}

	function taskProject(task: Task): Agent | null {
		return agentList.find((agent) => agent.id === task.agent_id) ?? null;
	}

	function projectFocused(agent: Agent): boolean {
		return focusedProject?.id === agent.id;
	}

	function linkClass(active: boolean): string {
		return `flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm transition-colors ${active
			? 'bg-[hsl(var(--sidebar-active))] text-foreground font-medium'
			: 'text-muted-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] hover:text-foreground'}`;
	}

	async function loadWorkspaceSummary() {
		try {
			const [nextAgents, taskResult, nextWorkflows] = await Promise.all([
				agents.list().catch(() => agentList),
				tasksApi.list().catch(() => ({ tasks: taskList })),
				workflowsApi.list().catch(() => workflowList),
			]);
			agentList = nextAgents;
			taskList = taskResult.tasks;
			workflowList = nextWorkflows;
			refreshTabMetadata();
		} catch {}
	}

	async function checkDocker() {
		try {
			const response = await fetch('/api/setup/check-docker');
			const data = await response.json();
			dockerAvailable = data.available;
			dockerInstalled = data.installed;
			dockerCanStart = data.can_start;
		} catch {}
	}

	async function startDocker() {
		dockerStarting = true;
		try {
			await fetch('/api/setup/start-docker', { method: 'POST' });
			for (let attempt = 0; attempt < 30; attempt += 1) {
				await new Promise((resolve) => setTimeout(resolve, 2000));
				await checkDocker();
				if (dockerAvailable) break;
			}
		} catch {}
		dockerStarting = false;
	}

	async function checkConnection() {
		try {
			const response = await fetch('/api/health', { signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS) });
			if (response.ok) {
				consecutiveFailures = 0;
				if (wasDisconnected) {
					wasDisconnected = false;
					serverConnected = true;
					window.location.reload();
					return;
				}
				serverConnected = true;
				return;
			}
		} catch {}
		consecutiveFailures += 1;
		if (consecutiveFailures >= FAILURES_BEFORE_DISCONNECT) {
			serverConnected = false;
			wasDisconnected = true;
		}
	}

	onMount(() => {
		let restored = false;
		try {
			const saved = JSON.parse(localStorage.getItem(WORKSPACE_STORAGE_KEY) ?? 'null') as { panes?: unknown[]; focusedPaneId?: string } | null;
			const savedPanes = saved?.panes?.filter(validWorkspacePane).slice(0, MAX_PANES) ?? [];
			if (savedPanes.length > 0) {
				panes = savedPanes.map((pane) => ({
					...pane,
					tabs: pane.tabs.map((tab) => ({ ...tab, status: null, ...describeWorkspacePath(tab.path) })),
				}));
				focusedPaneId = panes.some((pane) => pane.id === saved?.focusedPaneId) ? saved!.focusedPaneId! : panes[0].id;
				restored = true;
			}
			sidebarCollapsed = localStorage.getItem('xpressclaw.sidebar.collapsed') === 'true';
		} catch {}

		workspaceReady = true;
		const pathname = $page.url.pathname;
		if (restored && pathname === '/') {
			const restoredTab = activeTabFor(panes.find((pane) => pane.id === focusedPaneId) ?? panes[0]);
			lastSyncedPath = restoredTab.path;
			if (restoredTab.path !== pathname) goto(restoredTab.path, { replaceState: true, noScroll: true });
		} else {
			lastSyncedPath = pathname;
			openPath(pathname, false);
		}

		loadWorkspaceSummary();
		checkDocker();
		const interval = setInterval(() => {
			loadWorkspaceSummary();
			checkConnection();
			if (!dockerAvailable) checkDocker();
		}, 3000);
		return () => clearInterval(interval);
	});
</script>

<div class="flex h-[100dvh] min-w-0 overflow-hidden">
	<aside class="hidden shrink-0 flex-col border-r border-border/60 transition-[width] duration-200 md:flex {sidebarCollapsed ? 'w-14' : 'w-64'}" style="background: hsl(var(--sidebar))">
		<div class="flex h-11 shrink-0 items-center {sidebarCollapsed ? 'justify-center' : 'gap-2 px-3'}">
			<img src="/icon-32.png" alt="" class="h-5 w-5 rounded" />
			{#if !sidebarCollapsed}<span class="min-w-0 flex-1 truncate text-xs font-medium text-muted-foreground">xpressclaw</span>{/if}
			<button type="button" onclick={() => { sidebarCollapsed = !sidebarCollapsed; persistWorkspace(); }} class="flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent/50 hover:text-foreground" title={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'} aria-label={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}>
				<svg class="h-3.5 w-3.5 {sidebarCollapsed ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5 8.25 12l7.5-7.5" /></svg>
			</button>
		</div>

		{#if sidebarCollapsed}
			<div class="flex flex-1 flex-col items-center gap-1 overflow-y-auto px-1.5 py-2">
				<a href="/" class="mb-2 flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-lg text-primary-foreground" title="New work">+</a>
				{#each agentList as agent (agent.id)}
					{@const status = projectStatus(agent)}
					<a href="/agents/{agent.id}" class="relative flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {projectFocused(agent) ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}" title="{agent.title || agent.name} — {statusLabel(status)}">
						{harnessMark(agent.backend)}
						<span class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {statusDot(status)}"></span>
					</a>
				{/each}
			</div>
		{:else}
			<div class="flex-1 overflow-y-auto px-2 pb-3">
				<a href="/" class="mb-3 flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90">
					<span class="text-base leading-none">+</span><span>New work</span>
				</a>

				{#if attentionTasks.length > 0}
					<div class="mb-4">
						<div class="mb-1.5 flex items-center justify-between px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-orange-400">
							<span>Needs you</span><span>{attentionTasks.length}</span>
						</div>
						<div class="space-y-0.5">
							{#each attentionTasks.slice(0, 5) as task (task.id)}
								{@const project = taskProject(task)}
								<a href="/tasks/{task.id}" class="group flex items-start gap-2 rounded-lg px-2 py-2 text-left hover:bg-accent/50">
									<span class="mt-1.5 h-2 w-2 shrink-0 rounded-full {task.status === 'blocked' ? 'bg-red-500' : 'bg-orange-500 animate-pulse'}"></span>
									<span class="min-w-0 flex-1"><span class="block truncate text-xs text-foreground">{task.title}</span><span class="mt-0.5 block truncate text-[10px] text-muted-foreground">{project?.title || project?.name || 'Unassigned'} · {timeAgo(task.updated_at)}</span></span>
								</a>
							{/each}
						</div>
					</div>
				{/if}

				<div class="mb-1.5 flex items-center justify-between px-2">
					<span class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Projects</span>
					<a href="/setup?mode=add-session" class="flex h-5 w-5 items-center justify-center rounded text-sm text-muted-foreground hover:bg-accent hover:text-foreground" title="Add project">+</a>
				</div>
				<div class="space-y-0.5">
					{#each agentList as agent (agent.id)}
						{@const status = projectStatus(agent)}
						<a href="/agents/{agent.id}" class={linkClass(projectFocused(agent))}>
							<span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-[10px] font-semibold">{harnessMark(agent.backend)}</span>
							<span class="min-w-0 flex-1"><span class="block truncate">{agent.title || agent.name}</span><span class="mt-0.5 block text-[10px] font-normal text-muted-foreground">{statusLabel(status)}</span></span>
							<span class="h-2 w-2 shrink-0 rounded-full {statusDot(status)}"></span>
						</a>
					{/each}
				</div>
			</div>
		{/if}

		<div class="shrink-0 border-t border-border/60 p-1.5">
			<div class="{sidebarCollapsed ? 'flex flex-col items-center gap-1' : 'grid grid-cols-4 gap-1'}">
				{#each utilityTabs as item}
					<a href={item.href} class="flex flex-col items-center gap-1 rounded-lg px-1 py-2 text-[9px] {tabCategory(focusedTab?.kind) === tabCategory(item.kind) ? 'text-primary' : 'text-muted-foreground hover:bg-accent/40 hover:text-foreground'}" title={item.label}>
						<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d={item.icon} /></svg>
						{#if !sidebarCollapsed}<span>{item.label}</span>{/if}
					</a>
				{/each}
			</div>
		</div>
	</aside>

	<main class="flex min-w-0 flex-1 flex-col overflow-hidden pb-16 md:pb-0">
		<div class="flex h-12 shrink-0 items-center gap-3 border-b border-border bg-background/95 px-3 backdrop-blur md:hidden">
			<button type="button" onclick={() => (mobileMenuOpen = true)} aria-label="Open project switcher" class="flex h-9 w-9 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground">
				<svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24"><path stroke-linecap="round" d="M4 6h16M4 12h16M4 18h16" /></svg>
			</button>
			<div class="min-w-0 flex-1"><div class="truncate text-sm font-semibold">{focusedTab?.title || focusedProject?.title || focusedProject?.name || 'xpressclaw'}</div>{#if focusedProject}<div class="flex items-center gap-1.5 text-[10px] text-muted-foreground"><span class="h-1.5 w-1.5 rounded-full {statusDot(projectStatus(focusedProject))}"></span>{statusLabel(projectStatus(focusedProject))}</div>{/if}</div>
			<a href="/" aria-label="New work" class="flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-lg text-primary-foreground">+</a>
		</div>

		{#if workspacePath($page.url.pathname)}
			<div class="flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-border bg-card/35 lg:hidden scrollbar-hide">
				{#each openTabs as item (item.tab.id)}
					<div class="group flex max-w-52 shrink-0 items-center border-r border-border/70 {item.paneId === focusedPaneId && item.tab.id === focusedPane?.activeTabId ? 'bg-background text-foreground' : 'text-muted-foreground'}">
						<button type="button" onclick={() => activateTab(item.paneId, item.tab)} class="flex min-w-0 flex-1 items-center gap-2 py-2 pl-3 text-xs">
							{#if item.tab.status}<span class="h-1.5 w-1.5 shrink-0 rounded-full {statusDot(item.tab.status)}"></span>{/if}<span class="truncate">{item.tab.title}</span>
						</button>
						<button type="button" onclick={() => closeTab(item.paneId, item.tab)} class="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded text-sm text-muted-foreground/60 hover:bg-accent hover:text-foreground" aria-label="Close {item.tab.title}">×</button>
					</div>
				{/each}
			</div>

			<div bind:this={workspaceEl} class="flex min-h-0 flex-1 overflow-hidden">
				{#each panes as pane, index (pane.id)}
					<div data-workspace-pane class="min-w-0 flex-col overflow-hidden {pane.id === focusedPaneId ? 'flex' : 'hidden'} lg:flex" style:flex={`${pane.width} 1 0%`}>
						<WorkspacePane
							{pane}
							focused={pane.id === focusedPaneId}
							compact={panes.length > 1}
							canSplit={panes.length < MAX_PANES && (!workspaceEl || workspaceEl.clientWidth / (panes.length + 1) >= 380)}
							onfocus={() => focusPane(pane.id)}
							onactivate={(tab) => activateTab(pane.id, tab)}
							onclose={(tab) => closeTab(pane.id, tab)}
							onsplit={() => splitPane(pane.id)}
						/>
					</div>
					{#if index < panes.length - 1}
						<button type="button" onpointerdown={(event) => startResize(index, event)} class="relative z-20 hidden w-1 shrink-0 cursor-col-resize border-x border-border/60 bg-card/50 hover:bg-primary/30 lg:block" aria-label="Resize panes"></button>
					{/if}
				{/each}
			</div>
		{:else}
			<div class="min-h-0 flex-1 overflow-auto">{@render children()}</div>
		{/if}
	</main>
</div>

{#if mobileMenuOpen}
	<div class="fixed inset-0 z-50 md:hidden">
		<button type="button" class="absolute inset-0 bg-black/60" aria-label="Close project switcher" onclick={() => (mobileMenuOpen = false)}></button>
		<aside class="absolute inset-y-0 left-0 flex w-[min(88vw,22rem)] flex-col border-r border-border p-3 shadow-2xl" style="background: hsl(var(--sidebar))">
			<div class="mb-3 flex h-9 items-center gap-2"><img src="/icon-32.png" alt="" class="h-6 w-6 rounded" /><span class="flex-1 text-sm font-semibold">Projects</span><button type="button" onclick={() => (mobileMenuOpen = false)} aria-label="Close" class="flex h-8 w-8 items-center justify-center rounded-lg text-xl text-muted-foreground hover:bg-accent">×</button></div>
			<a href="/" onclick={() => (mobileMenuOpen = false)} class="mb-3 flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-sm font-medium text-primary-foreground">+ New work</a>
			{#if attentionTasks.length > 0}
				<div class="mb-4"><div class="mb-1 px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-orange-400">Needs you</div>{#each attentionTasks.slice(0, 5) as task (task.id)}<a href="/tasks/{task.id}" onclick={() => (mobileMenuOpen = false)} class="flex items-center gap-2 rounded-lg px-2 py-2 hover:bg-accent"><span class="h-2 w-2 rounded-full {task.status === 'blocked' ? 'bg-red-500' : 'bg-orange-500'}"></span><span class="min-w-0 flex-1 truncate text-sm">{task.title}</span></a>{/each}</div>
			{/if}
			<div class="mb-1 px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Projects</div>
			<div class="flex-1 space-y-1 overflow-y-auto">
				{#each agentList as agent (agent.id)}
					{@const status = projectStatus(agent)}
					<a href="/agents/{agent.id}" onclick={() => (mobileMenuOpen = false)} class={linkClass(projectFocused(agent))}><span class="flex h-7 w-7 items-center justify-center rounded-lg bg-muted text-xs font-semibold">{harnessMark(agent.backend)}</span><span class="min-w-0 flex-1 truncate">{agent.title || agent.name}</span><span class="h-2 w-2 rounded-full {statusDot(status)}"></span></a>
				{/each}
			</div>
			<a href="/setup?mode=add-session" onclick={() => (mobileMenuOpen = false)} class="mt-3 rounded-lg border border-dashed border-border px-3 py-3 text-center text-sm text-muted-foreground">+ Add project</a>
		</aside>
	</div>
{/if}

<nav class="fixed inset-x-0 bottom-0 z-40 grid h-16 grid-cols-4 border-t border-border bg-background/95 pb-[env(safe-area-inset-bottom)] backdrop-blur md:hidden">
	{#each utilityTabs as item}
		<a href={item.href} class="flex flex-col items-center justify-center gap-1 text-[10px] {tabCategory(focusedTab?.kind) === tabCategory(item.kind) ? 'text-primary' : 'text-muted-foreground'}"><svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d={item.icon} /></svg><span>{item.label}</span></a>
	{/each}
</nav>

{#if !serverConnected}
	<div class="fixed inset-0 z-[200] flex items-center justify-center bg-black/70 backdrop-blur-sm"><div class="mx-4 w-full max-w-xs space-y-3 rounded-xl border border-border bg-card p-6 text-center shadow-2xl"><div class="inline-flex h-10 w-10 items-center justify-center rounded-full bg-amber-500/10"><svg class="h-5 w-5 animate-pulse text-amber-500" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M8.288 15.038a5.25 5.25 0 017.424 0M5.106 11.856c3.807-3.808 9.98-3.808 13.788 0M1.924 8.674c5.565-5.565 14.587-5.565 20.152 0M12 20.25h.008v.008H12v-.008z" /></svg></div><h3 class="text-sm font-semibold">Reconnecting…</h3><p class="text-xs text-muted-foreground">Lost connection to the server. XpressClaw will reconnect automatically.</p></div></div>
{/if}

{#if !dockerAvailable}
	<div class="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm"><div class="mx-4 w-full max-w-sm space-y-4 rounded-xl border border-border bg-card p-6 shadow-2xl"><div class="flex items-center gap-3"><div class="flex h-10 w-10 items-center justify-center rounded-full bg-amber-500/10"><svg class="h-5 w-5 text-amber-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" /></svg></div><div><h3 class="text-sm font-semibold">Container runtime is not running</h3><p class="text-xs text-muted-foreground">{dockerInstalled ? 'Start Docker or Podman to run queued work.' : 'Install Docker or Podman to run ACP workers.'}</p></div></div><div class="flex justify-end gap-2"><button type="button" onclick={() => (dockerAvailable = true)} class="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-secondary">Dismiss</button>{#if dockerCanStart}<button type="button" onclick={startDocker} disabled={dockerStarting} class="rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground disabled:opacity-50">{dockerStarting ? 'Starting…' : 'Start runtime'}</button>{/if}</div></div></div>
{/if}
