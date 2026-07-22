import { isTauri } from '@tauri-apps/api/core';

export const WORKSPACE_WINDOW_PARAM = '_xpressclaw_window';

let windowCounter = 0;

function nextWindowLabel(): string {
	windowCounter += 1;
	return `workspace-${Date.now()}-${windowCounter}`;
}

function errorMessage(payload: unknown): string {
	if (payload instanceof Error) return payload.message;
	if (typeof payload === 'string') return payload;
	try {
		return JSON.stringify(payload);
	} catch {
		return String(payload);
	}
}

export async function openWorkspaceWindow(path: string, title = 'xpressclaw'): Promise<void> {
	const label = nextWindowLabel();
	const url = new URL(path, window.location.href);
	url.searchParams.set(WORKSPACE_WINDOW_PARAM, label);
	if (!isTauri()) {
		window.open(url.href, '_blank', 'popup,width=1200,height=800');
		return;
	}

	const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
	const workspaceWindow = new WebviewWindow(label, {
		url: url.href,
		title: `${title} — xpressclaw`,
		width: 1200,
		height: 800,
		minWidth: 800,
		minHeight: 600,
		focus: true,
	});

	await new Promise<void>((resolve, reject) => {
		void workspaceWindow.once('tauri://created', () => resolve());
		void workspaceWindow.once('tauri://error', (event) => {
			reject(new Error(`Could not open the window: ${errorMessage(event.payload)}`));
		});
	});
}
