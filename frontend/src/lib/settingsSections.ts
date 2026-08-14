import type { WorkspaceTabKind } from '$lib/workspace';

export const SETTINGS_SECTIONS: {
	kind: WorkspaceTabKind;
	label: string;
	shortLabel: string;
	href: string;
}[] = [
	{ kind: 'settings', label: 'Profile', shortLabel: 'P', href: '/settings' },
	{ kind: 'settings-sync', label: 'Project sync', shortLabel: '↕', href: '/settings/sync' },
	{ kind: 'settings-mcp', label: 'MCP servers', shortLabel: 'M', href: '/settings/mcp' },
	{ kind: 'settings-server', label: 'Instance', shortLabel: 'I', href: '/settings/server' },
];
