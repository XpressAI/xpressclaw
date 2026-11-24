import { expect, test, type Page } from '@playwright/test';
import {
	bezierEdgePath,
	classifyAppUrl,
	classifyCanvasLink,
	computeFitCamera,
	computeProjectZones,
	createCanvasNode,
	deriveCanvasEdges,
	extractActiveWorkspaceRoots,
	isAllowedHtmlPreviewEndpoint,
	isHtmlFilePath,
	isImageFilePath,
	normalizePersistedCanvasState,
	resolveWorkspaceRelativePath,
	seedCanvasNodes,
	tidyCanvasLayout,
	tidyFilteredCanvasNodes,
	type CanvasNode,
} from '../src/lib/canvas';

const now = new Date('2026-11-20T12:00:00.000Z').toISOString();

const png1x1 = Buffer.from(
	'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+X2dIAAAAASUVORK5CYII=',
	'base64',
);

const mockProjects = [
	{
		id: 'proj-alpha',
		name: 'Alpha Core',
		description: 'Main runtime project',
		icon: 'A',
		created_at: now,
		updated_at: now,
		agent_ids: ['agent-codex', 'agent-claude'],
		conversation_count: 1,
		task_count: 3,
	},
	{
		id: 'proj-beta',
		name: 'Beta UI',
		description: 'Design system workspace',
		icon: 'B',
		created_at: now,
		updated_at: now,
		agent_ids: ['agent-ui'],
		conversation_count: 0,
		task_count: 1,
	},
];

const mockAgents = [
	{
		id: 'agent-codex',
		name: 'agent-codex',
		title: 'Codex Builder',
		backend: 'codex',
		project_id: 'proj-alpha',
		status: 'running',
		desired_status: 'running',
		observed_status: 'running',
		container_id: 'container-codex',
		created_at: now,
		started_at: now,
		stopped_at: null,
		error_message: null,
		restart_count: 0,
	},
	{
		id: 'agent-claude',
		name: 'agent-claude',
		title: 'Claude Reviewer',
		backend: 'claude',
		project_id: 'proj-alpha',
		status: 'idle',
		desired_status: 'running',
		observed_status: 'running',
		container_id: 'container-claude',
		created_at: now,
		started_at: now,
		stopped_at: null,
		error_message: null,
		restart_count: 0,
	},
	{
		id: 'agent-ui',
		name: 'agent-ui',
		title: 'UI Designer',
		backend: 'codex',
		project_id: 'proj-beta',
		status: 'idle',
		desired_status: 'running',
		observed_status: 'running',
		container_id: 'container-ui',
		created_at: now,
		started_at: now,
		stopped_at: null,
		error_message: null,
		restart_count: 0,
	},
];

const mockConversations = [
	{
		id: 'conv-alpha',
		project_id: 'proj-alpha',
		title: 'Release Sync',
		icon: '#',
		created_at: now,
		updated_at: now,
		last_message_at: now,
		participants: [
			{ participant_type: 'agent', participant_id: 'agent-codex', joined_at: now },
		],
	},
];

const mockTasks = [
	{
		id: 'task-parent',
		title: 'Implement Spatial Canvas',
		description: 'Coordinate parallel frontend and backend tasks',
		status: 'in_progress',
		activity_status: 'waiting_for_subtasks',
		priority: 1,
		agent_id: 'agent-codex',
		parent_task_id: null,
		sop_id: null,
		conversation_id: null,
		project_id: 'proj-alpha',
		created_at: now,
		updated_at: now,
		completed_at: null,
		context: {},
		blocks_parent: false,
	},
	{
		id: 'task-child-1',
		title: 'Build Bezier Edge Layer',
		description: 'Render live delegation wires',
		status: 'in_progress',
		activity_status: 'running',
		priority: 1,
		agent_id: 'agent-codex',
		parent_task_id: 'task-parent',
		sop_id: null,
		conversation_id: null,
		project_id: 'proj-alpha',
		created_at: now,
		updated_at: now,
		completed_at: null,
		context: {},
		blocks_parent: true,
	},
	{
		id: 'task-child-2',
		title: 'Review Workspace Security',
		description: 'Verify raw asset endpoint headers',
		status: 'pending',
		activity_status: 'queued',
		priority: 1,
		agent_id: 'agent-claude',
		parent_task_id: 'task-parent',
		sop_id: null,
		conversation_id: null,
		project_id: 'proj-alpha',
		created_at: now,
		updated_at: now,
		completed_at: null,
		context: {},
		blocks_parent: true,
		depends_on: ['task-child-1'],
	},
];

