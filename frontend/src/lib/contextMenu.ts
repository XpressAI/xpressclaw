export interface ContextMenuItem {
	id: string;
	label: string;
	disabled?: boolean;
	separatorBefore?: boolean;
}

export const PROJECT_CONTEXT_MENU_ITEMS: ContextMenuItem[] = [
	{ id: 'open-new-window', label: 'Open in New Window' },
	{ id: 'open-tasks', label: 'Open Tasks', separatorBefore: true },
	{ id: 'open-schedules', label: 'Open Automations' },
	{ id: 'open-runner', label: 'Open Harness' },
	{ id: 'open-workspace', label: 'Open Environment' },
];
