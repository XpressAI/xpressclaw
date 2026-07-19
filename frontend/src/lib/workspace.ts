export type WorkspaceTabKind =
	| 'home'
	| 'projects'
	| 'project'
	| 'tasks'
	| 'task'
	| 'schedules'
	| 'workflows'
	| 'workflow'
	| 'workflow-new'
	| 'settings'
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

let idCounter = 0;

export function workspaceId(prefix: 'tab' | 'pane'): string {
	idCounter += 1;
	return `${prefix}-${Date.now().toString(36)}-${idCounter.toString(36)}`;
}

export function workspacePath(pathname: string): boolean {
	return pathname === '/'
		|| pathname === '/agents'
		|| pathname.startsWith('/agents/')
		|| pathname === '/tasks'
		|| pathname.startsWith('/tasks/')
		|| pathname === '/schedules'
		|| pathname === '/workflows'
		|| pathname.startsWith('/workflows/')
		|| pathname === '/settings'
		|| pathname.startsWith('/settings/');
}

export function describeWorkspacePath(pathname: string): Omit<WorkspaceTab, 'id' | 'status'> {
	if (pathname === '/') return { path: pathname, kind: 'home', title: 'New work', resourceId: null };
	if (pathname === '/agents') return { path: pathname, kind: 'projects', title: 'Projects', resourceId: null };
	if (pathname.startsWith('/agents/')) return { path: pathname, kind: 'project', title: 'Project', resourceId: pathname.slice('/agents/'.length) };
	if (pathname === '/tasks') return { path: pathname, kind: 'tasks', title: 'Tasks', resourceId: null };
	if (pathname.startsWith('/tasks/')) return { path: pathname, kind: 'task', title: 'Task', resourceId: pathname.slice('/tasks/'.length) };
	if (pathname === '/schedules') return { path: pathname, kind: 'schedules', title: 'Schedules', resourceId: null };
	if (pathname === '/workflows') return { path: pathname, kind: 'workflows', title: 'Workflows', resourceId: null };
	if (pathname === '/workflows/new') return { path: pathname, kind: 'workflow-new', title: 'New workflow', resourceId: null };
	if (pathname.startsWith('/workflows/')) return { path: pathname, kind: 'workflow', title: 'Workflow', resourceId: pathname.slice('/workflows/'.length) };
	if (pathname === '/settings/server') return { path: pathname, kind: 'settings-server', title: 'Settings', resourceId: null };
	return { path: '/settings', kind: 'settings', title: 'Settings', resourceId: null };
}

export function sameWorkspaceTab(tab: WorkspaceTab, pathname: string): boolean {
	const next = describeWorkspacePath(pathname);
	const settingsKinds: WorkspaceTabKind[] = ['settings', 'settings-server'];
	if (settingsKinds.includes(tab.kind) && settingsKinds.includes(next.kind)) return true;
	return tab.path === pathname;
}

export function createWorkspaceTab(pathname: string): WorkspaceTab {
	return { id: workspaceId('tab'), status: null, ...describeWorkspacePath(pathname) };
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