async function mockCanvasApi(page: Page) {
	const savedFiles: Array<{ path: string; content: string }> = [];
	const exposedPorts = [
		{ id: 'port-1', direction: 'container_to_host', host_port: 3100, container_port: 3000, active: true },
	];
	let fileContent = 'export const greeting = "hello canvas";\n';

	await page.route('**/api/**', async (route) => {
		const url = new URL(route.request().url());
		const path = url.pathname;
		const method = route.request().method();

		if (path === '/api/health') {
			return route.fulfill({ json: { status: 'ok' } });
		}
		if (path === '/api/auth/bootstrap') {
			return route.fulfill({
				json: {
					instance_id: 'test-instance',
					identity_public_key: 'pk-test',
					authentication_enabled: false,
					credential_kind: 'disabled',
					authenticated: true,
					csrf_token: 'csrf-test',
				},
			});
		}
		if (path === '/api/setup/status') {
			return route.fulfill({ json: { setup_complete: true } });
		}
		if (path === '/api/setup/check-docker') {
			return route.fulfill({ json: { available: true, installed: true, can_start: false } });
		}
		if (path === '/api/ui/events') {
			return route.fulfill({
				status: 200,
				contentType: 'text/event-stream',
				body: 'event: ready\ndata: {"projects":1,"agents":1,"conversations":1,"tasks":1,"sessions":1,"schedules":1,"workflows":1}\n\n',
			});
		}
		if (path === '/api/projects') {
			return route.fulfill({ json: mockProjects });
		}
		if (path.startsWith('/api/projects/')) {
			const id = decodeURIComponent(path.split('/')[3]);
			if (path.endsWith('/tasks')) {
				return route.fulfill({ json: mockTasks.filter((t) => t.project_id === id) });
			}
			const found = mockProjects.find((p) => p.id === id);
			return route.fulfill({ status: found ? 200 : 404, json: found ?? { error: 'not found' } });
		}
		if (path === '/api/agents') {
			return route.fulfill({ json: mockAgents });
		}
		if (path.startsWith('/api/agents/')) {
			const id = decodeURIComponent(path.split('/')[3]);
			const found = mockAgents.find((a) => a.id === id);
			return route.fulfill({ status: found ? 200 : 404, json: found ?? { error: 'not found' } });
		}
		if (path === '/api/conversations') {
			const projectId = url.searchParams.get('project_id');
			return route.fulfill({
				json: projectId ? mockConversations.filter((c) => c.project_id === projectId) : mockConversations,
			});
		}
		if (path.startsWith('/api/conversations/')) {
			const id = decodeURIComponent(path.split('/')[3]);
			if (path.endsWith('/messages')) return route.fulfill({ json: [] });
			if (path.endsWith('/turns')) return route.fulfill({ json: [] });
			if (path.endsWith('/tasks')) return route.fulfill({ json: [] });
			if (path.endsWith('/events')) {
				return route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' });
			}
			const found = mockConversations.find((c) => c.id === id);
			return route.fulfill({ status: found ? 200 : 404, json: found ?? { error: 'not found' } });
		}
		if (path === '/api/tasks') {
			const parentId = url.searchParams.get('parent_task_id');
			const list = parentId ? mockTasks.filter((t) => t.parent_task_id === parentId) : mockTasks;
			return route.fulfill({
				json: {
					tasks: list,
					counts: { pending: 1, in_progress: 2, waiting_for_input: 0, blocked: 0, completed: 0, cancelled: 0 },
				},
			});
		}
		if (path === '/api/tasks/recent-by-agent') {
			return route.fulfill({ json: { tasks: mockTasks } });
		}
		if (path === '/api/tasks/attention') {
			return route.fulfill({ json: { tasks: [] } });
		}
		if (path.startsWith('/api/tasks/')) {
			const id = decodeURIComponent(path.split('/')[3]);
			if (path.endsWith('/messages')) return route.fulfill({ json: [] });
			if (path.endsWith('/activity')) {
				return route.fulfill({
					json: {
						attempts: [],
						events: [],
						has_more_before: false,
						has_more_after: false,
						delegated_work: id === 'task-parent'
							? mockTasks
								.filter((t) => t.parent_task_id === 'task-parent')
								.map((t) => ({
									task_id: t.id,
									title: t.title,
									agent_id: t.agent_id,
									status: t.status,
									activity_status: t.activity_status,
									blocks_parent: t.blocks_parent,
									created_at: t.created_at,
									updated_at: t.updated_at,
									started_at: t.created_at,
									finished_at: null,
									latest_event_type: 'tool_call',
									latest_event_at: t.updated_at,
								}))
							: [],
					},
				});
			}
			if (path.endsWith('/config')) {
				return route.fulfill({
					json: {
						context: null,
						availability: 'unavailable',
						config_options: [],
						modes: null,
						event_id: null,
						revision: null,
						pending: {},
						model_pending: false,
						has_preferences: false,
						notice: '',
					},
				});
			}
			if (path.endsWith('/planning')) {
				const found = mockTasks.find((t) => t.id === id);
				return route.fulfill({ status: found ? 200 : 404, json: found ?? { error: 'not found' } });
			}
			const found = mockTasks.find((t) => t.id === id);
			return route.fulfill({ status: found ? 200 : 404, json: found ?? { error: 'not found' } });
		}
		if (path === '/api/workflows' || path === '/api/schedules') {
			return route.fulfill({ json: [] });
		}
		if (path.match(/^\/api\/workspaces\/[^/]+\/tree$/)) {
			return route.fulfill({
				json: {
					path: '',
					entries: [
						{ name: 'main.ts', path: 'main.ts', kind: 'file', symlink: false, size: fileContent.length, modified_at: now },
						{ name: 'preview.png', path: 'preview.png', kind: 'file', symlink: false, size: png1x1.length, modified_at: now },
						{ name: 'demo.html', path: 'demo.html', kind: 'file', symlink: false, size: 96, modified_at: now },
					],
					truncated: false,
				},
			});
		}
		if (path.match(/^\/api\/workspaces\/[^/]+\/file$/)) {
			const filePath = url.searchParams.get('path') ?? '';
			if (method === 'PUT') {
				const body = JSON.parse(route.request().postData() ?? '{}');
				fileContent = body.content;
				savedFiles.push({ path: body.path, content: body.content });
				return route.fulfill({ json: { path: body.path, revision: 'rev-2', size: body.content.length } });
			}
			if (filePath === 'demo.html') {
				return route.fulfill({
					json: {
						path: 'demo.html',
						content: '<!doctype html><html><body><h1 id="demo-app-heading">Canvas Demo App</h1></body></html>',
						revision: 'rev-html-1',
						size: 92,
					},
				});
			}
			return route.fulfill({
				json: {
					path: filePath || 'main.ts',
					content: fileContent,
					revision: 'rev-1',
					size: fileContent.length,
				},
			});
		}
		if (path.match(/^\/api\/workspaces\/[^/]+\/raw$/)) {
			return route.fulfill({
				status: 200,
				contentType: 'image/png',
				body: png1x1,
			});
		}
		if (path.match(/^\/api\/workspaces\/[^/]+\/git\/status$/)) {
			return route.fulfill({
				json: { repository: true, branch: 'main', files: [], repository_status: { state: 'attached' } },
			});
		}
		if (path.match(/^\/api\/workspaces\/[^/]+\/git\/diff$/)) {
			return route.fulfill({ json: { path: url.searchParams.get('path') ?? '', diff: '', truncated: false } });
		}
		if (path.match(/^\/api\/environments\/[^/]+\/ports$/)) {
			if (method === 'POST') {
				const body = JSON.parse(route.request().postData() ?? '{}');
				const created = { id: `port-${exposedPorts.length + 1}`, ...body, active: true };
				exposedPorts.push(created);
				return route.fulfill({ json: created });
			}
			return route.fulfill({ json: { ports: exposedPorts } });
		}
		if (path.match(/^\/api\/environments\/[^/]+\/terminal-sessions$/)) {
			return route.fulfill({ json: { sessions: ['xpressclaw'] } });
		}
		if (path.match(/^\/api\/sessions\/[^/]+\/events$/)) {
			return route.fulfill({ json: [] });
		}
		return route.fulfill({ json: {} });
	});

	return { savedFiles };
}

