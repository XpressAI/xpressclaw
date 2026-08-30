<script lang="ts">
	import type { Snippet } from 'svelte';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { agents, conversations as conversationsApi, projects as projectsApi, request, schedules as schedulesApi, tasks as tasksApi, workflows as workflowsApi } from '$lib/api';
	import type { Agent, Conversation, Project, Schedule, Task, Workflow } from '$lib/api';
	import { PROJECT_CONTEXT_MENU_ITEMS, type ContextMenuItem } from '$lib/contextMenu';
	import { openWorkspaceWindow, WORKSPACE_WINDOW_PARAM } from '$lib/openWorkspaceWindow';
	import { PROJECT_MUTATION_EVENT, sortProjectsByRecency, type ProjectMutation } from '$lib/projectEvents';
	import { serverTimestampMs } from '$lib/serverTime';
	import { agentRuntimeSummary, agentRuntimeTitle, timeAgo } from '$lib/utils';
	import {
		createWorkspaceTab,
		describeWorkspacePath,
		projectPath,
		sameWorkspaceTab,
		statusPriority,
		validWorkspacePane,
		workspaceId,
		workspacePath,
		WORKSPACE_OPEN_SPLIT_EVENT,
		TASK_FILE_SPLIT_MIN_PANE_WIDTH,
		type WorkspacePaneState,
		type WorkspaceOpenSplitDetail,
		type ProjectSection,
		type WorkspaceTab,
		type WorkspaceTabKind,
	} from '$lib/workspace';
	import ContextMenu from '../ContextMenu.svelte';
	import SidebarSettings from './SidebarSettings.svelte';
	import SidebarTasks from './SidebarTasks.svelte';
	import SidebarAutomations from './SidebarAutomations.svelte';
	import SidebarProjects from './SidebarProjects.svelte';
	import WorkspacePane from './WorkspacePane.svelte';

	let { children }: { children: Snippet } = $props();
	type WorkspaceContextMenu =
		| { kind: 'project'; agent: Agent; x: number; y: number }
		| { kind: 'tab'; paneId: string; tab: WorkspaceTab; x: number; y: number };

	const WORKSPACE_WINDOW_SESSION_KEY = 'xpressclaw.workspace.window-id';
	const requestedWorkspaceWindowId = validWorkspaceWindowId($page.url.searchParams.get(WORKSPACE_WINDOW_PARAM));
	const workspaceWindowId = resolveWorkspaceWindowId(requestedWorkspaceWindowId);
	const WORKSPACE_STORAGE_KEY = workspaceWindowId
		? `xpressclaw.workspace.v1.${workspaceWindowId}`
		: 'xpressclaw.workspace.v1';
	const MAX_PANES = 4;
	const MAX_TABS = 10;
	const MIN_PANE_WIDTH = 380;
	let recencyClock = Date.now();
	const initialRoute = currentRoute();
	const initialDescription = describeWorkspacePath(initialRoute);
	const initialTab: WorkspaceTab = { id: 'initial-tab', status: null, lastActiveAt: nextTabRecency(), ...initialDescription };

	let panes = $state<WorkspacePaneState[]>([
		{ id: 'initial-pane', tabs: [initialTab], activeTabId: initialTab.id, width: 1 },
	]);
	let focusedPaneId = $state('initial-pane');
	let workspaceReady = $state(false);
	let lastSyncedPath = '';
	let workspaceEl = $state<HTMLDivElement>();
	let compactTabStrip = $state<HTMLDivElement>();
	let sidebarCollapsed = $state(false);
	let mobileMenuOpen = $state(false);
	let agentList = $state<Agent[]>([]);
	let projectList = $state<Project[]>([]);
	let conversationList = $state<Conversation[]>([]);
	let taskList = $state<Task[]>([]);
	let sidebarTaskList = $state<Task[]>([]);
	let workflowList = $state<Workflow[]>([]);
	let scheduleList = $state<Schedule[]>([]);
	let contextMenu = $state<WorkspaceContextMenu | null>(null);
	let projectMutationVersion = 0;
	const projectMutations = new Map<string, { version: number; mutation: ProjectMutation }>();

	let dockerAvailable = $state(true);
	let dockerInstalled = $state(true);
	let dockerCanStart = $state(false);
	let dockerStarting = $state(false);
	let serverConnected = $state(true);
	let connectionCheckInFlight = false;
	let firstConnectionFailureAt: number | null = null;
	const DISCONNECT_GRACE_MS = 12_000;
	const HEALTH_TIMEOUT_MS = 8000;

	let focusedPane = $derived(panes.find((pane) => pane.id === focusedPaneId) ?? panes[0]);
	let focusedTab = $derived(focusedPane?.tabs.find((tab) => tab.id === focusedPane.activeTabId) ?? focusedPane?.tabs[0] ?? null);
	let openTabs = $derived(panes.flatMap((pane) => pane.tabs.map((tab) => ({ paneId: pane.id, tab }))));
	let sidebarCategory = $derived(tabCategory(focusedTab?.kind));
	let sidebarTitle = $derived(sidebarCategory === 'tasks'
		? 'Tasks'
		: sidebarCategory === 'automations'
			? 'Automations'
			: sidebarCategory === 'settings' ? 'Settings' : 'Projects');
	let focusedTaskId = $derived(focusedTab?.kind === 'task' ? focusedTab.resourceId : null);
	let focusedWorkflowId = $derived(focusedTab?.kind === 'workflow' ? focusedTab.resourceId : null);
	let attentionTasks = $derived(taskList
		.filter((task) => task.status === 'waiting_for_input' || task.status === 'blocked')
		.sort((left, right) => statusPriority(right.status) - statusPriority(left.status)
			|| (serverTimestampMs(right.updated_at) ?? 0) - (serverTimestampMs(left.updated_at) ?? 0)));
	let focusedAgent = $derived((() => {
		if (!focusedTab) return null;
		if (focusedTab.kind === 'agent') return agentList.find((agent) => agent.id === focusedTab.resourceId) ?? null;
		if (focusedTab.kind === 'task') {
			const task = taskList.find((candidate) => candidate.id === focusedTab.resourceId)
				?? sidebarTaskList.find((candidate) => candidate.id === focusedTab.resourceId);
			return agentList.find((agent) => agent.id === task?.agent_id) ?? null;
		}
		return null;
	})());
	let focusedConversation = $derived(focusedTab?.kind === 'conversation'
		? conversationList.find((conversation) => conversation.id === focusedTab.resourceId) ?? null
		: null);
	let activeProjectId = $derived((() => {
		if (!focusedTab) return null;
		if (focusedTab.kind === 'project') return focusedTab.resourceId;
		if (focusedConversation) return focusedConversation.project_id;
		if (focusedAgent) return focusedAgent.project_id;
		if (focusedTab.kind === 'task') {
			const task = taskList.find((candidate) => candidate.id === focusedTab.resourceId)
				?? sidebarTaskList.find((candidate) => candidate.id === focusedTab.resourceId);
			const conversation = conversationList.find((candidate) => candidate.id === task?.conversation_id);
			return task?.project_id ?? conversation?.project_id ?? agentList.find((agent) => agent.id === task?.agent_id)?.project_id ?? null;
		}
		return null;
	})());
	let focusedProject = $derived(projectList.find((project) => project.id === activeProjectId) ?? null);

	const utilityTabs: { kind: WorkspaceTabKind; label: string; href: string; icon: string }[] = [
		{ kind: 'projects', label: 'Projects', href: '/projects', icon: 'M3.75 6.75v10.5A2.25 2.25 0 006 19.5h12a2.25 2.25 0 002.25-2.25V8.25A2.25 2.25 0 0018 6h-5.25L10.5 3.75H6A2.25 2.25 0 003.75 6z' },
		{ kind: 'tasks', label: 'Tasks', href: '/tasks', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4' },
		{ kind: 'automations', label: 'Automations', href: '/automations', icon: 'M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5' },
		{ kind: 'settings', label: 'Settings', href: '/settings', icon: 'M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z M15 12a3 3 0 11-6 0 3 3 0 016 0z' },
	];

	$effect(() => {
		const route = currentRoute();
		if (!workspaceReady || !workspacePath(route) || route === lastSyncedPath) return;
		lastSyncedPath = route;
		openPath(route, false);
	});

	$effect(() => {
		focusedPaneId;
		focusedPane?.activeTabId;
		if (!compactTabStrip) return;
		const frame = window.requestAnimationFrame(() => {
			compactTabStrip
				?.querySelector<HTMLElement>('[data-workspace-tab-active="true"]')
				?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		});
		return () => window.cancelAnimationFrame(frame);
	});

	function validWorkspaceWindowId(value: string | null): string | null {
		return value && /^workspace-\d+-\d+$/.test(value) ? value : null;
	}

	function resolveWorkspaceWindowId(requestedId: string | null): string | null {
		try {
			if (requestedId) sessionStorage.setItem(WORKSPACE_WINDOW_SESSION_KEY, requestedId);
			return requestedId ?? validWorkspaceWindowId(sessionStorage.getItem(WORKSPACE_WINDOW_SESSION_KEY));
		} catch {
			return null;
		}
	}

	function currentRoute(): string {
		const search = new URLSearchParams($page.url.search);
		search.delete(WORKSPACE_WINDOW_PARAM);
		const query = search.toString();
		return `${$page.url.pathname}${query ? `?${query}` : ''}${$page.url.hash}`;
	}

	function workspaceStateStorage(): Storage {
		return workspaceWindowId ? sessionStorage : localStorage;
	}

	function activeTabFor(pane: WorkspacePaneState): WorkspaceTab {
		return pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0];
	}

	function nextTabRecency(): number {
		recencyClock = Math.max(Date.now(), recencyClock + 1);
		return recencyClock;
	}

	function tabKey(paneId: string, tabId: string): string {
		return `${paneId}:${tabId}`;
	}

	function enforceTabLimit(nextPanes: WorkspacePaneState[]): WorkspacePaneState[] {
		const openCount = nextPanes.reduce((count, pane) => count + pane.tabs.length, 0);
		if (openCount <= MAX_TABS) return nextPanes;

		const activeKeys = new Set(nextPanes.map((pane) => tabKey(pane.id, activeTabFor(pane).id)));
		let order = 0;
		const evictionCandidates = nextPanes
			.flatMap((pane) => pane.tabs.map((tab) => ({ paneId: pane.id, tab, order: order++ })))
			.filter((item) => !activeKeys.has(tabKey(item.paneId, item.tab.id)))
			.sort((left, right) => left.tab.lastActiveAt - right.tab.lastActiveAt || left.order - right.order);
		const evicted = new Set(
			evictionCandidates
				.slice(0, openCount - MAX_TABS)
				.map((item) => tabKey(item.paneId, item.tab.id)),
		);

		return nextPanes.map((pane) => ({
			...pane,
			tabs: pane.tabs.filter((tab) => !evicted.has(tabKey(pane.id, tab.id))),
		}));
	}

	function persistWorkspace() {
		if (!workspaceReady) return;
		try {
			workspaceStateStorage().setItem(WORKSPACE_STORAGE_KEY, JSON.stringify({ panes, focusedPaneId }));
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
			const task = taskList.find((candidate) => candidate.id === description.resourceId)
				?? sidebarTaskList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: task?.title ?? tab.title ?? 'Task', status: task?.status ?? tab.status };
		}
		if (description.kind === 'project') {
			const project = projectList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: project?.name || tab.title || 'Project', status: null };
		}
		if (description.kind === 'agent') {
			const agent = agentList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: agent?.title || agent?.name || tab.title || 'Agent', status: agent ? projectStatus(agent) : tab.status };
		}
		if (description.kind === 'conversation') {
			const conversation = conversationList.find((candidate) => candidate.id === description.resourceId);
			return { ...tab, ...description, title: conversation?.title || tab.title || 'Conversation', status: null };
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

	function openPath(route: string, navigate = true) {
		if (!workspacePath(route)) return;
		let paneIndex = Math.max(0, panes.findIndex((pane) => pane.id === focusedPaneId));
		let tabIndex = panes[paneIndex]?.tabs.findIndex((tab) => sameWorkspaceTab(tab, route)) ?? -1;

		if (tabIndex < 0) {
			const existingPaneIndex = panes.findIndex((pane) => pane.tabs.some((tab) => sameWorkspaceTab(tab, route)));
			if (existingPaneIndex >= 0) {
				paneIndex = existingPaneIndex;
				tabIndex = panes[paneIndex].tabs.findIndex((tab) => sameWorkspaceTab(tab, route));
			}
		}

		if (tabIndex >= 0) {
			const pane = panes[paneIndex];
			const existing = pane.tabs[tabIndex];
			const updated = decorateTab({ ...existing, ...describeWorkspacePath(route), lastActiveAt: nextTabRecency() });
			panes = panes.map((candidate, index) => index === paneIndex ? {
				...candidate,
				tabs: candidate.tabs.map((tab) => tab.id === existing.id ? updated : tab),
				activeTabId: existing.id,
			} : candidate);
			focusedPaneId = pane.id;
		} else {
			const tab = decorateTab({ ...createWorkspaceTab(route), lastActiveAt: nextTabRecency() });
			const pane = panes[paneIndex];
			panes = panes.map((candidate, index) => index === paneIndex
				? { ...candidate, tabs: [...candidate.tabs, tab], activeTabId: tab.id }
				: candidate);
			focusedPaneId = pane.id;
		}
		panes = enforceTabLimit(panes);

		persistWorkspace();
		if (navigate && currentRoute() !== route) {
			lastSyncedPath = route;
			goto(route, { keepFocus: true, noScroll: true });
		}
	}

	function activateTab(paneId: string, tab: WorkspaceTab) {
		const activatedAt = nextTabRecency();
		panes = panes.map((pane) => pane.id === paneId ? {
			...pane,
			activeTabId: tab.id,
			tabs: pane.tabs.map((candidate) => candidate.id === tab.id ? { ...candidate, lastActiveAt: activatedAt } : candidate),
		} : pane);
		focusedPaneId = paneId;
		persistWorkspace();
		if (currentRoute() !== tab.path) {
			lastSyncedPath = tab.path;
			goto(tab.path, { keepFocus: true, noScroll: true });
		}
	}

	function focusPane(paneId: string) {
		if (focusedPaneId === paneId) return;
		focusedPaneId = paneId;
		const pane = panes.find((candidate) => candidate.id === paneId);
		const tab = pane ? activeTabFor(pane) : null;
		if (tab) {
			const activatedAt = nextTabRecency();
			panes = panes.map((candidate) => candidate.id === paneId ? {
				...candidate,
				tabs: candidate.tabs.map((paneTab) => paneTab.id === tab.id ? { ...paneTab, lastActiveAt: activatedAt } : paneTab),
			} : candidate);
		}
		persistWorkspace();
		if (tab && currentRoute() !== tab.path) {
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
			const home = { ...createWorkspaceTab('/'), lastActiveAt: nextTabRecency() };
			panes = [{ ...pane, tabs: [home], activeTabId: home.id }];
			focusedPaneId = pane.id;
		} else {
			const nextActive = wasActive ? remaining[Math.min(tabIndex, remaining.length - 1)].id : pane.activeTabId;
			panes = panes.map((candidate) => candidate.id === paneId ? { ...candidate, tabs: remaining, activeTabId: nextActive } : candidate);
		}

		const nextFocusedPane = panes.find((candidate) => candidate.id === focusedPaneId) ?? panes[0];
		const nextTab = activeTabFor(nextFocusedPane);
		if (wasActive) {
			const activatedAt = nextTabRecency();
			panes = panes.map((candidate) => candidate.id === nextFocusedPane.id ? {
				...candidate,
				tabs: candidate.tabs.map((paneTab) => paneTab.id === nextTab.id ? { ...paneTab, lastActiveAt: activatedAt } : paneTab),
			} : candidate);
		}
		persistWorkspace();
		if (currentRoute() !== nextTab.path) {
			lastSyncedPath = nextTab.path;
			goto(nextTab.path, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	function closeOtherTabs(paneId: string, tab: WorkspaceTab) {
		const pane = panes.find((candidate) => candidate.id === paneId);
		const target = pane?.tabs.find((candidate) => candidate.id === tab.id);
		if (!pane || !target || pane.tabs.length <= 1) return;

		const activatedTarget = { ...target, lastActiveAt: nextTabRecency() };
		panes = panes.map((candidate) => candidate.id === paneId
			? { ...candidate, tabs: [activatedTarget], activeTabId: activatedTarget.id }
			: candidate);
		focusedPaneId = paneId;
		persistWorkspace();
		if (currentRoute() !== activatedTarget.path) {
			lastSyncedPath = activatedTarget.path;
			goto(activatedTarget.path, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	function closeAllTabs(paneId: string) {
		const paneIndex = panes.findIndex((pane) => pane.id === paneId);
		if (paneIndex < 0) return;

		if (panes.length > 1) {
			panes = panes.filter((pane) => pane.id !== paneId);
			focusedPaneId = panes[Math.min(paneIndex, panes.length - 1)].id;
		} else {
			const pane = panes[0];
			const home = { ...createWorkspaceTab('/'), lastActiveAt: nextTabRecency() };
			panes = [{ ...pane, tabs: [home], activeTabId: home.id }];
			focusedPaneId = pane.id;
		}

		const nextPane = panes.find((pane) => pane.id === focusedPaneId) ?? panes[0];
		const nextTab = activeTabFor(nextPane);
		const activatedAt = nextTabRecency();
		panes = panes.map((pane) => pane.id === nextPane.id ? {
			...pane,
			tabs: pane.tabs.map((tab) => tab.id === nextTab.id ? { ...tab, lastActiveAt: activatedAt } : tab),
		} : pane);
		persistWorkspace();
		if (currentRoute() !== nextTab.path) {
			lastSyncedPath = nextTab.path;
			goto(nextTab.path, { replaceState: true, keepFocus: true, noScroll: true });
		}
	}

	function showProjectContextMenu(event: MouseEvent, agent: Agent) {
		event.preventDefault();
		event.stopPropagation();
		contextMenu = { kind: 'project', agent, x: event.clientX, y: event.clientY };
	}

	function showTabContextMenu(event: MouseEvent, paneId: string, tab: WorkspaceTab) {
		event.preventDefault();
		event.stopPropagation();
		contextMenu = { kind: 'tab', paneId, tab, x: event.clientX, y: event.clientY };
	}

	function contextMenuItems(target: WorkspaceContextMenu): ContextMenuItem[] {
		if (target.kind === 'project') return PROJECT_CONTEXT_MENU_ITEMS;
		const pane = panes.find((candidate) => candidate.id === target.paneId);
		return [
			{ id: 'close-tab', label: 'Close Tab' },
			{ id: 'close-other-tabs', label: 'Close Other Tabs', disabled: !pane || pane.tabs.length <= 1 },
			{ id: 'close-all-tabs', label: 'Close All Tabs' },
			{ id: 'open-new-window', label: 'Open in New Window', separatorBefore: true },
		];
	}

	function launchWorkspaceWindow(path: string, title: string) {
		void openWorkspaceWindow(path, title).catch((error) => {
			console.error('failed to open workspace window', error);
			window.alert(error instanceof Error ? error.message : 'Could not open the window.');
		});
	}

	function selectContextMenuItem(target: WorkspaceContextMenu, action: string) {
		if (target.kind === 'tab') {
			if (action === 'close-tab') closeTab(target.paneId, target.tab);
			else if (action === 'close-other-tabs') closeOtherTabs(target.paneId, target.tab);
			else if (action === 'close-all-tabs') closeAllTabs(target.paneId);
			else if (action === 'open-new-window') launchWorkspaceWindow(target.tab.path, target.tab.title);
			return;
		}

		mobileMenuOpen = false;
		if (action === 'open-new-window') {
			launchWorkspaceWindow(projectPath(target.agent.id), target.agent.title || target.agent.name);
			return;
		}

		const sections: Record<string, ProjectSection> = {
			'open-tasks': 'tasks',
			'open-schedules': 'schedules',
			'open-runner': 'runner',
			'open-workspace': 'workspace',
		};
		const section = sections[action];
		if (section) openPath(projectPath(target.agent.id, section));
	}

	function splitPane(paneId: string) {
		if (!canCreatePane()) return;
		const paneIndex = panes.findIndex((pane) => pane.id === paneId);
		if (paneIndex < 0) return;
		const pane = panes[paneIndex];
		const source = activeTabFor(pane);
		const clone = { ...source, id: workspaceId('tab'), lastActiveAt: nextTabRecency() };
		const nextPane: WorkspacePaneState = { id: workspaceId('pane'), tabs: [clone], activeTabId: clone.id, width: 1 };
		panes = [
			...panes.slice(0, paneIndex),
			pane,
			nextPane,
			...panes.slice(paneIndex + 1),
		].map((candidate) => ({ ...candidate, width: 1 }));
		focusedPaneId = nextPane.id;
		panes = enforceTabLimit(panes);
		persistWorkspace();
	}

	function canCreatePane(minimumPaneWidth = MIN_PANE_WIDTH): boolean {
		return panes.length < MAX_PANES
			&& (!workspaceEl || workspaceEl.clientWidth / (panes.length + 1) >= minimumPaneWidth);
	}

	function openPathInSplit(route: string) {
		if (!workspacePath(route)) return;
		if (!canCreatePane(TASK_FILE_SPLIT_MIN_PANE_WIDTH)) {
			openPath(route);
			return;
		}
		const paneIndex = Math.max(0, panes.findIndex((pane) => pane.id === focusedPaneId));
		const tab = decorateTab({ ...createWorkspaceTab(route), lastActiveAt: nextTabRecency() });
		const nextPane: WorkspacePaneState = { id: workspaceId('pane'), tabs: [tab], activeTabId: tab.id, width: 1 };
		panes = [
			...panes.slice(0, paneIndex + 1),
			nextPane,
			...panes.slice(paneIndex + 1),
		].map((candidate) => ({ ...candidate, width: 1 }));
		focusedPaneId = nextPane.id;
		panes = enforceTabLimit(panes);
		persistWorkspace();
		if (currentRoute() !== route) {
			lastSyncedPath = route;
			goto(route, { keepFocus: true, noScroll: true });
		}
	}

	function handleOpenSplit(event: Event) {
		openPathInSplit((event as CustomEvent<WorkspaceOpenSplitDetail>).detail.path);
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

	function tabCategory(kind: WorkspaceTabKind | undefined): 'projects' | 'tasks' | 'automations' | 'settings' {
		if (kind === 'home' || kind === 'dashboard' || kind === 'projects' || kind === 'project' || kind === 'agents' || kind === 'agent' || kind === 'conversation') return 'projects';
		if (kind === 'tasks' || kind === 'task') return 'tasks';
		if (kind === 'automations' || kind === 'schedules' || kind === 'workflows' || kind === 'workflow' || kind === 'workflow-new') return 'automations';
		return 'settings';
	}

	function statusDot(status: string): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'bg-blue-500 animate-pulse';
		if (status === 'queued' || status === 'pending') return 'bg-amber-400';
		return 'bg-emerald-500';
	}

	function taskAgent(task: Task): Agent | null {
		return agentList.find((agent) => agent.id === task.agent_id) ?? null;
	}

	async function loadWorkspaceSummary() {
		const startingProjectMutationVersion = projectMutationVersion;
		try {
			const [nextProjects, nextConversations, nextAgents, taskResult, sidebarTaskResult, nextWorkflows, nextSchedules] = await Promise.all([
				projectsApi.list().catch(() => projectList),
				conversationsApi.list(undefined, 200).catch(() => conversationList),
				agents.list().catch(() => agentList),
				tasksApi.list().catch(() => ({ tasks: taskList })),
				tasksApi.recentByAgent().catch(() => null),
				workflowsApi.list().catch(() => workflowList),
				schedulesApi.list().catch(() => scheduleList),
			]);
			projectList = [...projectMutations.values()]
				.filter(({ version }) => version > startingProjectMutationVersion)
				.reduce((list, { mutation }) => applyProjectMutation(list, mutation), nextProjects);
			conversationList = nextConversations;
			agentList = nextAgents;
			taskList = taskResult.tasks;
			sidebarTaskList = sidebarTaskResult?.tasks ?? taskResult.tasks;
			workflowList = nextWorkflows;
			scheduleList = nextSchedules;
			refreshTabMetadata();
		} catch {}
	}

	function handleProjectMutation(event: Event) {
		const mutation = (event as CustomEvent<ProjectMutation>).detail;
		if (!mutation || (mutation.kind !== 'updated' && mutation.kind !== 'deleted')) return;
		projectMutationVersion += 1;
		const projectId = mutation.kind === 'updated' ? mutation.project.id : mutation.projectId;
		projectMutations.set(projectId, { version: projectMutationVersion, mutation });
		if (mutation.kind === 'updated') {
			projectList = applyProjectMutation(projectList, mutation);
			refreshTabMetadata();
			return;
		}
		const focusedProjectWasDeleted = focusedTab?.kind === 'project' && focusedTab.resourceId === mutation.projectId;
		projectList = projectList.filter((project) => project.id !== mutation.projectId);
		panes = panes.map((pane) => {
			const tabs = pane.tabs.filter((tab) => !(tab.kind === 'project' && tab.resourceId === mutation.projectId));
			if (tabs.length > 0) {
				return {
					...pane,
					tabs,
					activeTabId: tabs.some((tab) => tab.id === pane.activeTabId) ? pane.activeTabId : tabs[0].id,
				};
			}
			const fallback = { ...createWorkspaceTab('/projects'), lastActiveAt: nextTabRecency() };
			return { ...pane, tabs: [fallback], activeTabId: fallback.id };
		});
		persistWorkspace();
		if (focusedProjectWasDeleted) {
			const nextPane = panes.find((pane) => pane.id === focusedPaneId) ?? panes[0];
			const nextTab = activeTabFor(nextPane);
			if (currentRoute() !== nextTab.path) {
				lastSyncedPath = nextTab.path;
				void goto(nextTab.path, { replaceState: true, keepFocus: true, noScroll: true });
			}
		}
	}

	function applyProjectMutation(list: Project[], mutation: ProjectMutation): Project[] {
		if (mutation.kind === 'deleted') {
			return list.filter((project) => project.id !== mutation.projectId);
		}
		if (list.some((project) => project.id === mutation.project.id)) {
			return sortProjectsByRecency(
				list.map((project) => project.id === mutation.project.id ? mutation.project : project),
			);
		}
		return mutation.authoritative ? sortProjectsByRecency([...list, mutation.project]) : list;
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
			await request<void>('/api/setup/start-docker', { method: 'POST', body: '{}' });
			for (let attempt = 0; attempt < 30; attempt += 1) {
				await new Promise((resolve) => setTimeout(resolve, 2000));
				await checkDocker();
				if (dockerAvailable) break;
			}
		} catch {}
		dockerStarting = false;
	}

	async function checkConnection() {
		if (connectionCheckInFlight) return;
		connectionCheckInFlight = true;
		let connected = false;
		try {
			const response = await fetch('/api/health', { signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS) });
			connected = response.ok;
		} catch {}
		connectionCheckInFlight = false;

		if (connected) {
			const recovered = !serverConnected;
			firstConnectionFailureAt = null;
			serverConnected = true;
			if (recovered) void loadWorkspaceSummary();
			return;
		}

		firstConnectionFailureAt ??= Date.now();
		if (Date.now() - firstConnectionFailureAt >= DISCONNECT_GRACE_MS) serverConnected = false;
	}

	onMount(() => {
		let restored = false;
		try {
			if (requestedWorkspaceWindowId) {
				window.history.replaceState(window.history.state, '', currentRoute());
			}
			const saved = JSON.parse(workspaceStateStorage().getItem(WORKSPACE_STORAGE_KEY) ?? 'null') as { panes?: unknown[]; focusedPaneId?: string } | null;
			const savedPanes = saved?.panes?.filter(validWorkspacePane).slice(0, MAX_PANES) ?? [];
			if (savedPanes.length > 0) {
				let fallbackRecency = Date.now() - savedPanes.reduce((count, pane) => count + pane.tabs.length, 0);
				const restoredPanes = savedPanes.map((pane) => {
					const tabs = pane.tabs.map((tab) => ({
						...tab,
						status: null,
						lastActiveAt: Number.isFinite(tab.lastActiveAt) ? tab.lastActiveAt : fallbackRecency++,
						...describeWorkspacePath(tab.path),
					}));
					return {
						...pane,
						tabs,
						activeTabId: tabs.some((tab) => tab.id === pane.activeTabId) ? pane.activeTabId : tabs[0].id,
					};
				});
				recencyClock = Math.max(recencyClock, ...restoredPanes.flatMap((pane) => pane.tabs.map((tab) => tab.lastActiveAt)));
				panes = enforceTabLimit(restoredPanes);
				focusedPaneId = panes.some((pane) => pane.id === saved?.focusedPaneId) ? saved!.focusedPaneId! : panes[0].id;
				restored = true;
			}
			sidebarCollapsed = localStorage.getItem('xpressclaw.sidebar.collapsed') === 'true';
		} catch {}

		workspaceReady = true;
		persistWorkspace();
		const route = currentRoute();
		if (restored && route === '/') {
			const restoredTab = activeTabFor(panes.find((pane) => pane.id === focusedPaneId) ?? panes[0]);
			lastSyncedPath = restoredTab.path;
			if (restoredTab.path !== route) goto(restoredTab.path, { replaceState: true, noScroll: true });
		} else {
			lastSyncedPath = route;
			openPath(route, false);
		}

		loadWorkspaceSummary();
		checkDocker();
		const handleOnline = () => void checkConnection();
		window.addEventListener('online', handleOnline);
		window.addEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
		window.addEventListener(WORKSPACE_OPEN_SPLIT_EVENT, handleOpenSplit);
		const interval = setInterval(() => {
			loadWorkspaceSummary();
			checkConnection();
			if (!dockerAvailable) checkDocker();
		}, 3000);
		return () => {
			clearInterval(interval);
			window.removeEventListener('online', handleOnline);
			window.removeEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
			window.removeEventListener(WORKSPACE_OPEN_SPLIT_EVENT, handleOpenSplit);
		};
	});
</script>

<div class="flex h-[100dvh] min-w-0 overflow-hidden">
	<aside class="hidden shrink-0 flex-col border-r border-border/60 transition-[width] duration-200 md:flex {sidebarCollapsed ? 'w-14' : 'w-64'}" style="background: hsl(var(--sidebar))">
		<div class="flex h-11 shrink-0 items-center {sidebarCollapsed ? 'justify-center' : 'gap-2 px-3'}">
			<a href="/dashboard" class="flex min-w-0 items-center gap-2 rounded-md outline-none transition hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50" aria-label="Open Control center" title="Control center">
				<img src="/icon-32.png" alt="" class="h-5 w-5 rounded" />
				{#if !sidebarCollapsed}<span class="min-w-0 flex-1 truncate text-xs font-semibold text-muted-foreground">xpressclaw</span>{/if}
			</a>
			{#if !sidebarCollapsed}<span class="min-w-0 flex-1"></span>{/if}
			<button type="button" onclick={() => { sidebarCollapsed = !sidebarCollapsed; persistWorkspace(); }} class="flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent/50 hover:text-foreground" title={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'} aria-label={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}>
				<svg class="h-3.5 w-3.5 {sidebarCollapsed ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5 8.25 12l7.5-7.5" /></svg>
			</button>
		</div>

		{#if sidebarCollapsed}
			<div class="flex min-h-0 flex-1 flex-col items-center gap-1 overflow-y-auto px-1.5 py-2">
				<a href="/" class="mb-2 flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-lg text-primary-foreground" title="New work">+</a>
				{#if sidebarCategory === 'tasks'}
					<SidebarTasks {projectList} {conversationList} {agentList} taskList={sidebarTaskList} activeTaskId={focusedTaskId} compact />
				{:else if sidebarCategory === 'automations'}
					<SidebarAutomations {workflowList} {scheduleList} activeWorkflowId={focusedWorkflowId} compact />
				{:else if sidebarCategory === 'settings'}
					<SidebarSettings activeKind={focusedTab?.kind ?? 'settings'} compact />
				{:else}
					<SidebarProjects {projectList} {conversationList} {agentList} taskList={sidebarTaskList} {activeProjectId} activeConversationId={focusedConversation?.id ?? null} activeAgentId={focusedAgent?.id ?? null} compact onagentcontext={showProjectContextMenu} />
				{/if}
			</div>
		{:else}
			<div class="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
				<a href="/" class="mb-3 flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90">
					<span class="text-base leading-none">+</span><span>New work</span>
				</a>

				{#if sidebarCategory === 'tasks'}
					<SidebarTasks {projectList} {conversationList} {agentList} taskList={sidebarTaskList} activeTaskId={focusedTaskId} />
				{:else if sidebarCategory === 'automations'}
					<SidebarAutomations {workflowList} {scheduleList} activeWorkflowId={focusedWorkflowId} />
				{:else if sidebarCategory === 'settings'}
					<SidebarSettings activeKind={focusedTab?.kind ?? 'settings'} />
				{:else}
					{#if attentionTasks.length > 0}
						<div class="mb-4">
							<div class="mb-1.5 flex items-center justify-between px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-orange-400">
								<span>Needs you</span><span>{attentionTasks.length}</span>
							</div>
							<div class="space-y-0.5">
								{#each attentionTasks.slice(0, 5) as task (task.id)}
									{@const agent = taskAgent(task)}
									<a href="/tasks/{task.id}" class="group flex items-start gap-2 rounded-lg px-2 py-2 text-left hover:bg-accent/50">
										<span class="mt-1.5 h-2 w-2 shrink-0 rounded-full {task.status === 'blocked' ? 'bg-red-500' : 'bg-orange-500 animate-pulse'}"></span>
										<span class="min-w-0 flex-1"><span class="block truncate text-xs text-foreground">{task.title}</span><span class="mt-0.5 block truncate text-[10px] text-muted-foreground">{agent?.title || agent?.name || 'Unassigned'} · {timeAgo(task.updated_at)}</span></span>
									</a>
								{/each}
							</div>
						</div>
					{/if}

					<SidebarProjects {projectList} {conversationList} {agentList} taskList={sidebarTaskList} {activeProjectId} activeConversationId={focusedConversation?.id ?? null} activeAgentId={focusedAgent?.id ?? null} onagentcontext={showProjectContextMenu} />
				{/if}
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
			<button type="button" onclick={() => (mobileMenuOpen = true)} aria-label="Open agent switcher" class="flex h-9 w-9 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground">
				<svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24"><path stroke-linecap="round" d="M4 6h16M4 12h16M4 18h16" /></svg>
			</button>
			<a href="/dashboard" aria-label="Open Control center" title="Control center" class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"><img src="/icon-32.png" alt="" class="h-5 w-5 rounded" /></a>
			<div class="min-w-0 flex-1">
				<div class="truncate text-sm font-semibold">{focusedTab?.title || focusedProject?.name || 'xpressclaw'}</div>
				{#if focusedAgent}
					<div class="flex min-w-0 items-center gap-1.5 text-[10px] text-muted-foreground" title={agentRuntimeTitle(focusedAgent)}><span class="h-1.5 w-1.5 shrink-0 rounded-full {statusDot(projectStatus(focusedAgent))}"></span><span class="truncate">{agentRuntimeSummary(focusedAgent)}</span></div>
				{:else if focusedProject}
					<div class="truncate text-[10px] text-muted-foreground">{focusedProject.conversation_count} conversations · {focusedProject.agent_ids.length} Agents</div>
				{/if}
			</div>
			<a href="/" aria-label="New work" class="flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-lg text-primary-foreground">+</a>
		</div>

		{#if workspacePath($page.url.pathname)}
			<div bind:this={compactTabStrip} data-workspace-tab-strip class="flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-border bg-[hsl(var(--field))] lg:hidden scrollbar-hide">
				{#each openTabs as item (item.tab.id)}
					{@const isActive = item.paneId === focusedPaneId && item.tab.id === focusedPane?.activeTabId}
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div
						data-workspace-tab
						data-workspace-tab-title={item.tab.title}
						data-workspace-tab-active={isActive}
						oncontextmenu={(event) => showTabContextMenu(event, item.paneId, item.tab)}
						class="group relative flex max-w-52 shrink-0 items-center border-r border-border/70 transition-colors {isActive ? 'bg-card font-semibold text-primary shadow-[inset_0_0_0_1px_hsl(var(--border-strong))]' : 'text-muted-foreground hover:bg-[hsl(var(--hover))] hover:text-foreground'}"
					>
						<button type="button" onclick={() => activateTab(item.paneId, item.tab)} aria-current={isActive ? 'page' : undefined} class="flex min-w-0 flex-1 items-center gap-2 py-2 pl-3 text-xs">
							{#if item.tab.status}<span class="h-1.5 w-1.5 shrink-0 rounded-full {statusDot(item.tab.status)}"></span>{/if}<span class="truncate">{item.tab.title}</span>
						</button>
						<button type="button" onclick={() => closeTab(item.paneId, item.tab)} class="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded text-sm text-muted-foreground/60 hover:bg-accent hover:text-foreground" aria-label="Close {item.tab.title}">×</button>
						{#if isActive}<span data-active-tab-indicator class="pointer-events-none absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-primary" aria-hidden="true"></span>{/if}
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
							canSplit={canCreatePane()}
							onfocus={() => focusPane(pane.id)}
							onactivate={(tab) => activateTab(pane.id, tab)}
							onclose={(tab) => closeTab(pane.id, tab)}
							oncontext={(event, tab) => showTabContextMenu(event, pane.id, tab)}
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
		<button type="button" class="absolute inset-0 bg-black/60" aria-label="Close agent switcher" onclick={() => (mobileMenuOpen = false)}></button>
		<aside class="absolute inset-y-0 left-0 flex min-h-0 w-[min(88vw,22rem)] flex-col overflow-hidden border-r border-border p-3 shadow-2xl" style="background: hsl(var(--sidebar))">
			<div class="mb-3 flex h-9 shrink-0 items-center gap-2"><a href="/dashboard" onclick={() => (mobileMenuOpen = false)} class="flex min-w-0 items-center gap-2 rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50" aria-label="Open Control center"><img src="/icon-32.png" alt="" class="h-6 w-6 rounded" /><span class="text-sm font-semibold">xpressclaw</span></a><span class="min-w-0 flex-1 truncate text-right text-xs text-muted-foreground">{sidebarTitle}</span><button type="button" onclick={() => (mobileMenuOpen = false)} aria-label="Close" class="flex h-8 w-8 items-center justify-center rounded-lg text-xl text-muted-foreground hover:bg-accent">×</button></div>
			<a href="/" onclick={() => (mobileMenuOpen = false)} class="mb-3 flex shrink-0 items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2 text-sm font-medium text-primary-foreground">+ New work</a>
			{#if sidebarCategory === 'tasks'}
				<div data-mobile-sidebar-scroll class="workspace-scroll-y flex-1">
					<SidebarTasks {projectList} {conversationList} {agentList} taskList={sidebarTaskList} activeTaskId={focusedTaskId} showHeading={false} onnavigate={() => (mobileMenuOpen = false)} />
				</div>
			{:else if sidebarCategory === 'automations'}
				<div data-mobile-sidebar-scroll class="workspace-scroll-y flex-1">
					<SidebarAutomations {workflowList} {scheduleList} activeWorkflowId={focusedWorkflowId} showHeading={false} onnavigate={() => (mobileMenuOpen = false)} />
				</div>
			{:else if sidebarCategory === 'settings'}
				<div data-mobile-sidebar-scroll class="workspace-scroll-y flex-1">
					<SidebarSettings activeKind={focusedTab?.kind ?? 'settings'} showHeading={false} onnavigate={() => (mobileMenuOpen = false)} />
				</div>
			{:else}
				{#if attentionTasks.length > 0}
					<div class="mb-4 shrink-0"><div class="mb-1 px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-orange-400">Needs you</div>{#each attentionTasks.slice(0, 5) as task (task.id)}<a href="/tasks/{task.id}" onclick={() => (mobileMenuOpen = false)} class="flex items-center gap-2 rounded-lg px-2 py-2 hover:bg-accent"><span class="h-2 w-2 rounded-full {task.status === 'blocked' ? 'bg-red-500' : 'bg-orange-500'}"></span><span class="min-w-0 flex-1 truncate text-sm">{task.title}</span></a>{/each}</div>
				{/if}
				<div data-mobile-sidebar-scroll class="workspace-scroll-y flex-1">
					<SidebarProjects {projectList} {conversationList} {agentList} taskList={sidebarTaskList} {activeProjectId} activeConversationId={focusedConversation?.id ?? null} activeAgentId={focusedAgent?.id ?? null} onagentcontext={showProjectContextMenu} onnavigate={() => (mobileMenuOpen = false)} />
				</div>
			{/if}
		</aside>
	</div>
{/if}

<nav class="fixed inset-x-0 bottom-0 z-40 grid h-16 grid-cols-4 border-t border-border bg-background/95 pb-[env(safe-area-inset-bottom)] backdrop-blur md:hidden">
	{#each utilityTabs as item}
		<a href={item.href} class="flex flex-col items-center justify-center gap-1 text-[10px] {tabCategory(focusedTab?.kind) === tabCategory(item.kind) ? 'text-primary' : 'text-muted-foreground'}"><svg class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d={item.icon} /></svg><span>{item.label}</span></a>
	{/each}
</nav>

{#if contextMenu}
	<ContextMenu
		x={contextMenu.x}
		y={contextMenu.y}
		label={contextMenu.kind === 'project' ? `${contextMenu.agent.title || contextMenu.agent.name} actions` : `${contextMenu.tab.title} tab actions`}
		items={contextMenuItems(contextMenu)}
		onselect={(action) => selectContextMenuItem(contextMenu!, action)}
		onclose={() => (contextMenu = null)}
	/>
{/if}

{#if !serverConnected}
	<div data-connection-status role="status" aria-live="polite" class="pointer-events-none fixed inset-x-0 top-14 z-[200] flex justify-center px-3">
		<div class="flex max-w-md items-center gap-3 rounded-xl border border-amber-500/30 bg-card/95 px-4 py-3 shadow-xl backdrop-blur">
			<svg class="h-5 w-5 shrink-0 animate-pulse text-amber-500" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M8.288 15.038a5.25 5.25 0 017.424 0M5.106 11.856c3.807-3.808 9.98-3.808 13.788 0M1.924 8.674c5.565-5.565 14.587-5.565 20.152 0M12 20.25h.008v.008H12v-.008z" /></svg>
			<div>
				<div class="text-sm font-semibold">Connection interrupted</div>
				<p class="text-xs text-muted-foreground">Retrying in the background. Your draft is saved and you can keep typing.</p>
			</div>
		</div>
	</div>
{/if}

{#if !dockerAvailable}
	<div class="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm"><div class="mx-4 w-full max-w-sm space-y-4 rounded-xl border border-border bg-card p-6 shadow-2xl"><div class="flex items-center gap-3"><div class="flex h-10 w-10 items-center justify-center rounded-full bg-amber-500/10"><svg class="h-5 w-5 text-amber-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" /></svg></div><div><h3 class="text-sm font-semibold">Container runtime is not running</h3><p class="text-xs text-muted-foreground">{dockerInstalled ? 'Start Docker or Podman to run queued work.' : 'Install Docker or Podman to run ACP workers.'}</p></div></div><div class="flex justify-end gap-2"><button type="button" onclick={() => (dockerAvailable = true)} class="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-secondary">Dismiss</button>{#if dockerCanStart}<button type="button" onclick={startDocker} disabled={dockerStarting} class="rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground disabled:opacity-50">{dockerStarting ? 'Starting…' : 'Start runtime'}</button>{/if}</div></div></div>
{/if}
