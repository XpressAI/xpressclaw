export type WorkspaceTabKind =
	| 'home'
	| 'projects'
	| 'project'
	| 'agents'
	| 'agent'
	| 'conversation'
	| 'tasks'
	| 'task'
	| 'automations'
	| 'schedules'
	| 'workflows'
	| 'workflow'
	| 'workflow-new'
	| 'settings'
	| 'settings-mcp'
	| 'settings-sync'
	| 'settings-server';

export interface WorkspaceTab {
	id: string;
	path: string;
	kind: WorkspaceTabKind;
	title: string;
	resourceId: string | null;
	status: string | null;
}

export interface WorkspacePaneState {
	id: string;
	tabs: WorkspaceTab[];
	activeTabId: string;
	width: number;
}

export const projectSections = ['session', 'tasks', 'schedules', 'files', 'runner', 'workspace'] as const;
export type ProjectSection = typeof projectSections[number];

let idCounter = 0;

export function workspaceId(prefix: 'tab' | 'pane'): string {
	idCounter += 1;
	return `${prefix}-${Date.now().toString(36)}-${idCounter.toString(36)}`;
}

function pathnameFromRoute(route: string): string {
	const queryIndex = route.indexOf('?');
	const hashIndex = route.indexOf('#');
	const end = [queryIndex, hashIndex]
		.filter((index) => index >= 0)
		.reduce((lowest, index) => Math.min(lowest, index), route.length);
	return route.slice(0, end) || '/';
}

export function projectPath(projectId: string, section: ProjectSection = 'session'): string {
	const base = `/agents/${projectId}`;
	return section === 'session' ? base : `${base}?tab=${section}`;
}

export function collaborationProjectPath(projectId: string): string {
	return `/projects/${encodeURIComponent(projectId)}`;
}

export function conversationPath(conversationId: string): string {
	return `/conversations/${encodeURIComponent(conversationId)}`;
}

export function projectSection(route: string): ProjectSection {
	const queryIndex = route.indexOf('?');
	if (queryIndex < 0) return 'session';
	const hashIndex = route.indexOf('#', queryIndex);
	const query = route.slice(queryIndex + 1, hashIndex < 0 ? undefined : hashIndex);
	const section = new URLSearchParams(query).get('tab');
	return projectSections.includes(section as ProjectSection) ? section as ProjectSection : 'session';
}

export function workspacePath(route: string): boolean {
	const pathname = pathnameFromRoute(route);
	return pathname === '/'
		|| pathname === '/projects'
		|| pathname.startsWith('/projects/')
		|| pathname === '/conversations'
		|| pathname.startsWith('/conversations/')
		|| pathname === '/agents'
		|| pathname.startsWith('/agents/')
		|| pathname === '/tasks'
		|| pathname.startsWith('/tasks/')
		|| pathname === '/automations'
		|| pathname === '/schedules'
		|| pathname === '/workflows'
		|| pathname.startsWith('/workflows/')
		|| pathname === '/settings'
		|| pathname.startsWith('/settings/');
}

export function describeWorkspacePath(route: string): Omit<WorkspaceTab, 'id' | 'status'> {
	const pathname = pathnameFromRoute(route);
	if (pathname === '/') return { path: route, kind: 'home', title: 'New work', resourceId: null };
	if (pathname === '/projects') return { path: route, kind: 'projects', title: 'Projects', resourceId: null };
	if (pathname.startsWith('/projects/')) return { path: route, kind: 'project', title: 'Project', resourceId: decodeURIComponent(pathname.slice('/projects/'.length)) };
	if (pathname === '/conversations') return { path: route, kind: 'projects', title: 'Conversations', resourceId: null };
	if (pathname.startsWith('/conversations/')) return { path: route, kind: 'conversation', title: 'Conversation', resourceId: decodeURIComponent(pathname.slice('/conversations/'.length)) };
	if (pathname === '/agents') return { path: route, kind: 'agents', title: 'Agents', resourceId: null };
	if (pathname.startsWith('/agents/')) return { path: route, kind: 'agent', title: 'Agent', resourceId: decodeURIComponent(pathname.slice('/agents/'.length)) };
	if (pathname === '/tasks') return { path: route, kind: 'tasks', title: 'Tasks', resourceId: null };
	if (pathname.startsWith('/tasks/')) return { path: route, kind: 'task', title: 'Task', resourceId: pathname.slice('/tasks/'.length) };
	if (pathname === '/automations' || pathname === '/schedules' || pathname === '/workflows') return { path: route, kind: 'automations', title: 'Automations', resourceId: null };
	if (pathname === '/workflows/new') return { path: route, kind: 'workflow-new', title: 'New workflow', resourceId: null };
	if (pathname.startsWith('/workflows/')) return { path: route, kind: 'workflow', title: 'Workflow', resourceId: pathname.slice('/workflows/'.length) };
	if (pathname === '/settings/server') return { path: route, kind: 'settings-server', title: 'Settings', resourceId: null };
	if (pathname === '/settings/mcp') return { path: route, kind: 'settings-mcp', title: 'Settings', resourceId: null };
	if (pathname === '/settings/sync') return { path: route, kind: 'settings-sync', title: 'Settings', resourceId: null };
	return { path: '/settings', kind: 'settings', title: 'Settings', resourceId: null };
}

export function sameWorkspaceTab(tab: WorkspaceTab, route: string): boolean {
	const next = describeWorkspacePath(route);
	const settingsKinds: WorkspaceTabKind[] = ['settings', 'settings-mcp', 'settings-sync', 'settings-server'];
	const automationKinds: WorkspaceTabKind[] = ['automations', 'schedules', 'workflows'];
	if (settingsKinds.includes(tab.kind) && settingsKinds.includes(next.kind)) return true;
	if (automationKinds.includes(tab.kind) && automationKinds.includes(next.kind)) return true;
	if (tab.kind === 'project' && next.kind === 'project') return tab.resourceId === next.resourceId;
	if (tab.kind === 'agent' && next.kind === 'agent') return tab.resourceId === next.resourceId;
	if (tab.kind === 'conversation' && next.kind === 'conversation') return tab.resourceId === next.resourceId;
	return tab.path === route;
}

export function createWorkspaceTab(route: string): WorkspaceTab {
	return { id: workspaceId('tab'), status: null, ...describeWorkspacePath(route) };
}

export function validWorkspacePane(value: unknown): value is WorkspacePaneState {
	if (!value || typeof value !== 'object') return false;
	const pane = value as Partial<WorkspacePaneState>;
	return typeof pane.id === 'string'
		&& typeof pane.activeTabId === 'string'
		&& typeof pane.width === 'number'
		&& Array.isArray(pane.tabs)
		&& pane.tabs.length > 0
		&& pane.tabs.every((tab) => Boolean(tab)
			&& typeof tab.id === 'string'
			&& typeof tab.path === 'string'
			&& workspacePath(tab.path));
}

export function statusPriority(status: string | null | undefined): number {
	if (status === 'failed' || status === 'error' || status === 'blocked') return 5;
	if (status === 'waiting_for_input') return 4;
	if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 3;
	if (status === 'queued' || status === 'pending') return 2;
	return 1;
}