test.describe('Spatial Canvas workspace', () => {
	test('defaults to Tabs mode, switches cleanly to Canvas with spatial delegation edges, and persists layout', async ({ page }) => {
		await mockCanvasApi(page);
		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/');

		// Tabs mode remains the default.
		const toggle = page.locator('[data-workspace-mode-toggle]');
		await expect(toggle).toBeVisible();
		await expect(toggle.getByRole('button', { name: 'Tabs' })).toHaveAttribute('aria-pressed', 'true');
		await expect(page.locator('[data-workspace-canvas]')).toHaveCount(0);

		// Switch to optional Canvas view.
		await toggle.getByRole('button', { name: 'Canvas' }).click();
		const canvas = page.locator('[data-workspace-canvas]');
		await expect(canvas).toBeVisible();

		// Projects and active/delegated tasks are seeded spatially with delegation & dependency wires.
		await expect(page.locator('[data-canvas-project-card="proj-alpha"]')).toBeVisible();
		await expect(page.locator('[data-canvas-project-card="proj-beta"]')).toBeVisible();
		await expect(page.locator('[data-canvas-node-kind="task"]')).toHaveCount(3);
		await expect(page.locator('[data-canvas-edge-kind="delegation"]')).toHaveCount(2);
		await expect(page.locator('[data-canvas-edge-kind="dependency"]')).toHaveCount(1);
		await expect(page.locator('[data-canvas-project-zone="proj-alpha"]')).toBeVisible();

		// Dragging a canvas window updates its spatial coordinates and persists across reload.
		const projectWindow = page.locator('[data-canvas-node][data-canvas-node-kind="project"]').first();
		const beforeStyle = await projectWindow.getAttribute('style');
		const dragHandle = projectWindow.locator('[data-canvas-drag-handle]');
		const box = await dragHandle.boundingBox();
		expect(box).not.toBeNull();
		await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
		await page.mouse.down();
		await page.mouse.move(box!.x + box!.width / 2 + 120, box!.y + box!.height / 2 + 80, { steps: 6 });
		await page.mouse.up();

		const afterStyle = await projectWindow.getAttribute('style');
		expect(afterStyle).not.toEqual(beforeStyle);

		// Reloading restores Canvas mode and the moved window coordinates.
		await page.reload();
		await expect(page.locator('[data-workspace-canvas]')).toBeVisible();
		const restoredWindow = page.locator('[data-canvas-node][data-canvas-node-kind="project"]').first();
		await expect(restoredWindow).toHaveAttribute('style', afterStyle!);

		// Switching back to Tabs hides the canvas (while keeping it mounted to preserve buffers) without breaking tabbed panes.
		await page.locator('[data-workspace-mode-toggle]').getByRole('button', { name: 'Tabs' }).click();
		await expect(page.locator('[data-workspace-canvas]')).not.toBeVisible();
		await expect(page.locator('[data-workspace-pane]').first()).toBeVisible();
	});

	test('spawns connected Code Editor, Terminal, Image Viewer, and Web App windows in project context', async ({ page }) => {
		await mockCanvasApi(page);
		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/canvas');

		const canvas = page.locator('[data-workspace-canvas]');
		await expect(canvas).toBeVisible();
		const alphaCard = page.locator('[data-canvas-project-card="proj-alpha"]');
		await expect(alphaCard).toBeVisible();

		// 1. Spawn a connected Code Editor window from the Project card and open a file.
		await alphaCard.getByRole('button', { name: 'Editor', exact: true }).click();
		const editorCard = page.locator('[data-canvas-editor-card]').first();
		await expect(editorCard).toBeVisible();
		await editorCard.getByRole('button', { name: 'main.ts' }).click();
		await expect(editorCard.getByLabel('Workspace file path')).toHaveValue('main.ts');

		// 2. Clicking an image in the editor tree spawns a connected Image Viewer window.
		await editorCard.getByRole('button', { name: 'preview.png' }).click();
		const imageCard = page.locator('[data-canvas-image-card]').first();
		await expect(imageCard).toBeVisible();
		await expect(imageCard.locator('img[src*="/api/workspaces/agent-codex/raw"]')).toBeVisible();

		// Use the top HUD Fit control to bring the project hub and spawned tools back into view.
		await page.locator('[data-canvas-fit]').click();
		await expect(alphaCard).toBeVisible();

		// 3. Spawn a connected Terminal window from the Project card.
		await alphaCard.getByRole('button', { name: 'Terminal', exact: true }).click();
		const terminalCard = page.locator('[data-canvas-terminal-card]').first();
		await expect(terminalCard).toBeVisible();
		await expect(terminalCard.locator('[data-project-terminal]')).toBeVisible();

		// Jump back to the Alpha Core project cluster via the project pill and spawn a Web App window.
		await page.locator('[data-canvas-hud]').getByRole('button', { name: /Alpha Core/ }).click();
		await alphaCard.getByRole('button', { name: 'App', exact: true }).click();
		const appCard = page.locator('[data-canvas-app-card]').first();
		await expect(appCard).toBeVisible();
		await expect(appCard.getByText(':3100 ← :3000')).toBeVisible();
		await appCard.getByRole('button', { name: 'demo.html' }).click();
		const appFrame = appCard.locator('iframe[data-canvas-app-frame]');
		await expect(appFrame).toBeVisible();
		await expect(appFrame).toHaveAttribute('sandbox', 'allow-scripts allow-forms');

		// Tidy layout arranges everything cleanly without crashing, and context edges remain wired.
		await page.locator('[data-canvas-tidy]').click();
		const contextEdges = await page.locator('[data-canvas-edge-kind="context"]').count();
		expect(contextEdges).toBeGreaterThanOrEqual(4);

		// Space+drag over a canvas window pans the canvas instead of being swallowed by the window.
		const worldLayer = page.locator('[data-canvas-pan-surface]');
		const beforeTransform = await worldLayer.getAttribute('style');
		const appBox = await appCard.boundingBox();
		expect(appBox).not.toBeNull();
		await page.keyboard.down('Space');
		await page.mouse.move(appBox!.x + appBox!.width / 2, appBox!.y + appBox!.height / 2);
		await page.mouse.down();
		await page.mouse.move(appBox!.x + appBox!.width / 2 + 80, appBox!.y + appBox!.height / 2 + 50, { steps: 4 });
		await page.mouse.up();
		await page.keyboard.up('Space');
		const afterTransform = await worldLayer.getAttribute('style');
		expect(afterTransform).not.toEqual(beforeTransform);
	});

	test('preserves unsaved editor buffers across Tabs switches, supports duplicate file paths across editors, and discards stale agent loads', async ({ page }) => {
		let releaseSlowAgentRead = () => {};
		const slowAgentReadGate = new Promise<void>((resolve) => {
			releaseSlowAgentRead = resolve;
		});
		let codexReadCount = 0;

		await mockCanvasApi(page);
		// Override workspace file reads so agent-codex's second read can be delayed while agent-claude returns distinct content.
		await page.route('**/api/workspaces/*/file**', async (route) => {
			const url = new URL(route.request().url());
			const agentId = decodeURIComponent(url.pathname.split('/')[3]);
			const filePath = url.searchParams.get('path') ?? 'main.ts';
			if (agentId === 'agent-codex') {
				codexReadCount += 1;
				if (codexReadCount === 2) {
					await slowAgentReadGate;
				}
				return route.fulfill({
					json: { path: filePath, content: '// codex version\n', revision: 'rev-codex', size: 17 },
				});
			}
			return route.fulfill({
				json: { path: filePath, content: '// claude version\n', revision: 'rev-claude', size: 18 },
			});
		});

		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/canvas');
		const alphaCard = page.locator('[data-canvas-project-card="proj-alpha"]');
		await expect(alphaCard).toBeVisible();

		// Open two editors for different agents on the exact same relative path `main.ts`.
		await alphaCard.getByRole('button', { name: 'Open Codex Builder editor' }).click();
		const firstEditor = page.locator('[data-canvas-editor-card]').nth(0);
		await expect(firstEditor).toBeVisible();
		await firstEditor.getByRole('button', { name: 'main.ts' }).click();
		await expect(firstEditor.locator('[data-monaco-editor] .view-lines')).toContainText('codex version');

		// Edit buffer in first editor so it becomes dirty, then verify Refresh workspace and Tabs switches preserve it.
		await firstEditor.locator('[data-monaco-editor]').click();
		await page.keyboard.type('unsaved-edit ');
		await expect(firstEditor.getByRole('button', { name: /Save •/ })).toBeVisible();
		await firstEditor.getByRole('button', { name: 'Refresh workspace' }).click();
		await expect(firstEditor.getByRole('button', { name: /Save •/ })).toBeVisible();
		await expect(firstEditor.locator('[data-monaco-editor] .view-lines')).toContainText('unsaved-edit');

		await page.locator('[data-workspace-mode-toggle]').getByRole('button', { name: 'Tabs' }).click();
		await expect(page.locator('[data-workspace-canvas]')).not.toBeVisible();
		await page.locator('[data-workspace-mode-toggle]').getByRole('button', { name: 'Canvas' }).click();
		await expect(firstEditor.getByRole('button', { name: /Save •/ })).toBeVisible();
		await expect(firstEditor.locator('[data-monaco-editor] .view-lines')).toContainText('unsaved-edit');

		// Open a second editor for Claude Reviewer on the exact same relative path `main.ts` — Monaco must mount cleanly with no URI collision.
		await page.locator('[data-canvas-fit]').click();
		await alphaCard.getByRole('button', { name: 'Open Claude Reviewer editor' }).click();
		const secondEditor = page.locator('[data-canvas-editor-card]').nth(1);
		await expect(secondEditor).toBeVisible();
		await secondEditor.getByRole('button', { name: 'main.ts' }).click();
		await expect(secondEditor.locator('[data-monaco-editor] .view-lines')).toContainText('claude version');

		// Trigger a slow file read for agent-codex in a third editor, immediately switch to agent-claude, then release the slow response.
		await page.locator('[data-canvas-fit]').click();
		await alphaCard.getByRole('button', { name: 'Open Codex Builder editor' }).click();
		const thirdEditor = page.locator('[data-canvas-editor-card]').nth(2);
		await expect(thirdEditor).toBeVisible();
		await thirdEditor.getByRole('button', { name: 'main.ts' }).click();
		await thirdEditor.getByLabel('Editor agent').selectOption('agent-claude');
		releaseSlowAgentRead();
		// Stale codex response must NOT populate the editor now scoped to agent-claude.
		await expect(thirdEditor.getByLabel('Workspace file path')).toHaveValue('');
		await expect(thirdEditor.locator('[data-monaco-editor]')).toHaveCount(0);

		// Confirm before discarding unsaved editor buffers on Clear canvas and Close window.
		page.once('dialog', (dialog) => dialog.dismiss());
		await page.getByRole('button', { name: 'Clear canvas' }).click();
		await expect(firstEditor).toBeVisible();
		await expect(firstEditor.locator('[data-monaco-editor] .view-lines')).toContainText('unsaved-edit');

		const firstEditorWindow = page.locator('[data-canvas-node-kind="editor"]').nth(0);
		page.once('dialog', (dialog) => dialog.dismiss());
		await firstEditorWindow.getByRole('button', { name: /^Close / }).click();
		await expect(firstEditor).toBeVisible();
		await expect(firstEditor.locator('[data-monaco-editor] .view-lines')).toContainText('unsaved-edit');

		page.once('dialog', (dialog) => dialog.accept());
		await firstEditorWindow.getByRole('button', { name: /^Close / }).click();
		await expect(page.locator('[data-canvas-editor-card]')).toHaveCount(2);

		// Opening an Agent window and switching its section tab updates the Agent window route instead of staying on Session.
		await page.locator('[data-canvas-fit]').click();
		await alphaCard.locator('button[title="Open Agent window on canvas"]', { hasText: 'Codex Builder' }).click();
		const agentWindow = page.locator('[data-canvas-node-kind="agent"]').first();
		await expect(agentWindow).toBeVisible();
		const filesTab = agentWindow.getByRole('tab', { name: 'Files' });
		const workTab = agentWindow.getByRole('tab', { name: 'Work' });
		await expect(filesTab).toBeVisible();
		await filesTab.click();
		await expect(filesTab).toHaveAttribute('aria-selected', 'true');
		// Navigating back to the bare agent route via link (e.g. sidebar link) resets to Work,
		// and clicking Files again still works without getting stuck on a stale URL/lastSyncedPath.
		await page.evaluate(() => {
			const link = document.createElement('a');
			link.href = '/agents/agent-codex';
			link.style.position = 'fixed';
			link.style.top = '0';
			link.style.left = '0';
			link.style.zIndex = '99999';
			link.textContent = 'Test agent link';
			document.body.appendChild(link);
			link.click();
			link.remove();
		});
		await expect(workTab).toHaveAttribute('aria-selected', 'true');
		await filesTab.click();
		await expect(filesTab).toHaveAttribute('aria-selected', 'true');
		const environmentTab = agentWindow.getByRole('tab', { name: 'Environment' });
		await environmentTab.click();
		await expect(environmentTab).toHaveAttribute('aria-selected', 'true');
	});

	test('previews file links, HTML/image attachments, and web URLs in Canvas directly from inside Task windows', async ({ page }) => {
		await mockCanvasApi(page);
		await page.route('**/api/workspaces/agent-codex', (route) =>
			route.fulfill({
				json: {
					agent_id: 'agent-codex',
					root: '/workspace',
					container_root: '/workspace',
					repository: {
						state: 'attached',
						message: '',
						bootstrap_root: '/workspace',
						active: { relative_path: '', root: '/workspace', github_repository: null },
						candidates: [],
						discovery_truncated: false,
					},
					container_exists: true,
					container_running: true,
					terminal_available: true,
				},
			}),
		);
		await page.route('**/api/tasks/task-parent/messages', (route) =>
			route.fulfill({
				json: [
					{
						id: 101,
						task_id: 'task-parent',
						agent_id: 'agent-codex',
						role: 'assistant',
						content: [
							'Here are the outputs:',
							'- [demo.html](file:///workspace/demo.html)',
							'- [preview.png](file:///workspace/preview.png)',
							'- [main.ts](file:///workspace/main.ts)',
							'- [Local App](http://127.0.0.1:4173/demo)',
						].join('\n'),
						timestamp: now,
						attachments: [
							{ id: 'att-html', name: 'report.html', mime_type: 'text/html', size: 64 },
							{ id: 'att-img', name: 'shot.png', mime_type: 'image/png', size: png1x1.length },
						],
					},
				],
			}),
		);
		await page.route('**/api/tasks/task-parent/messages/101/attachments/att-html', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'text/html; charset=utf-8',
				body: '<!doctype html><html><body><h1 id="attached-report">Attached HTML Report</h1></body></html>',
			}),
		);
		await page.route('**/api/tasks/task-parent/messages/101/attachments/att-img', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'image/png',
				body: png1x1,
			}),
		);

		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/canvas');
		const parentTaskWindow = page.locator('[data-canvas-node-kind="task"]', { hasText: 'Implement Spatial Canvas' }).first();
		await expect(parentTaskWindow).toBeVisible();
		await expect(parentTaskWindow.getByRole('link', { name: 'demo.html' })).toBeVisible();

		// 1. Workspace HTML file:// link -> Preview in Canvas App (resolved as workspace-html with Edit button).
		await parentTaskWindow.getByRole('link', { name: 'demo.html' }).click();
		const popover = page.locator('[data-canvas-link-popover]');
		await expect(popover).toBeVisible();
		await expect(popover).toContainText('demo.html');
		await popover.getByRole('menuitem', { name: 'Preview in Canvas App' }).click();
		await expect(popover).toHaveCount(0);
		const demoAppCard = page.locator('[data-canvas-app-card]').first();
		await expect(demoAppCard).toBeVisible();
		await expect(demoAppCard.getByRole('button', { name: 'Edit' })).toBeVisible();
		await expect(demoAppCard.frameLocator('[data-canvas-app-frame]').locator('#demo-app-heading')).toHaveText('Canvas Demo App');

		// 2. Workspace image file:// link -> Preview in Canvas Image Viewer.
		await page.locator('[data-canvas-fit]').click();
		await parentTaskWindow.getByRole('link', { name: 'preview.png' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Preview in Canvas Image Viewer' }).click();
		const workspaceImageCard = page.locator('[data-canvas-image-card]').first();
		await expect(workspaceImageCard).toBeVisible();
		await expect(workspaceImageCard.locator('img')).toHaveAttribute('src', /\/api\/workspaces\/agent-codex\/raw\?path=preview\.png/);

		// 3. Workspace code file:// link -> Open in Canvas Editor.
		await page.locator('[data-canvas-fit]').click();
		await parentTaskWindow.getByRole('link', { name: 'main.ts' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Open in Canvas Editor' }).click();
		const editorCard = page.locator('[data-canvas-editor-card]').first();
		await expect(editorCard).toBeVisible();
		await expect(editorCard.locator('[data-monaco-editor] .view-lines')).toContainText('hello canvas');

		// 4. HTML attachment -> Preview in Canvas App via sandboxed Blob iframe.
		await page.locator('[data-canvas-fit]').click();
		await parentTaskWindow.getByRole('link', { name: /Download report\.html/ }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Preview in Canvas App' }).click();
		const attachedAppCard = page.locator('[data-canvas-app-card]').nth(1);
		await expect(attachedAppCard).toBeVisible();
		await expect(attachedAppCard.frameLocator('[data-canvas-app-frame]').locator('#attached-report')).toHaveText('Attached HTML Report');

		// 5. Image attachment -> Preview in Canvas Image Viewer.
		await page.locator('[data-canvas-fit]').click();
		await parentTaskWindow.getByRole('link', { name: 'Open shot.png' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Preview in Canvas Image Viewer' }).click();
		const attachedImageCard = page.locator('[data-canvas-image-card]').nth(1);
		await expect(attachedImageCard).toBeVisible();
		await expect(attachedImageCard.locator('img')).toHaveAttribute('src', '/api/tasks/task-parent/messages/101/attachments/att-img');

		// 6. External web URL -> Preview in Canvas App & Escape dismissal.
		await page.locator('[data-canvas-fit]').click();
		await parentTaskWindow.getByRole('link', { name: 'Local App' }).click();
		await expect(popover).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(popover).toHaveCount(0);

		await parentTaskWindow.getByRole('link', { name: 'Local App' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Preview in Canvas App' }).click();
		const webAppCard = page.locator('[data-canvas-app-card]').nth(2);
		await expect(webAppCard).toBeVisible();
		await expect(webAppCard.locator('[data-canvas-app-frame]')).toHaveAttribute('src', 'http://127.0.0.1:4173/demo');

		// All preview windows spawned from the Task window are tethered via context edges.
		const contextEdges = await page.locator('[data-canvas-edge-kind="context"]').count();
		expect(contextEdges).toBeGreaterThanOrEqual(6);
	});

	test('previews Conversation attachments and excludes bootstrap_root when a nested repository is active', async ({ page }) => {
		let activeRepoName = 'repo';
		await mockCanvasApi(page);
		await page.route('**/api/workspaces/agent-codex', (route) =>
			route.fulfill({
				json: {
					agent_id: 'agent-codex',
					root: `/srv/agent/${activeRepoName}`,
					container_root: `/workspace/${activeRepoName}`,
					repository: {
						state: 'attached',
						message: '',
						bootstrap_root: '/srv/agent',
						active: { relative_path: activeRepoName, root: `/srv/agent/${activeRepoName}`, github_repository: null },
						candidates: [],
						discovery_truncated: false,
					},
					container_exists: true,
					container_running: true,
					terminal_available: true,
				},
			}),
		);
		await page.route('**/api/conversations/conv-alpha/messages**', (route) =>
			route.fulfill({
				json: [
					{
						id: 201,
						conversation_id: 'conv-alpha',
						sender_type: 'agent',
						sender_id: 'agent-codex',
						sender_name: 'Codex Builder',
						content: [
							'Files:',
							'- [outside.txt](file:///workspace/outside.txt)',
							'- [main.ts](file:///workspace/repo/main.ts)',
							'- [other.ts](file:///workspace/repo-b/other.ts)',
						].join('\n'),
						created_at: now,
						attachments: [
							{ id: 'conv-att-html', name: 'summary.html', mime_type: 'text/html', size: 72 },
							{ id: 'conv-att-img', name: 'diagram.png', mime_type: 'image/png', size: png1x1.length },
						],
					},
				],
			}),
		);
		await page.route('**/api/conversations/conv-alpha/attachments/conv-att-html', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'text/html; charset=utf-8',
				body: '<!doctype html><html><body><h1 id="conv-report-heading">Conversation HTML Report</h1></body></html>',
			}),
		);
		await page.route('**/api/conversations/conv-alpha/attachments/conv-att-img', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'image/png',
				body: png1x1,
			}),
		);

		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/canvas');
		const alphaCard = page.locator('[data-canvas-project-card="proj-alpha"]');
		await expect(alphaCard).toBeVisible();
		await alphaCard.getByRole('button', { name: /Release Sync/ }).click();

		const convWindow = page.locator('[data-canvas-node-kind="conversation"]').first();
		await expect(convWindow).toBeVisible();
		const popover = page.locator('[data-canvas-link-popover]');

		// 1. Conversation HTML attachment (/api/conversations/{id}/attachments/{att_id}) -> Preview in Canvas App.
		await convWindow.getByRole('link', { name: /summary\.html/ }).click();
		await expect(popover).toBeVisible();
		await expect(popover).toContainText('summary.html');
		await popover.getByRole('menuitem', { name: 'Preview in Canvas App' }).click();
		const convAppCard = page.locator('[data-canvas-app-card]').first();
		await expect(convAppCard).toBeVisible();
		await expect(convAppCard.frameLocator('[data-canvas-app-frame]').locator('#conv-report-heading')).toHaveText('Conversation HTML Report');

		// 2. Conversation image attachment -> Preview in Canvas Image Viewer.
		await page.locator('[data-canvas-fit]').click();
		await convWindow.getByRole('link', { name: /diagram\.png/ }).click();
		await expect(popover).toBeVisible();
		await expect(popover).toContainText('diagram.png');
		await popover.getByRole('menuitem', { name: 'Preview in Canvas Image Viewer' }).click();
		const convImageCard = page.locator('[data-canvas-image-card]').first();
		await expect(convImageCard).toBeVisible();
		await expect(convImageCard.locator('img')).toHaveAttribute('src', '/api/conversations/conv-alpha/attachments/conv-att-img');

		// 3. File in bootstrap_root outside the nested active repository must NOT open in Canvas Editor.
		await page.locator('[data-canvas-fit]').click();
		await convWindow.getByRole('link', { name: 'outside.txt' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Open in Canvas Editor' }).click();
		await expect(popover.getByRole('status')).toContainText('outside the agent workspace');
		await expect(page.locator('[data-canvas-editor-card]')).toHaveCount(0);
		await page.keyboard.press('Escape');

		// 4. File inside the nested active repository resolves cleanly to main.ts in Canvas Editor.
		await convWindow.getByRole('link', { name: 'main.ts' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Open in Canvas Editor' }).click();
		const editorCard = page.locator('[data-canvas-editor-card]').first();
		await expect(editorCard).toBeVisible();
		await expect(editorCard.getByLabel('Workspace file path')).toHaveValue('main.ts');

		// 5. After switching the active repository to repo-b while Canvas remains mounted,
		// links into the old repo no longer resolve into the new active workspace, while repo-b links do.
		activeRepoName = 'repo-b';
		await page.locator('[data-canvas-fit]').click();
		await convWindow.getByRole('link', { name: 'main.ts' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Open in Canvas Editor' }).click();
		await expect(popover.getByRole('status')).toContainText('outside the agent workspace');
		await page.keyboard.press('Escape');

		await convWindow.getByRole('link', { name: 'other.ts' }).click();
		await expect(popover).toBeVisible();
		await popover.getByRole('menuitem', { name: 'Open in Canvas Editor' }).click();
		const secondEditorCard = page.locator('[data-canvas-editor-card]').nth(1);
		await expect(secondEditorCard).toBeVisible();
		await expect(secondEditorCard.getByLabel('Workspace file path')).toHaveValue('other.ts');
	});

	test('zooms when scrolling on the minimap, isolates projects via the project picker, and drags an entire project zone group', async ({ page }) => {
		let alphaTaskPolls = 0;
		await page.addInitScript(() => {
			const listeners: Array<(chunk: string) => void> = [];
			const originalFetch = window.fetch.bind(window);
			const encoder = new TextEncoder();
			let revisions = { seq: 1, projects: 1, agents: 1, conversations: 1, tasks: 1, sessions: 1, schedules: 1, workflows: 1 };
			window.fetch = (input, init) => {
				const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
				if (new URL(url, window.location.origin).pathname === '/api/ui/stream') {
					const stream = new ReadableStream<Uint8Array>({
						start(controller) {
							let closed = false;
							const push = (chunk: string) => {
								if (closed) return;
								controller.enqueue(encoder.encode(chunk));
							};
							listeners.push(push);
							push(`event: revisions\ndata: ${JSON.stringify(revisions)}\n\n`);
							init?.signal?.addEventListener('abort', () => {
								if (closed) return;
								closed = true;
								const idx = listeners.indexOf(push);
								if (idx >= 0) listeners.splice(idx, 1);
								controller.close();
							});
						},
					});
					return Promise.resolve(new Response(stream, {
						status: 200,
						headers: { 'Content-Type': 'text/event-stream' },
					}));
				}
				return originalFetch(input, init);
			};
			(window as unknown as { __emitCanvasRevisions?: (patch: Partial<typeof revisions>) => void }).__emitCanvasRevisions = (patch) => {
				revisions = { ...revisions, ...patch, seq: revisions.seq + 1 };
				const frame = `event: revisions\ndata: ${JSON.stringify(revisions)}\n\n`;
				for (const push of [...listeners]) push(frame);
			};
		});
		await mockCanvasApi(page);
		await page.route('**/api/tasks/task-parent/activity**', async (route) => {
			alphaTaskPolls += 1;
			return route.fulfill({
				json: {
					attempts: [],
					events: [],
					has_more_before: false,
					has_more_after: false,
					delegated_work: [],
				},
			});
		});

		await page.setViewportSize({ width: 1440, height: 900 });
		await page.goto('/canvas');
		const canvas = page.locator('[data-workspace-canvas]');
		await expect(canvas).toBeVisible();
		await expect(page.locator('[data-canvas-project-card="proj-alpha"]')).toBeVisible();
		await expect(page.locator('[data-canvas-project-card="proj-beta"]')).toBeVisible();

		// Verify live SSE revision events actually trigger polls on active Alpha Core tasks (makes the later isolation check falsifiable).
		const pollsBeforeActiveEmit = alphaTaskPolls;
		await page.evaluate(() => {
			(window as unknown as { __emitCanvasRevisions?: (patch: Record<string, number>) => void }).__emitCanvasRevisions?.({ tasks: 2, sessions: 2 });
		});
		await expect.poll(() => alphaTaskPolls).toBeGreaterThan(pollsBeforeActiveEmit);

		// 1. Scrolling inside the minimap zooms the canvas without requiring Ctrl/Cmd.
		const minimap = page.locator('[data-canvas-minimap]');
		await expect(minimap).toBeVisible();
		const initialZoom = Number(await canvas.getAttribute('data-canvas-zoom'));
		const minimapBox = await minimap.boundingBox();
		expect(minimapBox).not.toBeNull();
		await page.mouse.move(minimapBox!.x + minimapBox!.width / 2, minimapBox!.y + minimapBox!.height / 2);
		await page.mouse.wheel(0, -120);
		await expect.poll(async () => Number(await canvas.getAttribute('data-canvas-zoom'))).toBeGreaterThan(initialZoom);
		const zoomedIn = Number(await canvas.getAttribute('data-canvas-zoom'));
		await page.mouse.wheel(0, 120);
		await page.mouse.wheel(0, 120);
		await expect.poll(async () => Number(await canvas.getAttribute('data-canvas-zoom'))).toBeLessThan(zoomedIn);

		// 2. Dragging the project group header moves all windows in that project rigidly together.
		const parseCoords = (style: string | null) => {
			const left = Number(style?.match(/left:\s*(-?\d+(?:\.\d+)?)px/)?.[1] ?? NaN);
			const top = Number(style?.match(/top:\s*(-?\d+(?:\.\d+)?)px/)?.[1] ?? NaN);
			return { left, top };
		};
		await page.locator('[data-canvas-fit]').click();
		const alphaWindowLocators = [
			page.locator('[data-canvas-node]', { has: page.locator('[data-canvas-project-card="proj-alpha"]') }),
			page.locator('[data-canvas-node-kind="task"]').nth(0),
			page.locator('[data-canvas-node-kind="task"]').nth(1),
			page.locator('[data-canvas-node-kind="task"]').nth(2),
		];
		const betaWindow = page.locator('[data-canvas-node]', { has: page.locator('[data-canvas-project-card="proj-beta"]') });

		const alphaBefore = await Promise.all(alphaWindowLocators.map(async (loc) => parseCoords(await loc.getAttribute('style'))));
		const betaBefore = parseCoords(await betaWindow.getAttribute('style'));

		const zoneHandle = page.locator('[data-canvas-zone-drag-handle="proj-alpha"]');
		await expect(zoneHandle).toBeVisible();
		const handleBox = await zoneHandle.boundingBox();
		expect(handleBox).not.toBeNull();
		await page.mouse.move(handleBox!.x + handleBox!.width / 2, handleBox!.y + handleBox!.height / 2);
		await page.mouse.down();
		await page.mouse.move(handleBox!.x + handleBox!.width / 2 + 120, handleBox!.y + handleBox!.height / 2 + 80, { steps: 6 });
		await page.mouse.up();

		const alphaAfter = await Promise.all(alphaWindowLocators.map(async (loc) => parseCoords(await loc.getAttribute('style'))));
		const betaAfter = parseCoords(await betaWindow.getAttribute('style'));
		const dx0 = alphaAfter[0].left - alphaBefore[0].left;
		const dy0 = alphaAfter[0].top - alphaBefore[0].top;
		expect(dx0).toBeGreaterThan(40);
		expect(dy0).toBeGreaterThan(20);
		for (let i = 1; i < alphaAfter.length; i += 1) {
			expect(alphaAfter[i].left - alphaBefore[i].left).toBe(dx0);
			expect(alphaAfter[i].top - alphaBefore[i].top).toBe(dy0);
		}
		expect(betaAfter).toEqual(betaBefore);

		// Reloading persists the moved project cluster coordinates.
		await page.reload();
		await expect(canvas).toBeVisible();
		const alphaRestored = await Promise.all(alphaWindowLocators.map(async (loc) => parseCoords(await loc.getAttribute('style'))));
		expect(alphaRestored).toEqual(alphaAfter);

		// Opening a project from the Add menu while in All Projects does NOT isolate/unmount other projects.
		await page.locator('[data-canvas-add-button]').click();
		await page.locator('[data-canvas-add-menu]').getByRole('button', { name: 'Alpha Core' }).click();
		await expect(canvas).toHaveAttribute('data-canvas-project-filter', 'all');
		await expect(page.locator('[data-canvas-project-card="proj-beta"]')).toBeVisible();

		// 3. Selecting a project in the top project picker isolates it and hides/deactivates other projects.
		const alphaCard = page.locator('[data-canvas-project-card="proj-alpha"]');
		await alphaCard.getByRole('button', { name: 'Editor', exact: true }).click();
		const alphaEditor = page.locator('[data-canvas-editor-card]').first();
		await expect(alphaEditor).toBeVisible();
		await alphaEditor.getByRole('button', { name: 'main.ts' }).click();
		await expect(alphaEditor.locator('[data-monaco-editor] .view-lines')).toContainText('hello canvas');
		await alphaEditor.locator('[data-monaco-editor]').click();
		await page.keyboard.type('draft-keep ');
		await expect(alphaEditor.getByRole('button', { name: /Save •/ })).toBeVisible();

		// Also open a Conversation window with an unsaved attachment so we verify non-editor windows with unsaved state are retained too.
		await page.locator('[data-canvas-fit]').click();
		await alphaCard.getByRole('button', { name: /Release Sync/ }).click();
		const alphaConvWindow = page.locator('[data-canvas-node-kind="conversation"]').first();
		await expect(alphaConvWindow).toBeVisible();
		await alphaConvWindow.locator('[data-conversation-attachment-input]').setInputFiles({
			name: 'draft.png',
			mimeType: 'image/png',
			buffer: png1x1,
		});
		await expect(alphaConvWindow.locator('img[alt="draft.png"]')).toBeVisible();

		// Switch to Beta UI: clean Alpha Core windows/edges/zones unmount, while dirty editor & unsaved conversation stay hidden.
		await page.locator('[data-canvas-hud]').getByRole('button', { name: /Beta UI/ }).click();
		await expect(canvas).toHaveAttribute('data-canvas-project-filter', 'proj-beta');
		await expect(page.locator('[data-canvas-project-card="proj-beta"]')).toBeVisible();
		await expect(page.locator('[data-canvas-project-card="proj-alpha"]')).toHaveCount(0);
		await expect(page.locator('[data-canvas-node-kind="task"]')).toHaveCount(0);
		await expect(page.locator('[data-canvas-edge-kind="delegation"]')).toHaveCount(0);
		await expect(page.locator('[data-canvas-project-zone="proj-alpha"]')).toHaveCount(0);
		await expect(alphaEditor).not.toBeVisible();
		await expect(alphaConvWindow).not.toBeVisible();

		// Emit a real SSE revision push while isolated to Beta UI: unmounted Alpha Core tasks make 0 polls.
		const pollsAfterSwitch = alphaTaskPolls;
		await page.evaluate(() => {
			(window as unknown as { __emitCanvasRevisions?: (patch: Record<string, number>) => void }).__emitCanvasRevisions?.({ tasks: 3, sessions: 3 });
		});
		await page.waitForTimeout(250);
		expect(alphaTaskPolls).toBe(pollsAfterSwitch);

		// Clearing while filtered to Beta UI removes only Beta UI windows and preserves hidden Alpha Core windows.
		await page.getByRole('button', { name: 'Clear Beta UI windows' }).click();
		await expect(page.locator('[data-canvas-project-card="proj-beta"]')).toHaveCount(0);
		await expect(page.locator('[data-canvas-empty]')).toContainText('No windows open in Beta UI');

		// Switching to Alpha Core restores Alpha windows, preserves both dirty editor & unsaved attachment state, and resumes live updates.
		await page.locator('[data-canvas-hud]').getByRole('button', { name: /Alpha Core/ }).click();
		await expect(canvas).toHaveAttribute('data-canvas-project-filter', 'proj-alpha');
		await expect(page.locator('[data-canvas-project-card="proj-alpha"]')).toBeVisible();
		await expect(page.locator('[data-canvas-node-kind="task"]')).toHaveCount(3);
		await expect(alphaEditor).toBeVisible();
		await expect(alphaEditor.getByRole('button', { name: /Save •/ })).toBeVisible();
		await expect(alphaEditor.locator('[data-monaco-editor] .view-lines')).toContainText('draft-keep');
		await expect(alphaConvWindow).toBeVisible();
		await expect(alphaConvWindow.locator('img[alt="draft.png"]')).toBeVisible();
		await expect.poll(() => alphaTaskPolls).toBeGreaterThan(pollsAfterSwitch);

		// Expanding a collapsed task window resumes live SSE updates for it immediately.
		const parentTaskWindow = page.locator('[data-canvas-node-kind="task"]', { hasText: 'Implement Spatial Canvas' }).first();
		await parentTaskWindow.getByRole('button', { name: /^Collapse / }).click();
		const pollsWhileCollapsed = alphaTaskPolls;
		await page.evaluate(() => {
			(window as unknown as { __emitCanvasRevisions?: (patch: Record<string, number>) => void }).__emitCanvasRevisions?.({ tasks: 4, sessions: 4 });
		});
		await page.waitForTimeout(250);
		expect(alphaTaskPolls).toBe(pollsWhileCollapsed);
		await parentTaskWindow.getByRole('button', { name: /^Expand / }).click();
		await expect.poll(() => alphaTaskPolls).toBeGreaterThan(pollsWhileCollapsed);

		// Clicking All Projects returns to the unfiltered view.
		await page.locator('[data-canvas-hud]').getByRole('button', { name: 'All Projects' }).click();
		await expect(canvas).toHaveAttribute('data-canvas-project-filter', 'all');
		await expect(page.locator('[data-canvas-project-card="proj-alpha"]')).toBeVisible();
	});

	test('canvas model enforces safe app URLs, non-overlapping placement, delegation edges, and layout persistence', () => {
		const origin = 'http://127.0.0.1:8935';
		const localApp = classifyAppUrl('http://127.0.0.1:3000/dashboard', origin);
		expect(localApp.valid).toBe(true);
		expect(localApp.sameOrigin).toBe(false);
		expect(localApp.sandbox).toBe('allow-scripts allow-forms allow-same-origin allow-popups');

		const sameOriginPreview = classifyAppUrl('http://127.0.0.1:8935/preview', origin);
		expect(sameOriginPreview.valid).toBe(true);
		expect(sameOriginPreview.sandbox).toBe('allow-scripts allow-forms');

		expect(classifyAppUrl('http://127.0.0.1:8935/api/tasks', origin).valid).toBe(false);
		expect(classifyAppUrl('javascript:alert(1)', origin).valid).toBe(false);

		expect(isImageFilePath('assets/Logo.PNG')).toBe(true);
		expect(isImageFilePath('src/main.rs')).toBe(false);
		expect(isHtmlFilePath('apps/demo/index.HTML')).toBe(true);
		expect(isHtmlFilePath('README.md')).toBe(false);

		const parent = createCanvasNode({ kind: 'project', title: 'Alpha', projectId: 'proj-alpha', x: 80, y: 80 }, []);
		const child1 = createCanvasNode({ kind: 'editor', title: 'main.ts', parentId: parent.id, projectId: 'proj-alpha' }, [parent]);
		const child2 = createCanvasNode({ kind: 'terminal', title: 'shell', parentId: parent.id, projectId: 'proj-alpha' }, [parent, child1]);
		expect(child1.x).toBeGreaterThanOrEqual(parent.x + parent.width);
		expect(child2.y).not.toBe(child1.y);

		const seeded = seedCanvasNodes(mockProjects, mockAgents, mockConversations, mockTasks);
		const laidOut = tidyCanvasLayout(seeded, mockTasks, mockProjects);
		const projNode = laidOut.find((n) => n.kind === 'project' && n.projectId === 'proj-alpha')!;
		const parentTaskNode = laidOut.find((n) => n.taskId === 'task-parent')!;
		const childTaskNode = laidOut.find((n) => n.taskId === 'task-child-1')!;
		expect(projNode.x).toBeLessThan(parentTaskNode.x);
		expect(parentTaskNode.x).toBeLessThan(childTaskNode.x);

		// Tidying an isolated project keeps it anchored at its own origin rather than stacking onto hidden projects at (80, 80).
		const betaNodes = laidOut.filter((n) => n.projectId === 'proj-beta');
		const betaOriginY = Math.min(...betaNodes.map((n) => n.y));
		const filteredTidy = tidyFilteredCanvasNodes(laidOut, betaNodes, mockTasks, mockProjects, true);
		const tidiedBeta = filteredTidy.tidiedVisible[0];
		expect(tidiedBeta.y).toBeGreaterThanOrEqual(betaOriginY);
		expect(filteredTidy.nodes.find((n) => n.id === projNode.id)?.y).toBe(projNode.y);

		const edges = deriveCanvasEdges(laidOut, mockTasks, mockAgents, mockConversations);
		expect(edges.some((e) => e.fromId === parentTaskNode.id && e.toId === childTaskNode.id && e.kind === 'delegation')).toBe(true);

		const zones = computeProjectZones(laidOut, mockProjects);
		expect(zones.some((z) => z.projectId === 'proj-alpha' && z.nodeCount >= 4)).toBe(true);
		expect(bezierEdgePath(parentTaskNode, childTaskNode).path).toMatch(/^M \d+/);
		expect(computeFitCamera(laidOut, 1440, 900).zoom).toBeLessThanOrEqual(1);

		const restored = normalizePersistedCanvasState({
			nodes: [...laidOut, { bad: true } as unknown as CanvasNode],
			camera: { x: 10, y: 20, zoom: 99 },
			focusedNodeId: parentTaskNode.id,
			activeProjectFilter: 'proj-alpha',
			snapToGrid: false,
			showMinimap: true,
			autoPinDelegated: true,
		});
		expect(restored?.nodes.length).toBe(laidOut.length);
		expect(restored?.camera.zoom).toBe(1.6);
		expect(restored?.activeProjectFilter).toBe('proj-alpha');
		expect(restored?.snapToGrid).toBe(false);

		const restoredAll = normalizePersistedCanvasState({
			nodes: laidOut,
			camera: { x: 0, y: 0, zoom: 1 },
			activeProjectFilter: '',
		});
		expect(restoredAll?.activeProjectFilter).toBeNull();

		// Link classification & workspace path mapping helpers
		expect(resolveWorkspaceRelativePath('/workspace/temp/demo.html', ['/workspace'])).toBe('temp/demo.html');
		expect(resolveWorkspaceRelativePath('/tmp/outside.html', ['/workspace'])).toBeNull();
		expect(resolveWorkspaceRelativePath('/workspace/../etc/passwd', ['/workspace'])).toBeNull();

		// Nested repository active roots use container_root and exclude bootstrap_root / host root
		// so container download paths stay in the container namespace and sibling files outside the active repo never alias into it.
		const nestedRoots = extractActiveWorkspaceRoots({
			root: '/srv/agent/repo',
			container_root: '/workspace/repo',
			repository: {
				active: { root: '/srv/agent/repo' },
				bootstrap_root: '/srv/agent',
			} as { active: { root: string }; bootstrap_root: string },
		});
		expect(nestedRoots).toEqual(['/workspace/repo']);
		expect(resolveWorkspaceRelativePath('/srv/agent/notes.txt', nestedRoots)).toBeNull();
		expect(resolveWorkspaceRelativePath('/workspace/notes.txt', nestedRoots)).toBeNull();
		expect(resolveWorkspaceRelativePath('/srv/agent/repo/notes.txt', nestedRoots)).toBeNull();
		expect(resolveWorkspaceRelativePath('/workspace/repo/notes.txt', nestedRoots)).toBe('notes.txt');

		// Overlapping host/container roots (e.g. host root `/workspace/project` vs container_root `/workspace`)
		// must resolve container downloads against `container_root` rather than stripping the host prefix.
		const overlappingRoots = extractActiveWorkspaceRoots({
			root: '/workspace/project',
			container_root: '/workspace',
			repository: {
				active: { root: '/workspace/project' },
				bootstrap_root: '/workspace/project',
			} as { active: { root: string }; bootstrap_root: string },
		});
		expect(overlappingRoots).toEqual(['/workspace']);
		expect(resolveWorkspaceRelativePath('/workspace/project/notes.txt', overlappingRoots)).toBe('project/notes.txt');

		// Host-backed agents report container_root === root, still excluding bootstrap_root.
		const hostRoots = extractActiveWorkspaceRoots({
			root: '/srv/agent/repo',
			container_root: '/srv/agent/repo',
			repository: {
				active: { root: '/srv/agent/repo' },
				bootstrap_root: '/srv/agent',
			} as { active: { root: string }; bootstrap_root: string },
		});
		expect(hostRoots).toEqual(['/srv/agent/repo']);
		expect(resolveWorkspaceRelativePath('/srv/agent/notes.txt', hostRoots)).toBeNull();
		expect(resolveWorkspaceRelativePath('/srv/agent/repo/notes.txt', hostRoots)).toBe('notes.txt');

		expect(classifyCanvasLink({ href: '/tasks/task-1' }, origin)).toBeNull();
		expect(classifyCanvasLink({ href: '/projects/proj-alpha' }, origin)).toBeNull();
		expect(classifyCanvasLink({ href: '/api/environments/agent-1/download?path=%2Fworkspace%2Farchive.zip' }, origin)).toBeNull();

		const envHtml = classifyCanvasLink({ href: '/api/environments/agent-1/download?path=%2Fworkspace%2Fdemo.html', downloadAttr: '' }, origin);
		expect(envHtml?.category).toBe('html');
		expect(envHtml?.downloadAgentId).toBe('agent-1');
		expect(envHtml?.downloadPath).toBe('/workspace/demo.html');

		const envCode = classifyCanvasLink({ href: '/api/environments/agent-1/download?path=%2Fworkspace%2Fsrc%2Fmain.ts', downloadAttr: '' }, origin);
		expect(envCode?.category).toBe('file');

		const attImg = classifyCanvasLink({ href: '/api/tasks/t1/messages/1/attachments/a1', hasImageChild: true, imageAlt: 'shot.png' }, origin);
		expect(attImg?.category).toBe('image');
		expect(attImg?.isAttachment).toBe(true);

		const attHtml = classifyCanvasLink({ href: '/api/tasks/t1/messages/1/attachments/a2', downloadAttr: 'report.html' }, origin);
		expect(attHtml?.category).toBe('html');
		expect(attHtml?.isAttachment).toBe(true);

		const convAttImg = classifyCanvasLink({
			href: '/api/conversations/conv-alpha/attachments/c-img',
			text: 'shot.png 68 B',
			hasImageChild: true,
			imageAlt: 'shot.png',
			attachmentName: 'shot.png',
			attachmentMimeType: 'image/png',
		}, origin);
		expect(convAttImg?.category).toBe('image');
		expect(convAttImg?.label).toBe('shot.png');
		expect(convAttImg?.isAttachment).toBe(true);

		const convAttHtml = classifyCanvasLink({
			href: '/api/conversations/conv-alpha/attachments/c-html',
			text: 'report.html 64 B',
			attachmentName: 'report.html',
			attachmentMimeType: 'text/html',
		}, origin);
		expect(convAttHtml?.category).toBe('html');
		expect(convAttHtml?.label).toBe('report.html');
		expect(convAttHtml?.isAttachment).toBe(true);

		const extWeb = classifyCanvasLink({ href: 'http://127.0.0.1:3000/app', text: 'App' }, origin);
		expect(extWeb?.category).toBe('web');

		expect(isAllowedHtmlPreviewEndpoint('/api/tasks/t1/messages/1/attachments/a2', origin)).toBe(true);
		expect(isAllowedHtmlPreviewEndpoint('/api/conversations/conv-alpha/attachments/c-html', origin)).toBe(true);
		expect(isAllowedHtmlPreviewEndpoint('/api/conversations/conv-alpha/messages/1/attachments/c-html', origin)).toBe(false);
		expect(isAllowedHtmlPreviewEndpoint('/api/environments/agent-1/download?path=%2Ftmp%2Findex.html', origin)).toBe(true);
		expect(isAllowedHtmlPreviewEndpoint('/api/environments/agent-1/download?path=%2Ftmp%2Fsecret.env', origin)).toBe(false);
		expect(isAllowedHtmlPreviewEndpoint('/api/auth/bootstrap', origin)).toBe(false);
	});
});
