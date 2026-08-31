import { expect, test, type Locator, type Page } from '@playwright/test';

const taskId = 'task-browser-test';
const agentId = 'project-browser-test';
const projectId = 'collaboration-project-test';
const conversationId = 'conversation-browser-test';
const startTime = Date.parse('2026-07-19T00:00:00.000Z');

function timestamp(second: number): string {
	return new Date(startTime + second * 1_000).toISOString();
}

function recentSqliteTimestamp(millisecondsAgo: number): string {
	return new Date(Date.now() - millisecondsAgo).toISOString().slice(0, 19).replace('T', ' ');
}

function visualizationDocument(artifactId: string, label: string, followUp?: { prompt: string; title?: string }): string {
	const action = followUp
		? `<button id="visual-follow-up" type="button">Follow up</button><script>
			window.openai = Object.freeze({ sendFollowUpMessage(value) {
				parent.postMessage({ source: 'xpressclaw-visualization', type: 'follow-up-request', artifactId: ${JSON.stringify(artifactId)}, requestId: 'request-browser-test', prompt: value.prompt, title: value.title || null }, '*');
			} });
			document.querySelector('#visual-follow-up').addEventListener('click', () => window.openai.sendFollowUpMessage(${JSON.stringify(followUp)}));
		</script>`
		: '';
	const artifactDocument = `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"></head><body><div id="visual-content">${label}</div>${action}<script>
		addEventListener('message', (event) => {
			if (event.source === parent && event.data?.source === 'xpressclaw-host' && event.data.artifactId === ${JSON.stringify(artifactId)} && event.data.type === 'theme') {
				document.documentElement.dataset.theme = event.data.theme;
			}
		});
		try { void parent.document.body; document.documentElement.dataset.parentBlocked = 'false'; } catch { document.documentElement.dataset.parentBlocked = 'true'; }
		try { localStorage.setItem('artifact-test', 'bad'); document.documentElement.dataset.storageBlocked = 'false'; } catch { document.documentElement.dataset.storageBlocked = 'true'; }
		try { document.cookie = 'artifact-test=bad'; document.documentElement.dataset.cookieBlocked = document.cookie ? 'false' : 'true'; } catch { document.documentElement.dataset.cookieBlocked = 'true'; }
		try { const popup = window.open('about:blank'); document.documentElement.dataset.popupBlocked = popup === null ? 'true' : 'false'; popup?.close(); } catch { document.documentElement.dataset.popupBlocked = 'true'; }
		try { top.location.href = 'https://example.invalid/visualization-top-navigation'; document.documentElement.dataset.navigationBlocked = 'false'; } catch { document.documentElement.dataset.navigationBlocked = 'true'; }
		fetch('https://example.invalid/visualization-network').then(() => { document.documentElement.dataset.networkBlocked = 'false'; }).catch(() => { document.documentElement.dataset.networkBlocked = 'true'; });
		const form = document.createElement('form'); form.action = 'https://example.invalid/visualization-form'; form.method = 'post'; document.body.append(form); const beforeSubmit = location.href; form.submit(); setTimeout(() => { document.documentElement.dataset.formBlocked = location.href === beforeSubmit ? 'true' : 'false'; }, 0);
	</script></body></html>`;
	const encodedDocument = Buffer.from(artifactDocument, 'utf8').toString('base64');
	return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-src blob:; child-src blob:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"><style>html,body,#root,iframe{width:100%;height:100%;margin:0;border:0}#blocked[hidden]{display:none}</style><script>
		const artifactId = ${JSON.stringify(artifactId)};
		let child;
		let childUrl;
		let initialLoadComplete = false;
		let lastTheme = null;
		addEventListener('message', (event) => {
			const data = event.data;
			if (!data || typeof data !== 'object') return;
			if (event.source === parent) {
				if (data.source !== 'xpressclaw-host' || data.artifactId !== artifactId) return;
				if (data.type === 'theme') lastTheme = data.theme;
				if (data.type === 'theme' || data.type === 'follow-up-result') child?.contentWindow?.postMessage(data, '*');
				return;
			}
			if (event.source !== child?.contentWindow || data.source !== 'xpressclaw-visualization' || data.artifactId !== artifactId) return;
			if (data.type === 'resize' || data.type === 'follow-up-request') parent.postMessage(data, '*');
		});
		addEventListener('DOMContentLoaded', () => {
			const binary = atob(${JSON.stringify(encodedDocument)});
			const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
			childUrl = URL.createObjectURL(new Blob([bytes], { type: 'text/html;charset=utf-8' }));
			child = document.createElement('iframe');
			child.dataset.artifactFrame = '';
			child.title = ${JSON.stringify(label)};
			child.setAttribute('sandbox', 'allow-scripts');
			child.setAttribute('referrerpolicy', 'no-referrer');
			child.addEventListener('load', () => {
				if (initialLoadComplete) {
					child.remove();
					document.querySelector('#blocked').hidden = false;
					parent.postMessage({ source: 'xpressclaw-visualization', type: 'resize', artifactId, height: 160 }, '*');
					return;
				}
				initialLoadComplete = true;
				if (lastTheme) child.contentWindow?.postMessage({ source: 'xpressclaw-host', type: 'theme', artifactId, theme: lastTheme }, '*');
			});
			child.src = childUrl;
			document.querySelector('#root').append(child);
		});
	</script></head><body><div id="root"></div><div id="blocked" role="status" hidden>Navigation was blocked.</div></body></html>`;
}

async function expectVerticalScroll(scroller: Locator) {
	await expect(scroller).toBeVisible();
	await expect(scroller).toHaveCSS('overflow-y', 'auto');
	await expect.poll(() => scroller.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
	await scroller.evaluate((element) => element.scrollTo({ top: element.scrollHeight }));
	await expect.poll(() => scroller.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
}

async function elapsedSeconds(indicator: Locator): Promise<number> {
	const text = await indicator.locator('[data-elapsed-time]').innerText();
	expect(text).toMatch(/^\d{1,2}\.\d+s$/);
	return Number.parseFloat(text);
}

function cssRgb(value: string): [number, number, number] {
	const channels = value.match(/[\d.]+/g)?.slice(0, 3).map(Number);
	if (!channels || channels.length !== 3) throw new Error(`Could not parse CSS color: ${value}`);
	return channels as [number, number, number];
}

function contrastRatio(foreground: string, background: string): number {
	const luminance = (value: string) => {
		const channels = cssRgb(value).map((channel) => {
			const normalized = channel / 255;
			return normalized <= 0.04045
				? normalized / 12.92
				: ((normalized + 0.055) / 1.055) ** 2.4;
		});
		return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
	};
	const lighter = Math.max(luminance(foreground), luminance(background));
	const darker = Math.min(luminance(foreground), luminance(background));
	return (lighter + 0.05) / (darker + 0.05);
}

async function expectExternalPopup(page: Page, buttonName: string, expectedUrl: string) {
	await page.context().route(expectedUrl, async (route) => {
		await route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>External documentation</title>' });
	}, { times: 1 });
	const popupPromise = page.waitForEvent('popup');
	await page.getByRole('button', { name: buttonName, exact: true }).click();
	const popup = await popupPromise;
	await expect.poll(() => popup.url()).toBe(expectedUrl);
	await popup.close();
}

async function expectReadableCode(message: Locator) {
	const inlineCode = message.locator('.prose-chat p code').first();
	const fencedCode = message.locator('.prose-chat pre').first();
	await expect(inlineCode).toBeVisible();
	await expect(fencedCode).toBeVisible();

	const inlineStyles = await inlineCode.evaluate((element) => {
		const styles = getComputedStyle(element);
		return {
			color: styles.color,
			background: styles.backgroundColor,
			borderStyle: styles.borderStyle,
			borderWidth: styles.borderWidth,
			userSelect: styles.userSelect,
		};
	});
	const fencedStyles = await fencedCode.evaluate((element) => {
		const styles = getComputedStyle(element);
		const codeStyles = getComputedStyle(element.querySelector('code')!);
		return {
			color: codeStyles.color,
			background: styles.backgroundColor,
			borderStyle: styles.borderStyle,
			borderWidth: styles.borderWidth,
			overflowX: styles.overflowX,
			userSelect: codeStyles.userSelect,
		};
	});

	expect(contrastRatio(inlineStyles.color, inlineStyles.background)).toBeGreaterThanOrEqual(4.5);
	expect(contrastRatio(fencedStyles.color, fencedStyles.background)).toBeGreaterThanOrEqual(4.5);
	expect(inlineStyles.borderStyle).not.toBe('none');
	expect(inlineStyles.borderWidth).not.toBe('0px');
	expect(fencedStyles.borderStyle).not.toBe('none');
	expect(fencedStyles.borderWidth).not.toBe('0px');
	expect(fencedStyles.overflowX).toBe('auto');
	expect(inlineStyles.userSelect).not.toBe('none');
	expect(fencedStyles.userSelect).not.toBe('none');
}

async function installTauriClipboardImage(page: Page) {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'isTauri', { value: true });
		(window as unknown as { __clipboardCommands: string[] }).__clipboardCommands = [];
		(window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> } }).__TAURI_INTERNALS__ = {
			invoke: async (command: string) => {
				if (command === 'get_active_instance_profile') return { identity_status: 'matched', local: true };
				(window as unknown as { __clipboardCommands: string[] }).__clipboardCommands.push(command);
				if (command === 'plugin:clipboard-manager|read_image') return 42;
				if (command === 'plugin:image|rgba') return [255, 0, 0, 255];
				if (command === 'plugin:image|size') return { width: 1, height: 1 };
				if (command === 'plugin:resources|close') return null;
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
}

function activityEvent(id: number, prefix = id <= 20 ? 'Earlier activity' : 'Current activity') {
	return {
		id,
		session_id: agentId,
		attempt_id: 'attempt-browser-test',
		task_id: taskId,
		source_type: 'acp',
		source_id: 'codex',
		event_type: 'runner_progress',
		summary: `${prefix} ${id}`,
		payload: { item_type: 'command', command: `echo ${id}` },
		created_at: timestamp(id),
	};
}

function timelineEvent(id: number, second: number, eventType: string, summary: string, payload: Record<string, unknown>) {
	return {
		...activityEvent(id),
		event_type: eventType,
		summary,
		payload,
		created_at: timestamp(second),
	};
}

function attempt(
	status: string,
	contextUsed = 128_000,
	errorMessage: string | null = null,
	result: string | null = null,
	overrides: Record<string, unknown> = {},
) {
	return {
		id: 'attempt-browser-test',
		session_id: agentId,
		task_id: taskId,
		queue_id: 1,
		kind: 'message',
		runner: 'codex',
		status,
		prompt: 'Test the workspace',
		native_session_id: 'native-browser-test',
		container_id: 'container-browser-test',
		result,
		error_message: errorMessage,
		created_at: timestamp(0),
		started_at: timestamp(1),
		completed_at: status === 'completed' ? timestamp(61) : null,
		context_used: contextUsed,
		context_size: 256_000,
		trigger_message_id: null,
		response_queued_at: timestamp(0),
		response_started_at: status === 'running' ? timestamp(1) : null,
		...overrides,
	};
}

interface MockProject {
	id: string;
	name: string;
	description: string | null;
	icon: string | null;
	created_at: string;
	updated_at: string;
	agent_ids: string[];
	conversation_count: number;
	task_count: number;
	deletion_started_at?: string | null;
	deletion_counts?: {
		agents: number;
		tasks: number;
		task_messages: number;
		conversations: number;
		conversation_messages: number;
		memory_notes: number;
		workflow_runs: number;
		schedules: number;
	};
}

interface SharedProjectState {
	project?: MockProject;
	deleted?: boolean;
}

async function mockApi(
	page: Page,
	options: {
		live?: boolean;
		attemptError?: string;
		taskLoadFailureOnce?: boolean;
		agentTimeline?: boolean;
		agentResponseLinks?: boolean;
		richToolActivity?: boolean;
		postedMessages?: Record<string, unknown>[];
		interruptedAttempts?: string[];
		pendingElicitation?: Record<string, unknown>;
		elicitationResponses?: { elicitationId: string; payload: Record<string, unknown> }[];
		connection?: { online: boolean };
		multipleAgents?: boolean;
		projectCount?: number;
		queuedSessionMessages?: { agentId: string; payload: Record<string, unknown> }[];
		projectTaskUpdates?: number[];
		completedTaskCount?: number;
		taskTitle?: string;
		taskDescription?: string;
		taskStatus?: string;
		taskActivityStatus?: string;
		taskSubtasks?: Record<string, unknown>[];
		taskMessages?: Record<string, unknown>[];
		taskActivityEvents?: Record<string, unknown>[];
		taskAttempts?: Record<string, unknown>[];
		attemptResult?: string;
		mcpServers?: {
			name: string;
			type: 'stdio' | 'http' | 'sse';
			command: string | null;
			args: string[];
			url: string | null;
			env: Record<string, string>;
			headers: Record<string, string>;
		}[];
		mcpVerificationResults?: Record<string, {
			ok: boolean;
			status: string;
			message: string;
			suggestion: string | null;
		}>;
		mcpVerificationRequests?: { name: string; agent_id: string | null }[];
		openUrlRequests?: string[];
		agentCatalog?: {
			kind: string;
			name: string;
			install_url: string;
			image: string;
			host_image: string;
		}[];
		agentKind?: { value: string };
		workflows?: {
			id: string;
			name: string;
			description: string | null;
			yaml_content: string;
			enabled: boolean;
			version: number;
			created_at: string;
			updated_at: string;
			last_triggered_at?: string | null;
			trigger_count?: number;
			trigger_error?: string | null;
		}[];
		schedules?: {
			id: string;
			name: string;
			cron: string;
			agent_id: string;
			title: string;
			description: string | null;
			enabled: boolean;
			last_run: string | null;
			run_count: number;
			created_at: string;
			schedule_type: 'cron' | 'once';
			run_at: string | null;
			continuation_task_id: string | null;
			conversation_id: string | null;
		}[];
		workflowCreateRequests?: { name: string; description?: string; yaml_content: string }[];
		workflowRunRequests?: { id: string; inputs: Record<string, unknown>; projectId?: string }[];
		workspaceSaveRequests?: { path: string; content: string; expected_revision: string }[];
		workspaceSaveDelayMs?: number;
		workspaceReadRequests?: string[];
		workspaceReadDelaysMs?: Record<string, number>;
		repositoryStatus?: Record<string, unknown>;
		repositorySelections?: (string | null)[];
		includeDeletedWorkspaceFile?: boolean;
		conversations?: Record<string, unknown>[];
		conversationMessages?: Record<string, unknown>[];
		conversationTurns?: Record<string, unknown>[];
		conversationMessageRequests?: Record<string, unknown>[];
		conversationTaskRequests?: Record<string, unknown>[];
		projectSyncStatuses?: Record<string, unknown>[];
		projectSyncRequests?: { projectId: string; operation: 'fetch' | 'publish'; force: boolean }[];
		projectSyncFetchConflictOnce?: boolean;
		collaborationSettings?: Record<string, unknown>;
		collaborationActions?: string[];
		projectUpdateRequests?: { projectId: string; data: Record<string, unknown> }[];
		projectDeleteRequests?: string[];
		projectDeleteAcknowledgements?: (string | null)[];
		projectDeleteGate?: Promise<void>;
		projectDeleteError?: string;
		projectDeletionStartedAt?: string | null;
		projectDeletionCounts?: MockProject['deletion_counts'];
		projectGetRequests?: string[];
		projectGetGate?: Promise<void>;
		projectListRequests?: string[];
		projectListGate?: Promise<void>;
		projectListTargetLast?: boolean;
		secondaryProjectName?: string;
		secondaryProjectUpdatedAt?: string;
		sharedProjectState?: SharedProjectState;
		preserveWorkspace?: boolean;
		agentRunnerKind?: string;
		runnerReadiness?: Record<string, unknown>;
		visualizationDocuments?: Record<string, string>;
		visualizationRequests?: { path: string; token: string | undefined }[];
	} = {},
) {
	let liveEvent = 0;
	let contextUsed = 128_000;
	let createdWorkflow: Record<string, unknown> | null = null;
	let workspaceFileContent = 'export const greeting = "hello";\n';
	let workspaceFileRevision = 'revision-before-save';
	let repositoryStatus: Record<string, unknown> = options.repositoryStatus ?? {
		state: 'attached',
		message: 'The active repository and bundled GitHub MCP are ready.',
		bootstrap_root: '/srv/workspaces/browser-tested',
		active: { relative_path: 'xpressclaw', root: '/srv/workspaces/browser-tested/xpressclaw', github_repository: 'XpressAI/xpressclaw' },
		candidates: [
			{ relative_path: 'xpressclaw', root: '/srv/workspaces/browser-tested/xpressclaw', github_repository: 'XpressAI/xpressclaw' },
		],
		discovery_truncated: false,
		selected_relative_path: 'xpressclaw',
		pending_relative_path: null,
		pending_action: null,
		github_status: 'attached',
		github_repository: 'XpressAI/xpressclaw',
		restart_required: false,
	};
	let projectSyncConflictReturned = false;
	let taskLoadFailed = false;
	const status = options.taskStatus ?? (options.pendingElicitation ? 'waiting_for_input' : options.live ? 'in_progress' : 'completed');
	const attemptStatus = options.attemptError
		? 'failed'
		: options.pendingElicitation
			? 'waiting_for_input'
			: options.live ? 'running' : 'completed';
	const mockAttempt = () => attempt(
		attemptStatus,
		contextUsed,
		options.attemptError ?? null,
		options.attemptResult ?? null,
	);
	const firstAnswer = options.agentResponseLinks
		? 'First answer with [agent docs](https://example.com/agent-docs).'
		: 'First answer';
	const userFollowUp = options.agentResponseLinks
		? 'Please continue using [my reference](https://example.com/user-reference).'
		: 'Please continue';
	const secondAnswer = options.agentResponseLinks
		? 'Second answer with <a href="https://example.com/raw-agent-link" target="_self" rel="opener">a raw agent link</a>.'
		: 'Second answer';
	const firstAgentUpdate = options.agentResponseLinks
		? "Yes, I'll inspect the project.\n\nI'm starting with the [timeline component](https://example.com/timeline) so this update remains readable even when it spans multiple lines."
		: "Yes, I'll inspect the project.\n\nI'm starting with the timeline component so this update remains readable even when it spans multiple lines.";
	const task = {
		id: taskId,
		title: options.taskTitle ?? 'Browser-tested workspace',
		description: options.taskDescription ?? 'Inspect the project and report what you find.',
		status,
		priority: 0,
		agent_id: agentId,
		parent_task_id: null,
		sop_id: null,
		conversation_id: null,
		created_at: timestamp(0),
		updated_at: timestamp(61),
		completed_at: status === 'completed' ? timestamp(61) : null,
		context: {},
		provenance: 'durable',
		blocks_parent: true,
		activity_status: options.taskActivityStatus ?? status,
		depends_on: [],
		dependents: [],
		blocked_by: [],
		ready: true,
	};
	const agentRunnerKind = options.agentRunnerKind ?? 'codex';
	const agent = {
		id: agentId,
		name: 'browser-tested-workspace',
		title: 'Browser-tested workspace',
		backend: agentRunnerKind,
		project_id: projectId,
		status: options.live ? 'running' : 'stopped',
		desired_status: options.live ? 'running' : 'stopped',
		observed_status: options.live ? 'running' : 'stopped',
		container_id: options.live ? 'container-browser-test' : null,
		config: {
			runner: {
				kind: agentRunnerKind,
				workspace: '/srv/repos/xpressclaw',
				project_name: 'Browser-tested workspace',
				session_config: {},
			},
		},
		created_at: timestamp(0),
		started_at: options.live ? timestamp(1) : null,
		stopped_at: options.live ? null : timestamp(61),
		error_message: null,
		restart_count: 0,
	};
	const secondaryAgent = {
		...agent,
		id: 'project-secondary-test',
		name: 'secondary-browser-workspace',
		title: 'Secondary browser workspace',
	};
	const availableAgents = options.projectCount
		? Array.from({ length: options.projectCount }, (_, index) => index === 0 ? agent : {
			...agent,
			id: `project-mobile-${index + 1}`,
			name: `mobile-workspace-${index + 1}`,
			title: `Mobile workspace ${index + 1}`,
			project_id: `project-mobile-${index + 1}`,
		})
		: options.multipleAgents ? [agent, secondaryAgent] : [agent];
	const listedTasks = options.completedTaskCount
		? Array.from({ length: options.completedTaskCount }, (_, index) => ({
			...task,
			id: `completed-task-${index + 1}`,
			title: `Completed task ${index + 1}`,
			agent_id: options.projectCount
				? availableAgents[index % availableAgents.length].id
				: task.agent_id,
			status: 'completed',
			created_at: timestamp(index),
			updated_at: timestamp(index),
			completed_at: timestamp(index),
		}))
		: options.projectTaskUpdates?.map((second, index) => ({
			...task,
			id: `project-task-${index}`,
			title: `Work updated at ${second}`,
			created_at: timestamp(index),
			updated_at: timestamp(second),
		})) ?? [task];
	const counts = listedTasks.reduce((result, listedTask) => {
		result[listedTask.status as keyof typeof result] += 1;
		return result;
	}, {
		pending: 0,
		in_progress: 0,
		waiting_for_input: 0,
		blocked: 0,
		completed: 0,
		cancelled: 0,
	});
	let project: MockProject = {
		id: projectId,
		name: 'Browser collaboration project',
		description: 'A project with conversations, Agents, and tasks.',
		icon: null,
		created_at: timestamp(0),
		updated_at: timestamp(61),
		agent_ids: availableAgents.map((availableAgent) => availableAgent.id),
		conversation_count: options.conversations?.length ?? 0,
		task_count: listedTasks.length,
		deletion_started_at: options.projectDeletionStartedAt ?? null,
		deletion_counts: options.projectDeletionCounts ?? {
			agents: availableAgents.length,
			tasks: listedTasks.length,
			task_messages: options.taskMessages?.length ?? 0,
			conversations: options.conversations?.length ?? 0,
			conversation_messages: options.conversationMessages?.length ?? 0,
			memory_notes: 0,
			workflow_runs: 0,
			schedules: 0,
		},
	};
	if (options.sharedProjectState?.project) {
		project = { ...project, ...options.sharedProjectState.project };
	} else if (options.sharedProjectState) {
		options.sharedProjectState.project = { ...project };
	}
	let availableProjects = options.projectCount
		? availableAgents.map((availableAgent, index) => ({
			...project,
			id: index === 0 ? project.id : availableAgent.project_id,
			name: index === 0 ? project.name : options.secondaryProjectName ?? `Mobile project ${index + 1}`,
			description: index === 0
				? project.description
				: `Project ${index + 1} verifies the mobile hierarchy can scroll.`,
			updated_at: index === 0 ? project.updated_at : options.secondaryProjectUpdatedAt ?? project.updated_at,
			agent_ids: [availableAgent.id],
			task_count: listedTasks.filter((listedTask) => listedTask.agent_id === availableAgent.id).length,
		}))
		: [project];
	if (options.projectListTargetLast && availableProjects.length > 1) {
		availableProjects = [...availableProjects.slice(1), availableProjects[0]];
	}
	let conversationMessages = [...(options.conversationMessages ?? [])];

	if (!options.preserveWorkspace) {
		await page.addInitScript(() => localStorage.removeItem('xpressclaw.workspace.v1'));
	}
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		let response: unknown;

		if (path.includes('/visualizations/')) {
			options.visualizationRequests?.push({ path, token: request.headers()['x-xpressclaw-artifact-token'] });
			const document = options.visualizationDocuments?.[path];
			if (!document) {
				await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'Visualization not found' }) });
				return;
			}
			await route.fulfill({
				status: 200,
				contentType: 'text/html; charset=utf-8',
				headers: { 'Referrer-Policy': 'no-referrer', 'X-Content-Type-Options': 'nosniff' },
				body: document,
			});
			return;
		} else if (path === '/api/health') {
			if (options.connection?.online === false) {
				await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ status: 'unavailable' }) });
				return;
			}
			response = { status: 'ok', version: '0.2.0', build: 'dev', git_hash: 'test' };
		} else if (path === '/api/setup/check-docker') {
			response = { available: true, installed: true, can_start: false };
		} else if (path === '/api/setup/status') {
			response = { setup_complete: true };
		} else if (path === '/api/setup/mcp-servers') {
			response = { servers: options.mcpServers ?? [] };
		} else if (path === '/api/setup/agent-catalog') {
			response = { agents: options.agentCatalog ?? [
				{
					kind: 'codex', name: 'Codex', mark: 'C', description: 'Codex over ACP.',
					command: ['codex-acp'], login_command: 'codex login', install_url: 'https://developers.openai.com/codex/cli/',
					image: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest',
					installed: true, configured: true, status: 'ready', executable: 'codex',
				},
				{
					kind: 'deepseek-harness', name: 'DeepSeek Harness', mark: 'DS', description: "DeepSeek Harness through openma-ai's maintained ACP adapter.",
					command: ['dsh-acp'], login_command: 'dsh-acp login', install_url: 'https://github.com/openma-ai/deepseek-harness-acp',
					image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness-docker:latest',
					installed: true, configured: true, status: 'ready', executable: 'dsh-acp',
				},
			] };
		} else if (path === '/api/open-url') {
			const payload = request.postDataJSON() as { url: string };
			options.openUrlRequests?.push(payload.url);
			response = { success: true };
		} else if (/^\/api\/setup\/mcp-servers\/[^/]+\/verify$/.test(path)) {
			const name = decodeURIComponent(path.split('/')[4]);
			const payload = request.postDataJSON() as { agent_id: string | null };
			options.mcpVerificationRequests?.push({ name, agent_id: payload.agent_id });
			response = options.mcpVerificationResults?.[name] ?? {
				ok: true,
				status: 'ready',
				message: 'The MCP endpoint accepted a protocol verification request.',
				suggestion: null,
			};
		} else if (path === '/api/settings/sync') {
			const sharedProject = options.sharedProjectState?.project;
			response = {
				projects: (options.projectSyncStatuses ?? [])
					.filter((syncProject) => syncProject.project_id !== projectId || !options.sharedProjectState?.deleted)
					.map((syncProject) => syncProject.project_id === projectId && sharedProject
						? { ...syncProject, project_name: sharedProject.name, project_icon: sharedProject.icon }
						: syncProject),
			};
		} else if (/^\/api\/settings\/sync\/[^/]+\/(fetch|publish)$/.test(path)) {
			const parts = path.split('/');
			const projectId = decodeURIComponent(parts[4]);
			const operation = parts[5] as 'fetch' | 'publish';
			const payload = (request.postDataJSON() ?? {}) as { force?: boolean };
			const force = operation === 'fetch' && payload.force === true;
			options.projectSyncRequests?.push({ projectId, operation, force });
			if (operation === 'fetch' && options.projectSyncFetchConflictOnce && !force && !projectSyncConflictReturned) {
				projectSyncConflictReturned = true;
				await route.fulfill({
					status: 409,
					contentType: 'application/json',
					body: JSON.stringify({ error: 'project synchronization error: this is the first fetch for a populated local Project; rerun with --force to acknowledge a non-destructive merge' }),
				});
				return;
			}
			response = {
				action: operation,
				project_id: projectId,
				commit: operation === 'fetch' ? '12345678fetch' : '87654321publish',
				counts: { agents: 2, tasks: 8, task_messages: 13, conversations: 3, conversation_messages: 21, workflows: 1, memory_notes: 5 },
			};
		} else if (path === '/api/settings/collaboration') {
			response = options.collaborationSettings ?? {
				config: {
					enabled: false, bind_address: '127.0.0.1', gitbucket_port: 8088, jenkins_port: 8089,
					gitbucket_image: 'ghcr.io/gitbucket/gitbucket:4.46.1', jenkins_image: 'jenkins/jenkins:2.568.1-jdk21',
					authorized_agents: [],
				},
				status: {
					configured: false, docker_available: true, network: 'xpressclaw-collaboration-test-network', data_path: '/data/collaboration',
					gitbucket: { state: 'not_installed', health: 'unknown', image: 'ghcr.io/gitbucket/gitbucket:4.46.1', version: '4.46.1', host_url: 'http://127.0.0.1:8088', internal_url: 'http://gitbucket:8080', volume: 'gitbucket-data', error: null },
					jenkins: { state: 'not_installed', health: 'unknown', image: 'jenkins/jenkins:2.568.1-jdk21', version: '2.568.1-jdk21', host_url: 'http://127.0.0.1:8089', internal_url: 'http://jenkins:8080', volume: 'jenkins-data', error: null },
				},
				credentials_configured: false, reset_confirmation: 'RESET LOCAL COLLABORATION',
			};
			if (request.method() === 'PUT') {
				options.collaborationActions?.push('save');
				response = { ...response, config: request.postDataJSON() };
				options.collaborationSettings = response;
			}
		} else if (/^\/api\/settings\/collaboration\/(install|start|stop|restart|upgrade|reset)$/.test(path)) {
			options.collaborationActions?.push(path.split('/').at(-1) ?? '');
			response = options.collaborationSettings;
		} else if (path === '/api/projects') {
			const sharedProject = options.sharedProjectState?.project;
			const projectListSnapshot = availableProjects
				.filter((availableProject) => availableProject.id !== projectId || !options.sharedProjectState?.deleted)
				.map((availableProject) => ({
					...availableProject,
					...(availableProject.id === projectId && sharedProject ? sharedProject : {}),
					agent_ids: [...availableProject.agent_ids],
				}));
			options.projectListRequests?.push(path);
			await options.projectListGate;
			response = projectListSnapshot;
		} else if (path === `/api/projects/${projectId}`) {
			if (request.method() === 'PATCH') {
				const data = request.postDataJSON() as Record<string, unknown>;
				options.projectUpdateRequests?.push({ projectId, data });
				project = {
					...project,
					name: typeof data.name === 'string' ? data.name : project.name,
					description: typeof data.description === 'string' ? data.description : project.description,
					updated_at: timestamp(120),
				};
				if (options.sharedProjectState) {
					options.sharedProjectState.project = { ...project };
					options.sharedProjectState.deleted = false;
				}
				availableProjects = availableProjects.map((availableProject) =>
					availableProject.id === projectId ? { ...availableProject, ...project } : availableProject
				);
				response = project;
			} else if (request.method() === 'DELETE') {
				options.projectDeleteRequests?.push(projectId);
				options.projectDeleteAcknowledgements?.push(url.searchParams.get('cascade'));
				await options.projectDeleteGate;
				if (options.projectDeleteError) {
					await route.fulfill({
						status: 409,
						contentType: 'application/json',
						body: JSON.stringify({ error: options.projectDeleteError }),
					});
					return;
				}
				availableProjects = availableProjects.filter((availableProject) => availableProject.id !== projectId);
				if (options.sharedProjectState) options.sharedProjectState.deleted = true;
				await route.fulfill({ status: 204, body: '' });
				return;
			} else {
				const projectWasDeleted = options.sharedProjectState?.deleted === true;
				const projectSnapshot = {
					...project,
					...(options.sharedProjectState?.project ?? {}),
				};
				options.projectGetRequests?.push(projectId);
				await options.projectGetGate;
				if (projectWasDeleted) {
					await route.fulfill({
						status: 404,
						contentType: 'application/json',
						body: JSON.stringify({ error: 'Project not found' }),
					});
					return;
				}
				response = projectSnapshot;
			}
		} else if (path === `/api/projects/${projectId}/tasks`) {
			response = listedTasks;
		} else if (path === '/api/conversations') {
			response = options.conversations ?? [];
		} else if (path === `/api/conversations/${conversationId}`) {
			response = options.conversations?.find((conversation) => conversation.id === conversationId) ?? { error: 'Unknown conversation' };
		} else if (path === `/api/conversations/${conversationId}/messages`) {
			if (request.method() === 'POST') {
				const payload = request.postDataJSON() as Record<string, unknown>;
				options.conversationMessageRequests?.push(payload);
				const messageId = conversationMessages.length + 100;
				const uploads = Array.isArray(payload.attachments)
					? payload.attachments as { name: string; mime_type: string; data: string }[]
					: [];
				const sent = {
					id: messageId,
					conversation_id: conversationId,
					sender_type: 'user', sender_id: 'local', sender_name: 'You',
					content: payload.content, message_type: 'message', linked_task_id: null,
					metadata: {},
					attachments: uploads.map((attachment, index) => ({
						id: `sent-attachment-${index}`,
						message_id: messageId,
						name: attachment.name,
						mime_type: attachment.mime_type,
						size: Math.floor(attachment.data.length * 3 / 4),
						source_task_id: null,
						created_at: timestamp(200),
					})),
					created_at: timestamp(200),
				};
				conversationMessages.push(sent);
				response = { message: sent, queued_agents: [agentId] };
			} else response = conversationMessages;
		} else if (path === `/api/conversations/${conversationId}/tasks`) {
			if (request.method() === 'POST') {
				const payload = request.postDataJSON() as Record<string, unknown>;
				options.conversationTaskRequests?.push(payload);
				response = payload.workflow_id
					? { workflow_instance_id: 'conversation-workflow-instance' }
					: { ...task, id: 'conversation-task-test', title: payload.title, description: payload.description ?? null, agent_id: payload.agent_id ?? null, conversation_id: conversationId };
			} else response = listedTasks.filter((listedTask) => listedTask.conversation_id === conversationId);
		} else if (path === `/api/conversations/${conversationId}/turns`) {
			response = options.conversationTurns ?? [];
		} else if (path === `/api/conversations/${conversationId}/events`) {
			await route.fulfill({ status: 200, contentType: 'text/event-stream', body: ': connected\n\n' });
			return;
		} else if (path === `/api/conversations/${conversationId}/participants`) {
			response = {};
		} else if (path.startsWith(`/api/conversations/${conversationId}/participants/`)) {
			response = {};
		} else if (path === '/api/agents') {
			response = availableAgents;
		} else if (path === `/api/agents/${agentId}`) {
			response = options.agentKind ? {
				...agent,
				backend: options.agentKind.value,
				config: {
					...agent.config,
					runner: { ...agent.config.runner, kind: options.agentKind.value },
				},
			} : agent;
		} else if (path === '/api/workflows') {
			if (request.method() === 'POST') {
				const payload = request.postDataJSON() as { name: string; description?: string; yaml_content: string };
				options.workflowCreateRequests?.push(payload);
				createdWorkflow = {
					id: 'workflow-created',
					...payload,
					enabled: true,
					version: 1,
					created_at: timestamp(100),
					updated_at: timestamp(100),
				};
				response = createdWorkflow;
			} else {
				response = options.workflows ?? [];
			}
		} else if (/^\/api\/workflows\/instances\/[^/]+$/.test(path)) {
			response = {
				instance: {
					id: path.split('/')[4], workflow_id: 'workflow-review-loop', status: 'running',
					current_flow: 'main', current_step_index: 0, trigger_data: '{}', variable_store: '{}',
					loop_state: null, started_at: timestamp(200), completed_at: null, error_message: null,
				},
				step_executions: [{
					id: 'conversation-workflow-step', instance_id: path.split('/')[4], flow_name: 'main',
					step_id: 'implement', task_id: taskId, status: 'running', input_context: null,
					output: null, attempt: 1, started_at: timestamp(200), completed_at: null,
				}],
			};
		} else if (/^\/api\/workflows\/[^/]+\/instances$/.test(path)) {
			response = [];
		} else if (/^\/api\/workflows\/[^/]+\/run$/.test(path)) {
			const id = path.split('/')[3];
			const inputs = request.postDataJSON() as Record<string, unknown>;
			const runRequest: { id: string; inputs: Record<string, unknown>; projectId?: string } = { id, inputs };
			if (url.searchParams.has('project_id')) runRequest.projectId = url.searchParams.get('project_id') ?? undefined;
			options.workflowRunRequests?.push(runRequest);
			response = {
				id: 'workflow-instance', workflow_id: id, status: 'running', current_flow: 'main', current_step_index: 0,
				current_task_id: taskId,
				trigger_data: JSON.stringify(inputs), variable_store: '{}', loop_state: null,
				started_at: timestamp(200), completed_at: null, error_message: null,
			};
		} else if (/^\/api\/workflows\/[^/]+$/.test(path)) {
			const id = path.split('/')[3];
			response = createdWorkflow ?? options.workflows?.find((workflow) => workflow.id === id) ?? {
				error: `Unknown workflow: ${id}`,
			};
		} else if (path === '/api/schedules') {
			response = options.schedules ?? [];
		} else if (path === '/api/setup/config') {
			const configuredKind = options.agentKind?.value ?? agentRunnerKind;
			response = {
				llm: { providers: [] },
				agents: [{
					name: agent.name,
					title: agent.title,
					backend: configuredKind,
					model: null,
					runner: {
						kind: configuredKind,
						image: `xpressclaw-runner-${configuredKind}:latest`,
						workspace: '/workspace',
						model: null,
						session_config: {},
						mcp_servers: [],
						environment: {},
						command: [],
						subscription_auth: true,
						ssh_agent_forwarding: false,
						container_engine: 'none',
					},
					tools: [],
					skills: [],
					volumes: [],
				}],
				system: { budget: { daily: '0', monthly: null, on_exceeded: 'warn' } },
				mcp_servers: [],
			};
		} else if (path === `/api/workspaces/${agentId}`) {
			response = {
				agent_id: agentId,
				root: (repositoryStatus.active as { root?: string } | null)?.root ?? '/srv/workspaces/browser-tested',
				repository: repositoryStatus,
				container_exists: true,
				container_running: true,
				terminal_available: true,
			};
		} else if (path === `/api/workspaces/${agentId}/repository`) {
			if (request.method() === 'PUT') {
				const payload = request.postDataJSON() as { path: string };
				options.repositorySelections?.push(payload.path);
				const selected = (repositoryStatus.candidates as Record<string, unknown>[]).find((candidate) => candidate.relative_path === payload.path) ?? null;
				repositoryStatus = {
					...repositoryStatus,
					state: selected ? 'pending' : 'missing',
					pending_relative_path: selected ? payload.path : null,
					pending_action: selected ? 'manual' : null,
					restart_required: Boolean(selected),
					message: selected ? 'The repository change is pending and will be applied at the next safe turn boundary.' : 'The selected repository is missing.',
				};
			} else if (request.method() === 'DELETE') {
				options.repositorySelections?.push(null);
				repositoryStatus = {
					...repositoryStatus,
					state: 'pending',
					pending_relative_path: null,
					pending_action: 'cleared',
					restart_required: true,
					message: 'The repository change is pending and will be applied at the next safe turn boundary.',
				};
			}
			response = repositoryStatus;
		} else if (path === `/api/workspaces/${agentId}/tree`) {
			const directory = url.searchParams.get('path') ?? '';
			response = directory === 'src'
				? {
					path: 'src',
					entries: [{ name: 'main.ts', path: 'src/main.ts', kind: 'file', size: workspaceFileContent.length }],
					truncated: false,
				}
				: {
					path: '',
					entries: [
						{ name: 'src', path: 'src', kind: 'directory', size: null },
						{ name: 'README.md', path: 'README.md', kind: 'file', size: 32 },
					],
					truncated: false,
				};
		} else if (path === `/api/workspaces/${agentId}/file`) {
			if (request.method() === 'PUT') {
				const payload = request.postDataJSON() as { path: string; content: string; expected_revision: string };
				options.workspaceSaveRequests?.push(payload);
				if (options.workspaceSaveDelayMs) {
					await new Promise((resolve) => setTimeout(resolve, options.workspaceSaveDelayMs));
				}
				workspaceFileContent = payload.content;
				workspaceFileRevision = 'revision-after-save';
				response = { path: payload.path, revision: workspaceFileRevision, size: payload.content.length };
			} else {
				const filePath = url.searchParams.get('path') ?? 'src/main.ts';
				options.workspaceReadRequests?.push(filePath);
				const readDelayMs = options.workspaceReadDelaysMs?.[filePath] ?? 0;
				if (readDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, readDelayMs));
				if (options.includeDeletedWorkspaceFile && filePath === 'src/removed.ts') {
					await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'workspace path was not found' }) });
					return;
				}
				response = {
					path: filePath,
					content: filePath === 'src/main.ts' ? workspaceFileContent : '# Browser-tested workspace\n',
					revision: workspaceFileRevision,
					size: workspaceFileContent.length,
				};
			}
		} else if (path === `/api/workspaces/${agentId}/git/status`) {
			response = {
				repository: true,
				branch: 'feature/workspace-browser',
				repository_status: repositoryStatus,
				files: [
					{ path: 'src/main.ts', original_path: null, status: ' M', index_status: ' ', worktree_status: 'M' },
					{ path: 'README.md', original_path: null, status: '??', index_status: '?', worktree_status: '?' },
					...(options.includeDeletedWorkspaceFile
						? [{ path: 'src/removed.ts', original_path: null, status: ' D', index_status: ' ', worktree_status: 'D' }]
						: []),
				],
			};
		} else if (path === `/api/workspaces/${agentId}/git/diff`) {
			const filePath = url.searchParams.get('path') ?? 'src/main.ts';
			response = {
				path: filePath,
				diff: filePath === 'src/main.ts'
					? 'diff --git a/src/main.ts b/src/main.ts\n--- a/src/main.ts\n+++ b/src/main.ts\n@@ -1 +1 @@\n-export const greeting = "hi";\n+export const greeting = "hello";\n'
					: filePath === 'src/removed.ts'
						? 'diff --git a/src/removed.ts b/src/removed.ts\ndeleted file mode 100644\n--- a/src/removed.ts\n+++ /dev/null\n@@ -1 +0,0 @@\n-export const removed = true;\n'
						: '',
				truncated: false,
			};
		} else if (path === '/api/tasks') {
			if (url.searchParams.has('parent_task_id')) {
				response = { tasks: options.taskSubtasks ?? [], counts: { ...counts, completed: 0 } };
			} else {
				const searchTerms = (url.searchParams.get('search') ?? '')
					.toLocaleLowerCase()
					.split(/\s+/)
					.filter(Boolean);
				const searchedTasks = listedTasks.filter((listedTask) => {
					const text = `${listedTask.title}\n${listedTask.description ?? ''}`.toLocaleLowerCase();
					return searchTerms.every((term) => text.includes(term));
				});
				const includedStatuses = new Set((url.searchParams.get('statuses') ?? '').split(',').filter(Boolean));
				const excludedStatuses = new Set((url.searchParams.get('exclude_statuses') ?? '').split(',').filter(Boolean));
				const limit = Number.parseInt(url.searchParams.get('limit') ?? '100', 10);
				const offset = Number.parseInt(url.searchParams.get('offset') ?? '0', 10);
				const filteredTasks = [...searchedTasks]
					.filter((listedTask) => includedStatuses.size === 0 || includedStatuses.has(listedTask.status))
					.filter((listedTask) => !excludedStatuses.has(listedTask.status));
				const filteredCounts = searchedTasks.reduce((result, listedTask) => {
					result[listedTask.status as keyof typeof result] += 1;
					return result;
				}, {
					pending: 0,
					in_progress: 0,
					waiting_for_input: 0,
					blocked: 0,
					completed: 0,
					cancelled: 0,
				});
				if (url.searchParams.get('sort') === 'recent') {
					filteredTasks.sort((left, right) =>
						Date.parse(right.updated_at) - Date.parse(left.updated_at)
						|| Date.parse(right.created_at) - Date.parse(left.created_at)
						|| right.id.localeCompare(left.id)
					);
				}
				response = {
					tasks: filteredTasks.slice(offset, offset + limit),
					counts: filteredCounts,
				};
			}
		} else if (path === '/api/tasks/recent-by-agent') {
			const limit = Number.parseInt(url.searchParams.get('limit') ?? '5', 10);
			const countsByAgent = new Map<string, number>();
			response = {
				tasks: [...listedTasks]
					.sort((left, right) =>
						Date.parse(right.updated_at) - Date.parse(left.updated_at)
						|| Date.parse(right.created_at) - Date.parse(left.created_at)
						|| right.id.localeCompare(left.id)
					)
					.filter((listedTask) => {
						const key = listedTask.agent_id ?? 'unassigned';
						const count = countsByAgent.get(key) ?? 0;
						if (count >= limit) return false;
						countsByAgent.set(key, count + 1);
						return true;
					}),
			};
		} else if (path === `/api/tasks/${taskId}`) {
			if (options.taskLoadFailureOnce && !taskLoadFailed) {
				taskLoadFailed = true;
				await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Temporary task load failure' }) });
				return;
			}
			response = task;
		} else if (path === `/api/tasks/${taskId}/messages`) {
			if (request.method() === 'POST') {
				const payload = request.postDataJSON() as Record<string, unknown>;
				options.postedMessages?.push(payload);
				response = {
					message: { id: 4, task_id: taskId, role: 'user', content: payload.content, attachments: [], timestamp: timestamp(62) },
					continuation_queued: true,
					attempt_id: 'attempt-image-message',
					delivery: payload.delivery === 'immediate' ? 'immediate' : (options.live ? 'after_tool' : 'queued'),
				};
			} else {
				response = options.taskMessages ?? [
					{ id: 1, task_id: taskId, role: 'assistant', content: firstAnswer, attachments: [], timestamp: timestamp(25) },
					{ id: 2, task_id: taskId, role: 'user', content: userFollowUp, attachments: [], timestamp: timestamp(40) },
					{ id: 3, task_id: taskId, role: 'assistant', content: secondAnswer, attachments: [], timestamp: timestamp(55) },
				];
			}
		} else if (path.startsWith(`/api/tasks/${taskId}/elicitations/`) && path.endsWith('/response')) {
			const elicitationId = decodeURIComponent(path.split('/')[5]);
			const payload = request.postDataJSON() as Record<string, unknown>;
			options.elicitationResponses?.push({ elicitationId, payload });
			response = { resolved: true, action: payload.action };
		} else if (path === `/api/tasks/${taskId}/activity`) {
			if (options.agentTimeline) {
				response = {
					attempts: options.taskAttempts ?? [mockAttempt()],
					events: [
						timelineEvent(1, 10, 'runner_progress', firstAgentUpdate, { item_type: 'agent_message', message_id: 'status-1' }),
						timelineEvent(2, 15, 'tool_call', 'Read the project', { toolCallId: 'tool-1', status: 'in_progress' }),
						timelineEvent(3, 20, 'runner_progress', 'Tests are running.\n\nI’m checking the timeline in both light and dark themes before I wrap up.', { item_type: 'agent_message', message_id: 'status-2' }),
						timelineEvent(4, 24, 'runner_progress', firstAnswer, { item_type: 'agent_message', message_id: 'final-1' }),
					],
					has_more_before: false,
					has_more_after: false,
				};
			} else if (url.searchParams.has('before')) {
				response = {
					attempts: options.taskAttempts ?? [mockAttempt()],
					events: Array.from({ length: 20 }, (_, index) => activityEvent(index + 1)),
					has_more_before: false,
					has_more_after: true,
				};
			} else if (url.searchParams.has('after')) {
				liveEvent += 1;
				contextUsed += 1_000;
				response = {
					attempts: options.taskAttempts ?? [mockAttempt()],
					events: options.live ? [activityEvent(60 + liveEvent, 'New background activity')] : [],
					has_more_before: false,
					has_more_after: false,
				};
			} else {
				response = {
					attempts: options.taskAttempts ?? [mockAttempt()],
					events: options.taskActivityEvents ?? (options.pendingElicitation ? [
						timelineEvent(42, 58, 'elicitation_pending', 'The agent needs your input', {
							elicitationId: 'elicitation-browser-test',
							status: 'pending',
							...options.pendingElicitation,
						}),
					] : options.richToolActivity ? [
						{
							...activityEvent(21),
							event_type: 'usage',
							summary: 'Updated context usage',
							payload: { sessionUpdate: 'usage_update', used: 128_000, size: 256_000 },
						},
						{
							...activityEvent(22),
							event_type: 'tool_call',
							summary: 'Tool call',
							payload: {
								sessionUpdate: 'tool_call',
								toolCallId: 'edit-browser-test',
								title: 'Tool call',
								kind: 'edit',
								status: 'in_progress',
								content: [{
									type: 'diff',
									path: '/workspace/src/example.ts',
									oldText: 'const state = "before";\n',
									newText: 'const state = "after";\n',
								}],
							},
						},
						{
							...activityEvent(23),
							event_type: 'tool_call_update',
							summary: 'Completed Editing files',
							payload: {
								sessionUpdate: 'tool_call_update',
								toolCallId: 'edit-browser-test',
								status: 'completed',
								rawOutput: { formatted_output: 'Applied patch.' },
							},
						},
					] : Array.from({ length: 40 }, (_, index) => activityEvent(index + 21))),
					has_more_before: options.taskActivityEvents ? false : !options.richToolActivity,
					has_more_after: false,
				};
			}
		} else if (path === `/api/sessions/${agentId}`) {
			response = {
				session: { id: agentId, status: options.live ? 'running' : 'idle' },
				active_attempts: [],
				queued_attempts: [],
				recent_attempts: [],
				recent_events: [],
				artifacts: [],
			};
		} else if (path === `/api/sessions/${agentId}/readiness`) {
			response = options.runnerReadiness ?? { ready: true };
		} else if (/^\/api\/sessions\/[^/]+\/messages$/.test(path) && request.method() === 'POST') {
			const targetAgentId = path.split('/')[3];
			const payload = request.postDataJSON() as Record<string, unknown>;
			options.queuedSessionMessages?.push({ agentId: targetAgentId, payload });
			response = {
				event: activityEvent(80),
				task: { ...task, id: taskId, agent_id: targetAgentId },
				attempt_id: 'attempt-new-work-test',
				queued: true,
			};
		} else if (path === `/api/sessions/${agentId}/attempts/attempt-browser-test/interrupt`) {
			options.interruptedAttempts?.push('attempt-browser-test');
			response = { ...attempt('interrupted'), status: 'interrupted', completed_at: timestamp(62) };
		} else if (path === `/api/sessions/${agentId}/events`) {
			response = [
				{
					...activityEvent(70),
					event_type: 'session_config_options',
					payload: {
						config_options: [
							{ id: 'model', name: 'Model', category: 'model', type: 'select', currentValue: 'codex-test', options: [{ value: 'codex-test', name: 'Codex Test' }] },
							{ id: 'reasoning_effort', name: 'Reasoning', category: 'thought_level', type: 'select', currentValue: 'high', options: [{ value: 'high', name: 'High' }] },
						],
					},
				},
				{
					...activityEvent(71),
					event_type: 'available_commands',
					payload: { available_commands: [
						{ name: 'review', description: 'Review the current changes' },
						{ name: 'plan', description: 'Switch to planning mode' },
					] },
				},
			];
		} else {
			await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: `Unmocked API route: ${path}` }) });
			return;
		}

		await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(response) });
	});
}

test('activity stays chronological, compact, expandable, and pageable', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	await expect(transcript).toBeVisible();
	const timestamps = await transcript.locator('[data-transcript-timestamp]').evaluateAll((elements) =>
		elements.map((element) => Date.parse(element.getAttribute('data-transcript-timestamp') ?? '')),
	);
	expect(timestamps.every((value, index) => index === 0 || value >= timestamps[index - 1])).toBe(true);

	const currentAnchor = page.getByRole('button', { name: /Current activity 21/ });
	const loadEarlier = page.getByRole('button', { name: 'Load earlier activity' });
	await loadEarlier.scrollIntoViewIfNeeded();
	const anchorBefore = await currentAnchor.boundingBox();
	expect(anchorBefore).not.toBeNull();
	await loadEarlier.click();
	await expect(page.getByText('Earlier activity 1', { exact: true })).toBeAttached();
	await expect(page.getByRole('button', { name: 'Load earlier activity' })).toHaveCount(0);
	await page.waitForTimeout(50);
	const anchorAfter = await currentAnchor.boundingBox();
	expect(anchorAfter).not.toBeNull();
	expect(Math.abs(anchorAfter!.y - anchorBefore!.y)).toBeLessThanOrEqual(2);

	await currentAnchor.click();
	await expect(page.getByText('"command": "echo 21"')).toBeVisible();
	await currentAnchor.click();
	await expect(page.getByText('"command": "echo 21"')).toHaveCount(0);

	const composer = page.locator(`#task-message-input-${taskId}`);
	await composer.fill('/');
	await expect(page.getByRole('button', { name: /review.*Review the current changes/i })).toBeVisible();
	await expect(page.getByRole('button', { name: /plan.*Switch to planning mode/i })).toBeVisible();
	await expect(page.getByRole('button', { name: /commands/i })).toHaveCount(0);

	await page.getByTitle('Model and reasoning effort').click();
	await expect(page.getByText('Reasoning effort', { exact: true })).toBeVisible();
});

test('attempt errors can be dismissed', async ({ page }) => {
	const rawError = '<img src=x onerror="window.__rawHtmlExecuted = true"> ACP process stopped during the turn';
	await mockApi(page, { attemptError: rawError });
	await page.goto(`/tasks/${taskId}`);

	const notification = page.locator('[data-attempt-error]');
	await expect(notification).toBeVisible();
	await expect(notification).toContainText(rawError);
	await expect(notification.locator('img')).toHaveCount(0);
	const dismiss = notification.getByRole('button', { name: 'Dismiss attempt error' });
	await dismiss.focus();
	await dismiss.press('Enter');
	await expect(notification).toHaveCount(0);
	await expect(page.locator('[data-task-transcript-scroll]')).toBeFocused();
});

test('failed initial task loads can be retried', async ({ page }) => {
	await mockApi(page, { taskLoadFailureOnce: true });
	await page.goto(`/tasks/${taskId}`);

	const alert = page.getByRole('alert');
	await expect(alert).toContainText('Temporary task load failure');
	await alert.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByRole('heading', { name: 'Browser-tested workspace' })).toBeVisible();
	await expect(alert).toHaveCount(0);
});

test('dismissing an initial task load failure returns to the task list', async ({ page }) => {
	await mockApi(page, { taskLoadFailureOnce: true });
	await page.goto(`/tasks/${taskId}`);

	await page.getByRole('button', { name: 'Dismiss task error and return to tasks' }).click();
	await expect(page).toHaveURL('/tasks');
});

test('agent updates stay beside their tools while the final reply is shown once', async ({ page }) => {
	await mockApi(page, { agentTimeline: true });
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	await expect(transcript).toBeVisible();
	const updates = transcript.locator('[data-agent-update]');
	await expect(updates).toHaveCount(2);
	const firstUpdate = updates.first();
	await expect(firstUpdate).toHaveAttribute('aria-label', 'Agent update');
	await expect(firstUpdate.getByText('agent', { exact: true })).toBeVisible();
	await expect(firstUpdate.getByText('Update', { exact: true })).toBeVisible();
	await expect(firstUpdate.getByText("Yes, I'll inspect the project.", { exact: true })).toBeVisible();
	await expect(firstUpdate.getByText("I'm starting with the timeline component so this update remains readable even when it spans multiple lines.", { exact: true })).toBeVisible();
	await expect(firstUpdate.locator('button')).toHaveCount(0);
	await expect(page.getByRole('button', { name: /Read the project/ })).toBeVisible();

	const updateContent = firstUpdate.locator('[data-agent-update-content]');
	const updateTextStyle = await updateContent.evaluate((element) => {
		const style = getComputedStyle(element);
		return { overflow: style.overflow, textOverflow: style.textOverflow, whiteSpace: style.whiteSpace };
	});
	expect(updateTextStyle.overflow).not.toBe('hidden');
	expect(updateTextStyle.textOverflow).not.toBe('ellipsis');
	expect(updateTextStyle.whiteSpace).not.toBe('nowrap');

	const finalMessage = transcript.locator('[data-message-role="assistant"]').first();
	const [updateBackground, finalBackground] = await Promise.all([
		updateContent.evaluate((element) => getComputedStyle(element).backgroundColor),
		finalMessage.locator('.rounded-lg').evaluate((element) => getComputedStyle(element).backgroundColor),
	]);
	expect(updateBackground).not.toBe(finalBackground);

	const entries = await transcript.locator('[data-transcript-kind]').allTextContents();
	const statusIndex = entries.findIndex((entry) => entry.includes("Yes, I'll inspect the project."));
	const toolIndex = entries.findIndex((entry) => entry.includes('Read the project'));
	const testIndex = entries.findIndex((entry) => entry.includes('Tests are running.'));
	const finalIndexes = entries
		.map((entry, index) => entry.includes('First answer') ? index : -1)
		.filter((index) => index >= 0);

	expect(statusIndex).toBeGreaterThanOrEqual(0);
	expect(statusIndex).toBeLessThan(toolIndex);
	expect(toolIndex).toBeLessThan(testIndex);
	expect(finalIndexes).toHaveLength(1);
	expect(testIndex).toBeLessThan(finalIndexes[0]);
});

test('near-simultaneous sentence fragments render as one agent update', async ({ page }) => {
	await mockApi(page, {
		taskMessages: [],
		taskActivityEvents: [
			timelineEvent(1, 10, 'runner_progress', 'The audit', { item_type: 'agent_message', message_id: 'fragment-1' }),
			timelineEvent(2, 10, 'runner_progress', '-retention regression is now exercising the actual migration DDL', { item_type: 'agent_message', message_id: 'fragment-2' }),
			timelineEvent(3, 11, 'runner_progress', 'against H2: it inserts an audit row', { item_type: 'agent_message', message_id: 'fragment-3' }),
			timelineEvent(4, 11, 'runner_progress', ',', { item_type: 'agent_message', message_id: 'fragment-4' }),
			timelineEvent(5, 12, 'runner_progress', 'deletes', { item_type: 'agent_message', message_id: 'fragment-5' }),
			timelineEvent(6, 12, 'runner_progress', 'the parent conversation, and creates a tombstone.', { item_type: 'agent_message', message_id: 'fragment-6' }),
			timelineEvent(7, 12, 'runner_progress', 'Tests passed.', { item_type: 'agent_message', message_id: 'status-1' }),
			timelineEvent(8, 12, 'runner_progress', 'Ready to review.', { item_type: 'agent_message', message_id: 'status-2' }),
			timelineEvent(9, 13, 'runner_progress', 'Done.', { item_type: 'agent_message', message_id: 'status-3' }),
			timelineEvent(10, 13, 'runner_progress', '.env is configured.', { item_type: 'agent_message', message_id: 'status-4' }),
			timelineEvent(11, 14, 'runner_progress', 'Updated config', { item_type: 'agent_message', message_id: 'status-5' }),
			timelineEvent(12, 14, 'runner_progress', '.env is ready.', { item_type: 'agent_message', message_id: 'status-6' }),
			timelineEvent(13, 15, 'runner_progress', 'The exit code is', { item_type: 'agent_message', message_id: 'status-7' }),
			timelineEvent(14, 15, 'runner_progress', '-1 when unavailable.', { item_type: 'agent_message', message_id: 'status-8' }),
			timelineEvent(15, 16, 'runner_progress', 'Then use', { item_type: 'agent_message', message_id: 'status-9' }),
			timelineEvent(16, 16, 'runner_progress', '--force to override.', { item_type: 'agent_message', message_id: 'status-10' }),
			timelineEvent(17, 17, 'runner_progress', 'The audit-', { item_type: 'agent_message', message_id: 'fragment-7' }),
			timelineEvent(18, 17, 'runner_progress', 'retention policy passed.', { item_type: 'agent_message', message_id: 'fragment-8' }),
			timelineEvent(19, 18, 'runner_progress', 'Choose and/', { item_type: 'agent_message', message_id: 'fragment-9' }),
			timelineEvent(20, 18, 'runner_progress', 'or syntax.', { item_type: 'agent_message', message_id: 'fragment-10' }),
			timelineEvent(21, 19, 'runner_progress', 'Call (', { item_type: 'agent_message', message_id: 'fragment-11' }),
			timelineEvent(22, 19, 'runner_progress', 'child) first.', { item_type: 'agent_message', message_id: 'fragment-12' }),
			timelineEvent(23, 20, 'runner_progress', 'Tests passed -', { item_type: 'agent_message', message_id: 'fragment-13' }),
			timelineEvent(24, 20, 'runner_progress', 'ready for review.', { item_type: 'agent_message', message_id: 'fragment-14' }),
			timelineEvent(25, 21, 'runner_progress', 'Use input /', { item_type: 'agent_message', message_id: 'fragment-15' }),
			timelineEvent(26, 21, 'runner_progress', 'output channels.', { item_type: 'agent_message', message_id: 'fragment-16' }),
		],
	});
	await page.goto(`/tasks/${taskId}`);

	const updates = page.locator('[data-task-transcript] [data-agent-update]');
	await expect(updates).toHaveCount(16);
	await expect(updates.nth(0).locator('[data-agent-update-content]')).toHaveText(
		'The audit-retention regression is now exercising the actual migration DDL against H2: it inserts an audit row, deletes the parent conversation, and creates a tombstone.',
	);
	await expect(updates.nth(1).locator('[data-agent-update-content]')).toHaveText('Tests passed.');
	await expect(updates.nth(2).locator('[data-agent-update-content]')).toHaveText('Ready to review.');
	await expect(updates.nth(3).locator('[data-agent-update-content]')).toHaveText('Done.');
	await expect(updates.nth(4).locator('[data-agent-update-content]')).toHaveText('.env is configured.');
	await expect(updates.nth(5).locator('[data-agent-update-content]')).toHaveText('Updated config');
	await expect(updates.nth(6).locator('[data-agent-update-content]')).toHaveText('.env is ready.');
	await expect(updates.nth(7).locator('[data-agent-update-content]')).toHaveText('The exit code is');
	await expect(updates.nth(8).locator('[data-agent-update-content]')).toHaveText('-1 when unavailable.');
	await expect(updates.nth(9).locator('[data-agent-update-content]')).toHaveText('Then use');
	await expect(updates.nth(10).locator('[data-agent-update-content]')).toHaveText('--force to override.');
	await expect(updates.nth(11).locator('[data-agent-update-content]')).toHaveText('The audit-retention policy passed.');
	await expect(updates.nth(12).locator('[data-agent-update-content]')).toHaveText('Choose and/or syntax.');
	await expect(updates.nth(13).locator('[data-agent-update-content]')).toHaveText('Call (child) first.');
	await expect(updates.nth(14).locator('[data-agent-update-content]')).toHaveText('Tests passed - ready for review.');
	await expect(updates.nth(15).locator('[data-agent-update-content]')).toHaveText('Use input / output channels.');
});

test('mirrored final fragments stay hidden without duplicating durable replies', async ({ page }) => {
	await mockApi(page, {
		taskMessages: [{
			id: 1,
			task_id: taskId,
			role: 'assistant',
			content: 'ready for review.',
			attachments: [],
			timestamp: timestamp(11),
		}],
		taskActivityEvents: [
			timelineEvent(1, 10, 'runner_progress', 'Tests passed -', { item_type: 'agent_message', message_id: 'fragment-before-reply' }),
			timelineEvent(2, 11, 'runner_progress', 'ready for review.', { item_type: 'agent_message', message_id: 'mirrored-reply' }),
		],
	});
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	const updates = transcript.locator('[data-agent-update]');
	await expect(updates).toHaveCount(1);
	await expect(updates.locator('[data-agent-update-content]')).toHaveText('Tests passed -');
	await expect(transcript.locator('[data-message-role="assistant"]')).toHaveCount(1);
	await expect(transcript.getByText('ready for review.', { exact: true })).toHaveCount(1);
});

test('mirrored replies remain boundaries between otherwise joinable updates', async ({ page }) => {
	await mockApi(page, {
		taskMessages: [{
			id: 1,
			task_id: taskId,
			role: 'assistant',
			content: 'Done.',
			attachments: [],
			timestamp: timestamp(50),
		}],
		taskActivityEvents: [
			timelineEvent(1, 10, 'runner_progress', 'Database ready', { item_type: 'agent_message', message_id: 'before-mirror' }),
			timelineEvent(2, 10, 'runner_progress', 'Done.', { item_type: 'agent_message', message_id: 'mirrored-boundary' }),
			timelineEvent(3, 11, 'runner_progress', 'checking deployment', { item_type: 'agent_message', message_id: 'after-mirror' }),
		],
	});
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	const updates = transcript.locator('[data-agent-update]');
	await expect(updates).toHaveCount(2);
	await expect(updates).toHaveText([/Database ready/, /checking deployment/]);
	await expect(transcript.getByText('Done.', { exact: true })).toHaveCount(1);
});

test('agent update coalescing does not cross message, attempt, time, or tool boundaries', async ({ page }) => {
	await mockApi(page, {
		taskMessages: [{
			id: 1,
			task_id: taskId,
			role: 'user',
			content: 'Keep the migration compatible.',
			attachments: [],
			timestamp: timestamp(41),
		}],
		taskActivityEvents: [
			timelineEvent(1, 9, 'tool_call', 'Read the project', { toolCallId: 'tool-boundary', status: 'in_progress' }),
			timelineEvent(2, 10, 'runner_progress', 'Reading', { item_type: 'agent_message', message_id: 'before-tool' }),
			timelineEvent(3, 10, 'tool_call_update', 'Completed Read the project', { toolCallId: 'tool-boundary', status: 'completed' }),
			timelineEvent(4, 10, 'runner_progress', 'the migration', { item_type: 'agent_message', message_id: 'after-tool' }),
			timelineEvent(5, 20, 'runner_progress', 'Checking', { item_type: 'agent_message', message_id: 'attempt-a' }),
			{
				...timelineEvent(6, 20, 'runner_progress', 'the workspace', { item_type: 'agent_message', message_id: 'attempt-b' }),
				attempt_id: 'attempt-browser-test-2',
			},
			timelineEvent(7, 30, 'runner_progress', 'Inspecting', { item_type: 'agent_message', message_id: 'early' }),
			timelineEvent(8, 33, 'runner_progress', 'the repository', { item_type: 'agent_message', message_id: 'late' }),
			timelineEvent(9, 40, 'runner_progress', 'Updating', { item_type: 'agent_message', message_id: 'before-message' }),
			timelineEvent(10, 42, 'runner_progress', 'the query', { item_type: 'agent_message', message_id: 'after-message' }),
			{
				...timelineEvent(11, 50, 'runner_progress', 'Working on tests', { item_type: 'agent_message', message_id: 'opencode-1' }),
				source_id: 'opencode',
			},
			{
				...timelineEvent(12, 50, 'runner_progress', 'checking deployment', { item_type: 'agent_message', message_id: 'opencode-2' }),
				source_id: 'opencode',
			},
		],
	});
	await page.goto(`/tasks/${taskId}`);

	const updates = page.locator('[data-task-transcript] [data-agent-update]');
	await expect(updates).toHaveCount(10);
	await expect(updates).toHaveText([
		/Reading/,
		/the migration/,
		/Checking/,
		/the workspace/,
		/Inspecting/,
		/the repository/,
		/Updating/,
		/the query/,
		/Working on tests/,
		/checking deployment/,
	]);
});

test('links in agent responses open in new windows without changing user links', async ({ page }) => {
	await mockApi(page, { agentTimeline: true, agentResponseLinks: true });
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	const responseLinks = transcript.locator('[data-message-role="assistant"] a, [data-agent-update] a');
	await expect(responseLinks).toHaveCount(2);
	for (const link of await responseLinks.all()) {
		await expect(link).toHaveAttribute('target', '_blank');
		await expect(link).toHaveAttribute('rel', 'noopener noreferrer');
	}

	const userLink = transcript.locator('[data-message-role="user"] a', { hasText: 'my reference' });
	await expect(userLink).toBeVisible();
	await expect(userLink).not.toHaveAttribute('target', '_blank');
	await expect(userLink).not.toHaveAttribute('rel', /noopener|noreferrer/);
	await expect(userLink).toHaveCSS('color', 'rgb(255, 255, 255)');

	const rawLinkMessage = transcript.locator('[data-message-role="assistant"]', { hasText: 'a raw agent link' });
	await expect(rawLinkMessage.locator('.prose-chat')).toContainText('<a href="https://example.com/raw-agent-link" target="_self" rel="opener">a raw agent link</a>');
	await expect(rawLinkMessage.locator('a')).toHaveCount(0);
});

test('assistant text selections offer follow-up actions in the task composer', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);
	const composer = page.locator(`#task-message-input-${taskId}`);
	await composer.fill('Keep this draft');

	const assistantMessage = page.locator('[data-message-role="assistant"]').first();
	const messageContent = assistantMessage.locator('.prose-chat');
	await messageContent.evaluate((element) => {
		const selection = window.getSelection();
		const range = document.createRange();
		range.selectNodeContents(element);
		selection?.removeAllRanges();
		selection?.addRange(range);
		element.dispatchEvent(new PointerEvent('pointerup', { bubbles: true }));
	});

	const actions = assistantMessage.locator('[data-selection-actions]');
	await expect(actions).toBeVisible();
	await actions.getByRole('button', { name: 'Improve' }).click();
	await expect(composer).toHaveValue(/Keep this draft\n\nImprove this passage:\n\n> First answer/);
	await expect(composer).toBeFocused();
});

test('active work uses the elapsed agent loading indicator', async ({ page }) => {
	await mockApi(page, {
		live: true,
		taskAttempts: [attempt('running', 128_000, null, null, {
			started_at: recentSqliteTimestamp(60_000),
			response_started_at: recentSqliteTimestamp(3_000),
		})],
	});
	await page.goto(`/tasks/${taskId}`);

	const loadingIndicator = page.locator('[data-agent-loading]', { hasText: 'The agent is responding' });
	await expect(loadingIndicator).toBeVisible();
	await expect(loadingIndicator).toHaveAttribute('data-agent-phase', 'active');
	await expect(loadingIndicator.locator('[aria-hidden="true"]').first().locator('span')).toHaveCount(9);
	await expect.poll(() => elapsedSeconds(loadingIndicator)).toBeGreaterThan(1);
	await expect.poll(() => elapsedSeconds(loadingIndicator)).toBeLessThan(60);
	await expect(loadingIndicator.locator('[data-elapsed-time]')).toHaveAttribute('aria-label', /Elapsed \d{1,2}\.\d+s/);
});

test.describe('current response timing semantics', () => {
	test.use({ timezoneId: 'America/Los_Angeles' });

	test('task guidance keeps the active timer and gets a distinct queue timer', async ({ page }) => {
		await mockApi(page, {
			live: true,
			taskAttempts: [
				attempt('queued', 128_000, null, null, {
					id: 'attempt-follow-up',
					created_at: recentSqliteTimestamp(20_000),
					response_queued_at: recentSqliteTimestamp(2_000),
				}),
				attempt('running', 128_000, null, null, {
					id: 'attempt-current',
					started_at: recentSqliteTimestamp(90_000),
					response_started_at: recentSqliteTimestamp(7_000),
				}),
			],
		});
		await page.goto(`/tasks/${taskId}`);

		const active = page.locator('[data-agent-loading]', { hasText: 'The agent is responding' });
		const queued = page.locator('[data-agent-loading]', { hasText: 'New guidance is queued for the next safe break' });
		await expect(active).toHaveAttribute('data-agent-phase', 'active');
		await expect(queued).toHaveAttribute('data-agent-phase', 'queued');
		await expect.poll(async () => (await elapsedSeconds(active)) - (await elapsedSeconds(queued))).toBeGreaterThan(3);
		await expect.poll(async () => (await elapsedSeconds(active)) - (await elapsedSeconds(queued))).toBeLessThan(8);
	});

	test('qualified API timestamps are not double-zoned', async ({ page }) => {
		await mockApi(page, {
			live: true,
			taskAttempts: [attempt('running', 128_000, null, null, {
				response_started_at: new Date(Date.now() - 3_000).toISOString(),
			})],
		});
		await page.goto(`/tasks/${taskId}`);
		const indicator = page.locator('[data-agent-loading]', { hasText: 'The agent is responding' });
		await expect.poll(() => elapsedSeconds(indicator)).toBeGreaterThan(0.5);
		await expect.poll(() => elapsedSeconds(indicator)).toBeLessThan(60);
	});

	test('a task follow-up uses its new response anchor', async ({ page }) => {
		await mockApi(page, {
			live: true,
			taskAttempts: [
				attempt('running', 128_000, null, null, {
					id: 'attempt-follow-up',
					started_at: recentSqliteTimestamp(600_000),
					response_started_at: recentSqliteTimestamp(2_000),
					trigger_message_id: 42,
				}),
				attempt('completed', 128_000, null, null, {
					id: 'attempt-initial',
					started_at: recentSqliteTimestamp(900_000),
					response_started_at: recentSqliteTimestamp(899_000),
				}),
			],
		});
		await page.goto(`/tasks/${taskId}`);
		const indicator = page.locator('[data-agent-loading]', { hasText: 'The agent is responding' });
		await expect.poll(() => elapsedSeconds(indicator)).toBeGreaterThan(0.5);
		await expect.poll(() => elapsedSeconds(indicator)).toBeLessThan(60);
	});

	test('waiting for input stops the response timer', async ({ page }) => {
		await mockApi(page, {
			taskStatus: 'waiting_for_input',
			taskActivityStatus: 'waiting_for_input',
			taskAttempts: [attempt('waiting_for_input', 128_000, null, null, {
				response_started_at: recentSqliteTimestamp(500_000),
			})],
		});
		await page.goto(`/tasks/${taskId}`);
		await expect(page.getByText('Waiting for your response...')).toBeVisible();
		await expect(page.locator('[data-agent-loading]')).toHaveCount(0);
	});

	test('conversation follow-ups and concurrent agents have independent response timers', async ({ page }) => {
		const conversation = {
			id: conversationId,
			project_id: projectId,
			title: 'Timed collaboration',
			icon: null,
			created_at: timestamp(1),
			updated_at: timestamp(20),
			last_message_at: timestamp(20),
			participants: [
				{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
				{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
				{ participant_type: 'agent', participant_id: 'project-secondary-test', joined_at: timestamp(2) },
			],
		};
		await mockApi(page, {
			multipleAgents: true,
			conversations: [conversation],
			conversationTurns: [
				{
					id: 'turn-current', conversation_id: conversationId, agent_id: agentId,
					trigger_message_id: 20, status: 'running', result_message_id: null,
					error_message: null, context_used: null, context_size: null,
					queued_at: recentSqliteTimestamp(90_000), started_at: recentSqliteTimestamp(60_000),
					completed_at: null, response_queued_at: recentSqliteTimestamp(8_000),
					response_started_at: recentSqliteTimestamp(7_000),
				},
				{
					id: 'turn-secondary', conversation_id: conversationId, agent_id: 'project-secondary-test',
					trigger_message_id: 20, status: 'running', result_message_id: null,
					error_message: null, context_used: null, context_size: null,
					queued_at: recentSqliteTimestamp(90_000), started_at: recentSqliteTimestamp(50_000),
					completed_at: null, response_queued_at: recentSqliteTimestamp(4_000),
					response_started_at: recentSqliteTimestamp(2_000),
				},
			],
		});
		await page.goto(`/conversations/${conversationId}`);

		const primary = page.locator('[data-agent-loading]', { hasText: 'Browser-tested workspace is responding' });
		const secondary = page.locator('[data-agent-loading]', { hasText: 'Secondary browser workspace is responding' });
		await expect.poll(async () => (await elapsedSeconds(primary)) - (await elapsedSeconds(secondary))).toBeGreaterThan(3);
		await expect.poll(async () => (await elapsedSeconds(primary)) - (await elapsedSeconds(secondary))).toBeLessThan(8);
		await expect(page.locator('[data-agent-loading]')).toHaveCount(2);
	});

	test('conversation queue and preparation durations are labelled without claiming active thinking', async ({ page }) => {
		const conversation = {
			id: conversationId, project_id: projectId, title: 'Queued collaboration', icon: null,
			created_at: timestamp(1), updated_at: timestamp(20), last_message_at: timestamp(20),
			participants: [
				{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
				{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
				{ participant_type: 'agent', participant_id: 'project-secondary-test', joined_at: timestamp(2) },
			],
		};
		await mockApi(page, {
			multipleAgents: true,
			conversations: [conversation],
			conversationTurns: [
				{
					id: 'turn-queued', conversation_id: conversationId, agent_id: agentId,
					status: 'queued', error_message: null, context_used: null, context_size: null,
					queued_at: recentSqliteTimestamp(80_000), started_at: null, completed_at: null,
					response_queued_at: recentSqliteTimestamp(3_000), response_started_at: null,
				},
				{
					id: 'turn-preparing', conversation_id: conversationId, agent_id: 'project-secondary-test',
					status: 'running', error_message: null, context_used: null, context_size: null,
					queued_at: recentSqliteTimestamp(80_000), started_at: recentSqliteTimestamp(5_000), completed_at: null,
					response_queued_at: recentSqliteTimestamp(8_000), response_started_at: null,
				},
			],
		});
		await page.goto(`/conversations/${conversationId}`);

		await expect(page.locator('[data-agent-loading]', { hasText: 'Browser-tested workspace is queued to respond' })).toHaveAttribute('data-agent-phase', 'queued');
		await expect(page.locator('[data-agent-loading]', { hasText: 'Preparing Secondary browser workspace' })).toHaveAttribute('data-agent-phase', 'preparing');
		await expect(page.getByText(/Secondary browser workspace is responding/)).toHaveCount(0);
	});
});

test('context usage is stateful and tool completion details stay on one row', async ({ page }) => {
	await mockApi(page, { live: true, richToolActivity: true });
	await page.goto(`/tasks/${taskId}`);

	await expect(page.locator('[data-context-usage]')).toContainText('128,000 / 256,000 tokens');
	await expect(page.locator('[data-context-usage]')).toContainText('129,000 / 256,000 tokens', { timeout: 5_000 });
	await expect(page.getByText('Updated context usage', { exact: true })).toHaveCount(0);
	await expect(page.getByText('Completed Editing files', { exact: true })).toHaveCount(0);
	await expect(page.getByText('Tool call', { exact: true })).toHaveCount(0);

	const editing = page.getByRole('button', { name: /Editing files/ });
	await expect(editing).toHaveCount(1);
	await editing.click();
	const diff = page.locator('[data-tool-diffs]');
	await expect(diff).toContainText('src/example.ts');
	await expect(diff).toContainText('const state = "before";');
	await expect(diff).toContainText('const state = "after";');
	await expect(diff.locator('[data-diff-before-content]')).toHaveCSS('color', 'rgb(153, 27, 27)');
	await expect(diff.locator('[data-diff-after-content]')).toHaveCSS('color', 'rgb(6, 95, 70)');
	await expect(page.getByText('Applied patch.', { exact: true })).toBeVisible();
});

test('new activity follows only while the transcript is at the bottom', async ({ page }) => {
	await mockApi(page, { live: true });
	await page.goto(`/tasks/${taskId}`);

	const scroller = page.locator('[data-task-transcript-scroll]');
	await expect(scroller).toBeVisible();
	await expect.poll(() => scroller.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight)).toBeLessThanOrEqual(24);
	await scroller.evaluate((element) => {
		element.scrollTop = Math.max(0, element.scrollTop - 400);
		element.dispatchEvent(new Event('scroll'));
	});
	await expect(page.getByRole('button', { name: 'Jump to latest' })).toBeVisible();
	const pinnedTop = await scroller.evaluate((element) => element.scrollTop);

	await expect(page.getByRole('button', { name: /New background activity 61/ })).toBeVisible({ timeout: 5_000 });
	await expect.poll(() => scroller.evaluate((element) => element.scrollTop)).toBe(pinnedTop);

	await page.getByRole('button', { name: 'Jump to latest' }).click();
	await expect.poll(() => scroller.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight)).toBeLessThanOrEqual(24);
	await expect(page.getByRole('button', { name: /New background activity 62/ })).toBeVisible({ timeout: 5_000 });
	await expect.poll(() => scroller.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight)).toBeLessThanOrEqual(24);
});

test('task messages accept selected and pasted images', async ({ page }) => {
	const postedMessages: Record<string, unknown>[] = [];
	await installTauriClipboardImage(page);
	await mockApi(page, { postedMessages });
	await page.goto(`/tasks/${taskId}`);

	const fileInput = page.locator('input[type="file"][accept*="image/png"]');
	await fileInput.setInputFiles({
		name: 'selected.png',
		mimeType: 'image/png',
		buffer: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
	});
	await expect(page.getByAltText('selected.png')).toBeVisible();

	const composer = page.locator(`#task-message-input-${taskId}`);
	await composer.evaluate((element) => {
		const transfer = new DataTransfer();
		transfer.items.add(new File(
			[new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
			'pasted.png',
			{ type: 'image/png' },
		));
		Object.defineProperty(transfer, 'files', { value: [] });
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(page.getByAltText('pasted.png')).toBeVisible();

	await composer.evaluate((element) => {
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: new DataTransfer(),
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(page.getByAltText('pasted-image.png')).toBeVisible();
	const textPasteAllowed = await composer.evaluate((element) => {
		const transfer = new DataTransfer();
		transfer.setData('text/plain', 'ordinary text');
		return element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	expect(textPasteAllowed).toBe(true);

	await page.getByRole('button', { name: 'Send message' }).click();
	await expect.poll(() => postedMessages.length).toBe(1);
	const attachments = postedMessages[0].attachments as { name: string; mime_type: string; data: string }[];
	expect(postedMessages[0].content).toBe('');
	expect(attachments.map((attachment) => attachment.name)).toEqual(['selected.png', 'pasted.png', 'pasted-image.png']);
	expect(attachments.every((attachment) => attachment.mime_type === 'image/png' && attachment.data.length > 0)).toBe(true);
	expect(await page.evaluate(() => (
		window as unknown as { __clipboardCommands: string[] }
	).__clipboardCommands)).toContain('plugin:resources|close');
});

test('inline and fenced code stay readable in user and agent chat bubbles in both themes', async ({ page }) => {
	const codeConversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Code contrast review',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(20),
		last_message_at: timestamp(20),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	const conversationMessages = [
		{
			id: 1,
			conversation_id: conversationId,
			sender_type: 'user', sender_id: 'local', sender_name: 'You',
			content: 'Conversation user `conversation-inline-user`.\n\n```ts\nconst conversationUserBlock = true;\n```',
			message_type: 'message', linked_task_id: null, metadata: {}, attachments: [], created_at: timestamp(10),
		},
		{
			id: 2,
			conversation_id: conversationId,
			sender_type: 'agent', sender_id: agentId, sender_name: 'Browser-tested workspace',
			content: 'Conversation agent `conversation-inline-agent`.\n\n```ts\nconst conversationAgentBlock = true;\n```',
			message_type: 'message', linked_task_id: null, metadata: {}, attachments: [], created_at: timestamp(20),
		},
	];
	const taskMessages = [
		{
			id: 1, task_id: taskId, role: 'user', attachments: [], timestamp: timestamp(25),
			content: 'Task user `task-inline-user`.\n\n```ts\nconst taskUserBlock = true;\n```',
		},
		{
			id: 2, task_id: taskId, role: 'assistant', attachments: [], timestamp: timestamp(30),
			content: 'Task agent `task-inline-agent`.\n\n```ts\nconst taskAgentBlock = true;\n```',
		},
	];
	await mockApi(page, {
		conversations: [codeConversation],
		conversationMessages,
		taskMessages,
	});

	await page.goto(`/conversations/${conversationId}`);
	for (const dark of [false, true]) {
		await page.evaluate((useDark) => {
			document.documentElement.classList.toggle('dark', useDark);
			document.documentElement.dataset.theme = useDark ? 'dark' : 'light';
		}, dark);
		await expectReadableCode(page.locator('[data-message-role="user"]', { hasText: 'conversation-inline-user' }));
		await expectReadableCode(page.locator('[data-message-role="assistant"]', { hasText: 'conversation-inline-agent' }));
	}

	await page.goto(`/tasks/${taskId}`);
	for (const dark of [false, true]) {
		await page.evaluate((useDark) => {
			document.documentElement.classList.toggle('dark', useDark);
			document.documentElement.dataset.theme = useDark ? 'dark' : 'light';
		}, dark);
		await expectReadableCode(page.locator('[data-message-role="user"]', { hasText: 'task-inline-user' }));
		await expectReadableCode(page.locator('[data-message-role="assistant"]', { hasText: 'task-inline-agent' }));
	}
});

test('project conversations accept picker, browser, and native clipboard attachments', async ({ page }) => {
	const conversationMessageRequests: Record<string, unknown>[] = [];
	const attachmentConversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Image handoff',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(20),
		last_message_at: timestamp(20),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z2QAAAABJRU5ErkJggg==', 'base64');
	await installTauriClipboardImage(page);
	await mockApi(page, {
		conversations: [attachmentConversation],
		conversationMessageRequests,
	});
	await page.route(new RegExp(`/api/conversations/${conversationId}/attachments/sent-attachment-(1|2)$`), async (route) => {
		await route.fulfill({ status: 200, contentType: 'image/png', body: png });
	});
	await page.goto(`/conversations/${conversationId}`);

	const fileInput = page.locator('[data-conversation-attachment-input]');
	await fileInput.setInputFiles([
		{ name: 'selected.png', mimeType: 'image/png', buffer: png },
		{ name: 'notes.txt', mimeType: 'text/plain', buffer: Buffer.from('supporting notes') },
	]);
	const composer = page.getByPlaceholder('Message #Image handoff…');
	const composerShell = page.locator('[data-conversation-composer]');
	await expect(composerShell.getByAltText('selected.png')).toBeVisible();
	await expect(composerShell.getByAltText('selected.png')).toHaveAttribute('src', /^data:image\/png;base64,/);
	await expect(composerShell.getByText('notes.txt', { exact: true })).toBeVisible();

	await composer.evaluate((element) => {
		const transfer = new DataTransfer();
		transfer.items.add(new File(
			[new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
			'pasted.png',
			{ type: 'image/png' },
		));
		Object.defineProperty(transfer, 'files', { value: [] });
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(composerShell.getByAltText('pasted.png')).toBeVisible();

	await composer.evaluate((element) => {
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: new DataTransfer(),
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(composerShell.getByAltText('pasted-image.png')).toBeVisible();

	const textPasteAllowed = await composer.evaluate((element) => {
		const transfer = new DataTransfer();
		transfer.setData('text/plain', 'ordinary text');
		return element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	expect(textPasteAllowed).toBe(true);

	await composer.fill('Keep this draft');
	await composer.evaluate((element) => {
		const transfer = new DataTransfer();
		transfer.setData('text/plain', 'clipboard caption');
		transfer.items.add(new File([new Uint8Array([1, 2, 3, 4])], 'mixed.png', { type: 'image/png' }));
		Object.defineProperty(transfer, 'files', { value: [] });
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(composer).toHaveValue('Keep this draft');
	await expect(composerShell.getByAltText('mixed.png')).toBeVisible();

	await fileInput.setInputFiles(Array.from({ length: 6 }, (_, index) => ({
		name: `overflow-${index + 1}.txt`,
		mimeType: 'text/plain',
		buffer: Buffer.from('x'),
	})));
	await expect(composerShell.getByRole('alert')).toHaveText('A message can include up to 10 files.');
	await expect(composerShell.getByAltText('selected.png')).toBeVisible();

	await composerShell.getByRole('button', { name: 'Remove selected.png' }).click();
	await composerShell.getByRole('button', { name: 'Remove mixed.png' }).click();
	await expect(composerShell.getByRole('alert')).toHaveCount(0);
	await expect(composerShell.getByAltText('selected.png')).toHaveCount(0);
	await page.evaluate(() => {
		const target = window as unknown as {
			__clipboardCommands: string[];
			__TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> };
		};
		target.__TAURI_INTERNALS__.invoke = async (command: string) => {
			target.__clipboardCommands.push(command);
			if (command === 'plugin:clipboard-manager|read_image') throw new Error('Clipboard unavailable');
			throw new Error(`Unexpected Tauri command: ${command}`);
		};
	});
	await composer.evaluate((element) => {
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: new DataTransfer(),
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(composerShell.getByRole('alert')).toHaveText('Could not read an image from the system clipboard.');
	await composer.fill('');
	await page.getByRole('button', { name: 'Send', exact: true }).click();

	await expect.poll(() => conversationMessageRequests.length).toBe(1);
	const request = conversationMessageRequests[0];
	const attachments = request.attachments as { name: string; mime_type: string; data: string }[];
	expect(request.content).toBe('');
	expect(attachments.map((attachment) => attachment.name)).toEqual(['notes.txt', 'pasted.png', 'pasted-image.png']);
	expect(attachments.every((attachment) => attachment.data.length > 0)).toBe(true);
	await expect(composerShell.locator('[data-image-attachments]')).toHaveCount(0);
	await expect(composerShell.getByText('notes.txt', { exact: true })).toHaveCount(0);
	await expect(composerShell.getByRole('alert')).toHaveCount(0);

	const sentPastedImage = page.getByAltText('pasted.png');
	const sentNativeImage = page.getByAltText('pasted-image.png');
	await expect(sentPastedImage).toBeVisible();
	await expect(sentNativeImage).toBeVisible();
	await expect.poll(() => sentPastedImage.evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
	await expect.poll(() => sentNativeImage.evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
	expect(await page.evaluate(() => (
		window as unknown as { __clipboardCommands: string[] }
	).__clipboardCommands)).toContain('plugin:resources|close');
});

test('project conversations coordinate files and project-wide linked work', async ({ page }) => {
	const conversationMessageRequests: Record<string, unknown>[] = [];
	const conversationTaskRequests: Record<string, unknown>[] = [];
	const collaborationConversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Release planning',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(20),
		last_message_at: timestamp(20),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	await mockApi(page, {
		multipleAgents: true,
		conversations: [collaborationConversation],
		conversationMessages: [{
			id: 1,
			conversation_id: conversationId,
			sender_type: 'agent',
			sender_id: agentId,
			sender_name: 'Browser-tested workspace',
			content: 'I finished the research while the implementation task kept running.',
			message_type: 'message',
			linked_task_id: null,
			metadata: { source_task_id: taskId },
			attachments: [{
				id: 'published-findings',
				message_id: 1,
				name: 'findings.md',
				mime_type: 'text/markdown',
				size: 128,
				source_task_id: taskId,
				created_at: timestamp(20),
			}],
			created_at: timestamp(20),
		}],
		conversationMessageRequests,
		conversationTaskRequests,
	});
	await page.goto(`/conversations/${conversationId}`);

	await expect(page.getByRole('heading', { name: 'Release planning' })).toBeVisible();
	await expect(page.getByRole('button', { name: '1 Agent' })).toBeVisible();
	await expect(page.getByText('I finished the research while the implementation task kept running.')).toBeVisible();
	const sidebar = page.locator('aside').first();
	await expect(sidebar.locator(`a[href="/projects/${projectId}"]`).filter({ hasText: 'Browser collaboration project' })).toBeVisible();
	await expect(sidebar.locator(`a[href="/conversations/${conversationId}"]`)).toBeVisible();

	await page.getByRole('button', { name: /Files 1/ }).click();
	await expect(page.getByRole('link', { name: /findings\.md/ })).toHaveAttribute(
		'href',
		`/api/conversations/${conversationId}/attachments/published-findings`,
	);
	await page.getByRole('button', { name: 'Conversation' }).click();

	const composer = page.getByPlaceholder('Message #Release planning…');
	await composer.fill('Keep this conversation draft');
	const assistantMessage = page.locator('[data-message-role="assistant"]').first();
	await assistantMessage.locator('.prose-chat').evaluate((element) => {
		const selection = window.getSelection();
		const range = document.createRange();
		range.selectNodeContents(element);
		selection?.removeAllRanges();
		selection?.addRange(range);
		element.dispatchEvent(new PointerEvent('pointerup', { bubbles: true }));
	});
	await assistantMessage.locator('[data-selection-actions]').getByRole('button', { name: 'Shorten' }).click();
	await expect(composer).toHaveValue(/Keep this conversation draft\n\nShorten this passage:/);

	await composer.fill('Please compare both approaches.');
	await page.getByRole('button', { name: 'Send', exact: true }).click();
	await expect.poll(() => conversationMessageRequests).toEqual([{
		content: 'Please compare both approaches.',
		attachments: [],
	}]);

	await page.getByRole('button', { name: 'Continue with task' }).click();
	await page.getByLabel('Title', { exact: true }).fill('Implement the selected approach');
	await page.getByLabel('Details', { exact: true }).fill('Use the decisions and files already published here.');
	await expect(page.getByLabel('Agent', { exact: true }).locator('option')).toHaveCount(3);
	await page.getByLabel('Agent', { exact: true }).selectOption('project-secondary-test');
	await page.getByRole('button', { name: 'Create task' }).click();
	await expect.poll(() => conversationTaskRequests).toEqual([{
		title: 'Implement the selected approach',
		description: 'Use the decisions and files already published here.',
		agent_id: 'project-secondary-test',
	}]);
});

test('raw HTML in user and agent conversation messages stays visible and inert', async ({ page }) => {
	const userRawHtml = '<script>alert("wee");</script>';
	const userReservedTags = '<think>literal user thinking</think> <tool_call name="demo">{"ok":true}</tool_call>';
	const execution = 'window.__rawHtmlExecuted = true';
	const agentRawHtml = [
		'**Markdown stays bold.**',
		'',
		`<img src="missing-image" onerror="${execution}">`,
		'',
		`<svg onload="${execution}"><circle></circle></svg>`,
		'',
		`<iframe srcdoc="<script>${execution}</script>"></iframe>`,
		'',
		'<style>body { display: none; }</style>',
		'',
		'<custom-card data-kind="demo">custom content</custom-card>',
		'',
		'Mixed Markdown with <mark>benign HTML</mark> and <span data-raw-html="true">inline HTML</span>.',
		'',
		'<div data-raw-html="block">raw block</div>',
		'**Adjacent Markdown stays bold.** and [adjacent link](https://example.com/adjacent)',
		'',
		'<broken attr="value"',
		'',
		`&lt;img src=x onerror="${execution}"&gt;`,
		'',
		'[Markdown link](https://example.com/safe)',
	].join('\n');
	const rawHtmlConversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Raw HTML safety',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(20),
		last_message_at: timestamp(20),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	await page.addInitScript(() => {
		(window as typeof window & { __rawHtmlExecuted?: boolean }).__rawHtmlExecuted = false;
	});
	await mockApi(page, {
		conversations: [rawHtmlConversation],
		conversationMessages: [
			{
				id: 1,
				conversation_id: conversationId,
				sender_type: 'user',
				sender_id: 'local',
				sender_name: 'You',
				content: userRawHtml,
				message_type: 'message',
				linked_task_id: null,
				metadata: {},
				attachments: [],
				created_at: timestamp(10),
			},
			{
				id: 2,
				conversation_id: conversationId,
				sender_type: 'user',
				sender_id: 'local',
				sender_name: 'You',
				content: userReservedTags,
				message_type: 'message',
				linked_task_id: null,
				metadata: {},
				attachments: [],
				created_at: timestamp(15),
			},
			{
				id: 3,
				conversation_id: conversationId,
				sender_type: 'agent',
				sender_id: agentId,
				sender_name: 'Browser-tested workspace',
				content: agentRawHtml,
				message_type: 'message',
				linked_task_id: null,
				metadata: {},
				attachments: [],
				created_at: timestamp(20),
			},
		],
	});
	await page.goto(`/conversations/${conversationId}`);

	let userContent = page.locator('[data-message-role="user"] .prose-chat').first();
	let userReservedContent = page.locator('[data-message-role="user"] .prose-chat').nth(1);
	let agentContent = page.locator('[data-message-role="assistant"] .prose-chat');
	await expect(userContent).toHaveText(userRawHtml);
	expect(await userContent.textContent()).toBe(userRawHtml);
	await expect(userReservedContent).toContainText('<think>literal user thinking</think>');
	await expect(userReservedContent).toContainText('<tool_call name="demo">{"ok":true}</tool_call>');
	await expect(userReservedContent.locator('details, tool_call, think')).toHaveCount(0);
	await expect(agentContent.getByText('Markdown stays bold.', { exact: true })).toHaveCount(1);
	await expect(agentContent.locator('strong', { hasText: /^Markdown stays bold\.$/ })).toHaveCount(1);
	await expect(agentContent).toContainText('<img src="missing-image" onerror="window.__rawHtmlExecuted = true">');
	await expect(agentContent).toContainText('<svg onload="window.__rawHtmlExecuted = true"><circle></circle></svg>');
	await expect(agentContent).toContainText('<iframe srcdoc="<script>window.__rawHtmlExecuted = true</script>"></iframe>');
	await expect(agentContent).toContainText('<style>body { display: none; }</style>');
	await expect(agentContent).toContainText('<custom-card data-kind="demo">custom content</custom-card>');
	await expect(agentContent).toContainText('<mark>benign HTML</mark>');
	await expect(agentContent).toContainText('<span data-raw-html="true">inline HTML</span>');
	await expect(agentContent).toContainText('<div data-raw-html="block">raw block</div>');
	await expect(agentContent.locator('strong', { hasText: /^Adjacent Markdown stays bold\.$/ })).toHaveCount(1);
	await expect(agentContent.getByRole('link', { name: 'adjacent link' })).toHaveAttribute('href', 'https://example.com/adjacent');
	await expect(agentContent).toContainText('<broken attr="value"');
	await expect(agentContent).toContainText('&lt;img src=x onerror="window.__rawHtmlExecuted = true"&gt;');
	await expect(agentContent.locator('script, img, svg, iframe, style, custom-card, mark, div[data-raw-html], span[data-raw-html]')).toHaveCount(0);
	await expect(agentContent.getByRole('link', { name: 'Markdown link' })).toHaveAttribute('href', 'https://example.com/safe');
	await expect.poll(() => page.evaluate(() => (
		window as typeof window & { __rawHtmlExecuted?: boolean }
	).__rawHtmlExecuted)).toBe(false);

	await page.reload();
	userContent = page.locator('[data-message-role="user"] .prose-chat').first();
	userReservedContent = page.locator('[data-message-role="user"] .prose-chat').nth(1);
	agentContent = page.locator('[data-message-role="assistant"] .prose-chat');
	await expect(userContent).toHaveText(userRawHtml);
	expect(await userContent.textContent()).toBe(userRawHtml);
	await expect(userContent).not.toContainText('&lt;script&gt;');
	await expect(userReservedContent).toContainText(userReservedTags);
	await expect(agentContent).toContainText('<custom-card data-kind="demo">custom content</custom-card>');

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(userContent).toHaveText(userRawHtml);
	await expect(agentContent.locator('script, img, svg, iframe, style, custom-card, mark, div[data-raw-html], span[data-raw-html]')).toHaveCount(0);
});

test('raw HTML stays literal across task messages, activity, results, and previews', async ({ page }) => {
	const execution = 'window.__rawHtmlExecuted = true';
	const taskPrompt = '<custom-prompt data-value="1">task prompt</custom-prompt>';
	const taskReply = `**Task Markdown** with <img src=x onerror="${execution}">`;
	const streamingUpdate = `<svg onload="${execution}">streaming update</svg>`;
	const thought = `**Thought Markdown** with <iframe src="javascript:${execution}">thought</iframe>`;
	const result = `**Result Markdown** with <custom-result>result body</custom-result>`;
	await page.addInitScript(() => {
		(window as typeof window & { __rawHtmlExecuted?: boolean }).__rawHtmlExecuted = false;
	});
	await mockApi(page, {
		taskDescription: taskPrompt,
		taskMessages: [{
			id: 1,
			task_id: taskId,
			role: 'assistant',
			content: taskReply,
			attachments: [],
			timestamp: timestamp(25),
		}],
		taskActivityEvents: [
			timelineEvent(90, 30, 'runner_progress', streamingUpdate, { item_type: 'agent_message', message_id: 'raw-update' }),
			timelineEvent(91, 35, 'agent_thought', thought, {}),
		],
		attemptResult: result,
	});
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	const promptContent = transcript.locator('[data-message-role="user"] .prose-chat');
	const replyContent = transcript.locator('[data-message-role="assistant"] .prose-chat');
	await expect(promptContent).toHaveText(taskPrompt);
	await expect(promptContent.locator('custom-prompt')).toHaveCount(0);
	await expect(replyContent.locator('strong')).toHaveText('Task Markdown');
	await expect(replyContent).toContainText(`<img src=x onerror="${execution}">`);
	await expect(replyContent.locator('img')).toHaveCount(0);

	const updateContent = transcript.locator('[data-agent-update-content]');
	await expect(updateContent).toContainText(streamingUpdate);
	await expect(updateContent.locator('svg')).toHaveCount(0);

	const thoughtActivity = transcript.locator('[data-transcript-kind="activity"]', { hasText: 'Thought Markdown' });
	await thoughtActivity.getByRole('button').click();
	const thoughtContent = thoughtActivity.locator('[data-activity-rich-content]');
	await expect(thoughtContent.locator('strong')).toHaveText('Thought Markdown');
	await expect(thoughtContent).toContainText(`<iframe src="javascript:${execution}">thought</iframe>`);
	await expect(thoughtContent.locator('iframe')).toHaveCount(0);

	const resultContent = page.locator('[data-task-result-content]');
	await expect(resultContent.locator('strong')).toHaveText('Result Markdown');
	await expect(resultContent).toContainText('<custom-result>result body</custom-result>');
	await expect(resultContent.locator('custom-result')).toHaveCount(0);
	await expect.poll(() => page.evaluate(() => (
		window as typeof window & { __rawHtmlExecuted?: boolean }
	).__rawHtmlExecuted)).toBe(false);

	await page.goto('/tasks');
	await page.getByRole('button', { name: 'Done 1' }).click();
	const preview = page.locator(`[data-task-row][href="/tasks/${taskId}"]`);
	await expect(preview).toContainText(taskPrompt);
	await expect(preview.locator('custom-prompt')).toHaveCount(0);
});

test('task presentation attachments render as durable download cards', async ({ page }) => {
	await mockApi(page, {
		taskMessages: [{
			id: 7,
			task_id: taskId,
			role: 'assistant',
			content: 'The rendered and validated deck is attached.',
			attachments: [{
				id: 'presentation-browser-test',
				name: 'Review deck.pptx',
				mime_type: 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
				size: 2_621_440,
			}],
			timestamp: timestamp(25),
		}],
	});
	await page.goto(`/tasks/${taskId}`);

	const message = page.locator('[data-message-role="assistant"]');
	await expect(message).toContainText('The rendered and validated deck is attached.');
	const attachment = message.getByRole('link', { name: 'Download Review deck.pptx' });
	await expect(attachment).toHaveAttribute(
		'href',
		`/api/tasks/${taskId}/messages/7/attachments/presentation-browser-test`,
	);
	await expect(attachment).toContainText('PowerPoint presentation · 2.5 MB');
	await expect(message.locator('[data-message-attachments] img')).toHaveCount(0);
});

test('task assistant visualizations render in order, stay isolated, and confirm follow-ups', async ({ page }) => {
	const postedMessages: Record<string, unknown>[] = [];
	const visualizationRequests: { path: string; token: string | undefined }[] = [];
	const firstReference = 'visualize{"path":"/workspace/dependencies.html","title":"Task dependency map"}';
	const wideReference = 'visualize{"path":"/workspace/timeline.html","mode":"wide"}';
	const missingReference = 'visualize{"path":"/workspace/large.html"}';
	const userReference = 'visualize{"path":"/workspace/user.html"}';
	const firstPath = `/api/tasks/${taskId}/messages/2/visualizations/viz-task-one`;
	const widePath = `/api/tasks/${taskId}/messages/2/visualizations/viz-task-wide`;
	await mockApi(page, {
		postedMessages,
		visualizationRequests,
		visualizationDocuments: {
			[firstPath]: visualizationDocument('viz-task-one', 'Task visualization loaded', { prompt: 'Inspect dependency node A', title: 'Inspect dependency' }),
			[widePath]: visualizationDocument('viz-task-wide', 'Wide timeline loaded'),
		},
		taskMessages: [
			{ id: 1, task_id: taskId, role: 'user', content: userReference, attachments: [], visualizations: [], timestamp: timestamp(20) },
			{
				id: 2,
				task_id: taskId,
				role: 'assistant',
				content: [
					'**Dependency analysis**',
					firstReference,
					'Markdown between views.',
					wideReference,
					missingReference,
					'\\visualize{"path":"/workspace/escaped.html"}',
					'visualize{"path":"/workspace/bad.html","mode":"fullscreen"}',
				].join('\n\n'),
				attachments: [],
				visualizations: [
					{ id: 'viz-task-one', reference_index: 0, title: 'Task dependency map', mode: 'normal', status: 'ready', error_code: null, size: 120, retrieval_token: 'token-one' },
					{ id: 'viz-task-wide', reference_index: 1, title: 'Timeline', mode: 'wide', status: 'ready', error_code: null, size: 130, retrieval_token: 'token-wide' },
					{ id: 'viz-task-missing', reference_index: 2, title: 'Large view', mode: 'normal', status: 'unavailable', error_code: 'oversize', size: null, retrieval_token: 'token-missing' },
				],
				timestamp: timestamp(25),
			},
		],
	});
	await page.goto(`/tasks/${taskId}`);

	const assistant = page.locator('[data-message-role="assistant"]');
	await expect(assistant.locator('strong', { hasText: 'Dependency analysis' })).toBeVisible();
	await expect(assistant).toContainText('Markdown between views.');
	await expect(page.locator('[data-inline-visualization]')).toHaveCount(3);
	await expect(page.locator('[data-inline-visualization][data-visualization-mode="wide"]')).toHaveCount(1);
	await expect(page.getByText('The generated visualization exceeded the 1 MiB limit.')).toBeVisible();
	await expect(page.locator('[data-message-role="user"] .prose-chat').filter({ hasText: userReference })).toHaveText(userReference);
	await expect(assistant).toContainText('visualize{"path":"/workspace/escaped.html"}');
	await expect(assistant).toContainText('visualize{"path":"/workspace/bad.html","mode":"fullscreen"}');

	const firstFrameElement = page.locator('iframe[title="Task dependency map"]');
	await expect(firstFrameElement).toHaveAttribute('sandbox', 'allow-scripts');
	await expect(firstFrameElement).toHaveAttribute('referrerpolicy', 'no-referrer');
	const firstOuterFrame = page.frameLocator('iframe[title="Task dependency map"]');
	const firstFrame = firstOuterFrame.frameLocator('iframe[data-artifact-frame]');
	await expect(firstFrame.getByText('Task visualization loaded')).toBeVisible();
	await expect(firstFrame.locator('html')).toHaveAttribute('data-parent-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-storage-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-cookie-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-popup-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-navigation-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-network-blocked', 'true');
	await expect(firstFrame.locator('html')).toHaveAttribute('data-form-blocked', 'true');
	await page.evaluate(() => document.documentElement.classList.add('dark'));
	await expect(firstFrame.locator('html')).toHaveAttribute('data-theme', 'dark');
	await page.evaluate(() => document.documentElement.classList.remove('dark'));
	await expect(firstFrame.locator('html')).toHaveAttribute('data-theme', 'light');
	await expect(page.frameLocator('iframe[title="Timeline"]').frameLocator('iframe[data-artifact-frame]').getByText('Wide timeline loaded')).toBeVisible();
	await expect.poll(() => visualizationRequests).toEqual(expect.arrayContaining([
		{ path: firstPath, token: 'token-one' },
		{ path: widePath, token: 'token-wide' },
	]));

	await firstFrame.getByRole('button', { name: 'Follow up' }).click();
	const dialog = page.getByRole('dialog', { name: 'Inspect dependency' });
	await expect(dialog).toContainText('Inspect dependency node A');
	expect(postedMessages).toEqual([]);
	await dialog.getByRole('button', { name: 'Cancel' }).click();
	expect(postedMessages).toEqual([]);
	await firstFrame.getByRole('button', { name: 'Follow up' }).click();
	await page.getByRole('dialog', { name: 'Inspect dependency' }).getByRole('button', { name: 'Send follow-up' }).click();
	await expect.poll(() => postedMessages).toEqual([expect.objectContaining({
		role: 'user',
		content: 'Inspect dependency node A',
		delivery: 'after_tool',
	})]);

	await firstFrame.locator('html').evaluate((_, artifactId) => {
		parent.postMessage({ source: 'xpressclaw-visualization', type: 'resize', artifactId, height: 260 }, '*');
	}, 'viz-task-one');
	await expect(firstFrameElement).toHaveCSS('height', '260px');
	await page.setViewportSize({ width: 390, height: 844 });
	const firstCard = page.locator('[data-inline-visualization]').first();
	await firstCard.getByRole('button', { name: 'Expand visualization' }).click();
	await expect(firstCard).toHaveCSS('position', 'fixed');
	await firstFrame.locator('html').evaluate((_, artifactId) => {
		parent.postMessage({ source: 'xpressclaw-visualization', type: 'resize', artifactId, height: 700 }, '*');
	}, 'viz-task-one');
	await firstCard.getByRole('button', { name: 'Exit expanded visualization' }).click();
	await expect(firstCard).not.toHaveCSS('position', 'fixed');
	await expect(firstFrameElement).toHaveCSS('height', '260px');

	const selfNavigationRequests: string[] = [];
	page.on('request', (request) => {
		if (request.url().startsWith('https://example.invalid/visualization-self-navigation')) selfNavigationRequests.push(request.url());
	});
	await firstFrame.locator('html').evaluate(() => {
		window.location.assign('https://example.invalid/visualization-self-navigation?artifact-data=secret');
	});
	await expect(firstOuterFrame.getByRole('status')).toHaveText('Navigation was blocked.');
	expect(selfNavigationRequests).toEqual([]);
});

test('conversation visualizations persist on reload and route confirmed follow-ups to the conversation', async ({ page }) => {
	const conversationMessageRequests: Record<string, unknown>[] = [];
	const visualizationRequests: { path: string; token: string | undefined }[] = [];
	const reference = 'visualize{"path":"/workspace/conversation-map.html","title":"Conversation map"}';
	const path = `/api/conversations/${conversationId}/messages/3/visualizations/viz-conversation`;
	const conversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Visualization review',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(20),
		last_message_at: timestamp(20),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	await mockApi(page, {
		conversations: [conversation],
		conversationMessageRequests,
		visualizationRequests,
		visualizationDocuments: {
			[path]: visualizationDocument('viz-conversation', 'Conversation visualization loaded', { prompt: 'Compare the highlighted paths' }),
		},
		conversationMessages: [
			{ id: 1, conversation_id: conversationId, sender_type: 'user', sender_id: 'local', sender_name: 'You', content: reference, message_type: 'message', linked_task_id: null, metadata: {}, attachments: [], visualizations: [], created_at: timestamp(10) },
			{ id: 2, conversation_id: conversationId, sender_type: 'system', sender_id: 'system', sender_name: 'System', content: reference, message_type: 'message', linked_task_id: null, metadata: {}, attachments: [], visualizations: [], created_at: timestamp(15) },
			{ id: 3, conversation_id: conversationId, sender_type: 'agent', sender_id: agentId, sender_name: 'Browser-tested workspace', content: `Here is the map.\n\n${reference}`, message_type: 'message', linked_task_id: null, metadata: {}, attachments: [], visualizations: [{ id: 'viz-conversation', reference_index: 0, title: 'Conversation map', mode: 'normal', status: 'ready', error_code: null, size: 120, retrieval_token: 'conversation-token' }], created_at: timestamp(20) },
		],
	});
	await page.goto(`/conversations/${conversationId}`);

	await expect(page.locator('[data-inline-visualization]')).toHaveCount(1);
	await expect(page.locator('[data-message-role="user"] .prose-chat')).toHaveText(reference);
	await expect(page.locator('[data-message-role="system"] .prose-chat')).toHaveText(reference);
	const frame = page.frameLocator('iframe[title="Conversation map"]').frameLocator('iframe[data-artifact-frame]');
	await expect(frame.getByText('Conversation visualization loaded')).toBeVisible();
	await frame.getByRole('button', { name: 'Follow up' }).click();
	const dialog = page.getByRole('dialog', { name: 'Send this follow-up?' });
	await expect(dialog).toContainText('Compare the highlighted paths');
	expect(conversationMessageRequests).toEqual([]);
	await dialog.getByRole('button', { name: 'Send follow-up' }).click();
	await expect.poll(() => conversationMessageRequests).toEqual([{ content: 'Compare the highlighted paths', attachments: [] }]);

	await page.reload();
	await expect(page.locator('[data-inline-visualization]')).toHaveCount(1);
	await expect(page.frameLocator('iframe[title="Conversation map"]').frameLocator('iframe[data-artifact-frame]').getByText('Conversation visualization loaded')).toBeVisible();
	await expect.poll(() => visualizationRequests.filter((request) => request.path === path).length).toBeGreaterThanOrEqual(2);
});

test('project pages expose a copyable canonical ID', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: async (value: string) => {
					(window as unknown as { __copiedProjectId: string }).__copiedProjectId = value;
				},
			},
		});
	});
	await mockApi(page);
	await page.goto(`/projects/${projectId}`);

	await expect(page.locator('[data-project-id]')).toHaveText(projectId);
	await page.getByRole('button', { name: 'Copy project ID' }).click();
	await expect(page.getByRole('button', { name: 'Copy project ID' })).toHaveText('Copied');
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __copiedProjectId?: string }
	).__copiedProjectId)).toBe(projectId);
});

test('project settings rename and delete a project', async ({ page }) => {
	const projectUpdateRequests: { projectId: string; data: Record<string, unknown> }[] = [];
	const projectDeleteRequests: string[] = [];
	const projectDeleteAcknowledgements: (string | null)[] = [];
	await mockApi(page, {
		projectUpdateRequests,
		projectDeleteRequests,
		projectDeleteAcknowledgements,
		projectDeletionCounts: {
			agents: 1,
			tasks: 3,
			task_messages: 7,
			conversations: 2,
			conversation_messages: 11,
			memory_notes: 4,
			workflow_runs: 5,
			schedules: 2,
		},
	});
	await page.goto(`/projects/${projectId}`);

	await page.getByRole('button', { name: 'Project settings' }).click();
	let dialog = page.getByRole('dialog');
	await expect(dialog).toHaveAccessibleName('Project settings');
	await dialog.getByLabel('Project name').fill('Renamed collaboration project');
	await dialog.getByLabel('Description').fill('A clearer project description.');
	await dialog.getByRole('button', { name: 'Save changes' }).click();

	await expect.poll(() => projectUpdateRequests).toEqual([{
		projectId,
		data: {
			name: 'Renamed collaboration project',
			description: 'A clearer project description.',
		},
	}]);
	await expect(page.getByRole('heading', { name: 'Renamed collaboration project' })).toBeVisible();
	await expect(page.getByText('A clearer project description.')).toBeVisible();
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab-title="Renamed collaboration project"]')).toBeVisible();
	await expect(page.locator('aside').first().getByText('Renamed collaboration project', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Project settings' }).click();
	dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Delete project' }).click();
	await expect(dialog.getByRole('heading', { name: 'Delete project?' })).toBeVisible();
	await expect(dialog.getByText('This permanently deletes “Renamed collaboration project” and cannot be undone.')).toBeVisible();
	await expect(dialog.getByText('7 task messages', { exact: false })).toBeVisible();
	await expect(dialog.getByText('11 conversation messages', { exact: false })).toBeVisible();
	await expect(dialog.getByText('local collaboration access', { exact: false })).toBeVisible();
	await expect(dialog.getByText('including steps and waits', { exact: false })).toBeVisible();
	await expect(dialog.getByText('source repositories, workspace folders, and reusable workflow definitions', { exact: false })).toBeVisible();
	const permanentDelete = dialog.getByRole('button', { name: 'Permanently delete project' });
	await expect(permanentDelete).toBeDisabled();
	await dialog.getByLabel('Type Renamed collaboration project to confirm').fill('Renamed collaboration project');
	await permanentDelete.click();

	await expect.poll(() => projectDeleteRequests).toEqual([projectId]);
	await expect.poll(() => projectDeleteAcknowledgements).toEqual(['confirmed']);
	await expect(page).toHaveURL(/\/projects$/);
	await expect(page.getByRole('heading', { name: 'Projects' })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Create your first project' })).toBeVisible();
	await expect(page.locator(`[data-workspace-tab][data-workspace-tab-title="Renamed collaboration project"]`)).toHaveCount(0);
});

test('project deletion refreshes the authoritative name and counts before confirmation', async ({ page }) => {
	const projectGetRequests: string[] = [];
	const sharedProjectState: SharedProjectState = {};
	await mockApi(page, { projectGetRequests, sharedProjectState });
	await page.goto(`/projects/${projectId}`);
	await expect.poll(() => projectGetRequests).toHaveLength(1);

	sharedProjectState.project = {
		...sharedProjectState.project!,
		name: 'Current Project name',
		updated_at: timestamp(120),
		deletion_counts: {
			agents: 2,
			tasks: 9,
			task_messages: 13,
			conversations: 4,
			conversation_messages: 17,
			memory_notes: 3,
			workflow_runs: 5,
			schedules: 6,
		},
	};

	await page.getByRole('button', { name: 'Project settings' }).click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Delete project' }).click();

	await expect.poll(() => projectGetRequests).toHaveLength(2);
	await expect(dialog.getByRole('heading', { name: 'Delete project?' })).toBeVisible();
	await expect(dialog.getByText('This permanently deletes “Current Project name” and cannot be undone.')).toBeVisible();
	await expect(dialog.getByText('9 tasks', { exact: false })).toBeVisible();
	await expect(dialog.getByText('13 task messages', { exact: false })).toBeVisible();
	await expect(dialog.getByText('17 conversation messages', { exact: false })).toBeVisible();
	await expect(dialog.getByLabel('Type Current Project name to confirm')).toBeVisible();
	await expect(dialog.getByLabel('Type Browser collaboration project to confirm')).toHaveCount(0);
});

test('project mutations synchronize split panes and separate workspace windows', async ({ page, context }) => {
	const projectUpdateRequests: { projectId: string; data: Record<string, unknown> }[] = [];
	const projectDeleteRequests: string[] = [];
	const sharedProjectState: SharedProjectState = {};
	await mockApi(page, { projectUpdateRequests, projectDeleteRequests, sharedProjectState });
	await page.goto(`/projects/${projectId}`);

	await page.getByRole('button', { name: 'Split active tab right' }).click();
	const panes = page.locator('[data-workspace-pane]');
	await expect(panes).toHaveCount(2);
	await expect(panes.getByRole('heading', { name: 'Browser collaboration project' })).toHaveCount(2);

	let releaseProjectGet!: () => void;
	const projectGetGate = new Promise<void>((resolve) => (releaseProjectGet = resolve));
	const projectGetRequests: string[] = [];
	const otherPage = await context.newPage();
	await otherPage.addInitScript(() => {
		window.addEventListener('xpressclaw:project-mutation', (event) => {
			const mutation = (event as CustomEvent<{ kind: string; project?: { name?: string } }>).detail;
			(window as typeof window & { __latestProjectMutation?: string }).__latestProjectMutation = mutation.project?.name;
		});
	});
	await mockApi(otherPage, { preserveWorkspace: true, projectGetRequests, projectGetGate, sharedProjectState });
	const otherNavigation = otherPage.goto(`/projects/${projectId}?_xpressclaw_window=workspace-12345-1`);
	await expect.poll(() => projectGetRequests.length).toBeGreaterThan(0);
	expect(projectGetRequests.every((requestedProjectId) => requestedProjectId === projectId)).toBe(true);

	let releaseProjectList!: () => void;
	const projectListGate = new Promise<void>((resolve) => (releaseProjectList = resolve));
	const projectListRequests: string[] = [];
	const indexPage = await context.newPage();
	await mockApi(indexPage, {
		preserveWorkspace: true,
		projectCount: 2,
		projectListRequests,
		projectListGate,
		projectListTargetLast: true,
		secondaryProjectName: 'Éclair project',
		secondaryProjectUpdatedAt: timestamp(120),
		sharedProjectState,
	});
	const indexNavigation = indexPage.goto('/projects?_xpressclaw_window=workspace-12345-2');
	await expect.poll(() => projectListRequests.length).toBeGreaterThan(1);

	const newWorkPage = await context.newPage();
	await mockApi(newWorkPage, { preserveWorkspace: true, sharedProjectState });
	await newWorkPage.goto('/?_xpressclaw_window=workspace-12345-3');
	const newWorkProject = newWorkPage.getByLabel('Project').locator(`option[value="${projectId}"]`);
	await expect(newWorkProject).toHaveText('Browser collaboration project');

	const syncPage = await context.newPage();
	await mockApi(syncPage, {
		preserveWorkspace: true,
		sharedProjectState,
		projectSyncStatuses: [{
			project_id: projectId,
			project_name: 'Browser collaboration project',
			project_icon: null,
			status: 'ready',
			project_dir: '/srv/repos/xpressclaw',
			remote: 'git@github.com:XpressAI/project-data.git',
			branch: 'main',
			store_path: `projects/${projectId}`,
			share_project_memory: true,
			last_commit: null,
			last_synced_at: null,
			message: null,
			warnings: [],
		}],
	});
	await syncPage.goto('/settings/sync?_xpressclaw_window=workspace-12345-4');
	const syncProject = syncPage.locator(`[data-project-sync="${projectId}"]`);
	await expect(syncProject.getByRole('heading', { name: 'Browser collaboration project' })).toBeVisible();

	const conversationPage = await context.newPage();
	await mockApi(conversationPage, {
		preserveWorkspace: true,
		sharedProjectState,
		conversations: [{
			id: conversationId,
			project_id: projectId,
			title: 'Mounted project conversation',
			icon: null,
			created_at: timestamp(1),
			updated_at: timestamp(61),
			last_message_at: timestamp(61),
			participants: [],
		}],
	});
	await conversationPage.goto(`/conversations/${conversationId}?_xpressclaw_window=workspace-12345-5`);
	const conversationProjectLink = conversationPage.locator('header p a');
	await expect(conversationProjectLink).toHaveText('Browser collaboration project');

	await panes.first().getByRole('button', { name: 'Project settings' }).click();
	let dialog = page.getByRole('dialog');
	await dialog.getByLabel('Project name').fill('Synchronized project');
	await dialog.getByLabel('Description').fill('Visible in every project view.');
	await dialog.getByRole('button', { name: 'Save changes' }).click();

	await expect.poll(() => projectUpdateRequests).toHaveLength(1);
	await expect(panes.getByRole('heading', { name: 'Synchronized project' })).toHaveCount(2);
	await expect(panes.getByText('Visible in every project view.', { exact: true })).toHaveCount(2);
	await expect.poll(() => otherPage.evaluate(() => (
		window as typeof window & { __latestProjectMutation?: string }
	).__latestProjectMutation)).toBe('Synchronized project');
	releaseProjectGet();
	releaseProjectList();
	await otherNavigation;
	await indexNavigation;
	const otherPane = otherPage.locator('[data-workspace-pane]');
	await expect(otherPane.getByRole('heading', { name: 'Synchronized project' })).toBeVisible();
	await expect(otherPane.getByText('Visible in every project view.', { exact: true })).toBeVisible();
	await expect(otherPage.locator('[data-workspace-pane] [data-workspace-tab-title="Synchronized project"]')).toBeVisible();
	await expect(indexPage.getByRole('heading', { name: 'Synchronized project' })).toBeVisible();
	await expect(indexPage.getByText('Visible in every project view.', { exact: true })).toBeVisible();
	const projectCards = indexPage.locator('[data-projects-scroll] a[href^="/projects/"]');
	await expect(projectCards).toHaveCount(2);
	await expect(projectCards.first().getByRole('heading', { name: 'Synchronized project' })).toBeVisible();
	await expect(indexPage.locator('aside').first().getByText('Synchronized project', { exact: true })).toBeVisible();
	await expect(indexPage.locator('aside').first().getByText('Éclair project', { exact: true })).toBeVisible();
	await expect(indexPage.locator('aside').first().getByText('Browser collaboration project', { exact: true })).toHaveCount(0);
	await expect(indexPage.locator('aside').first().locator(`a[href="/projects/${projectId}"]`)).toContainText('Synchronized project');
	await expect(newWorkProject).toHaveText('Synchronized project');
	await expect(syncProject.getByRole('heading', { name: 'Synchronized project' })).toBeVisible();
	await expect(conversationProjectLink).toHaveText('Synchronized project');

	await otherPage.evaluate(async (id) => {
		const response = await fetch(`/api/projects/${id}`, {
			method: 'PATCH',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				name: 'Authoritative project',
				description: 'The final concurrent update.',
			}),
		});
		const authoritativeProject = await response.json();
		const channel = new BroadcastChannel('xpressclaw:project-mutations:v1');
		channel.postMessage({ mutation: { kind: 'updated', project: authoritativeProject } });
		channel.postMessage({
			mutation: {
				kind: 'updated',
				project: {
					...authoritativeProject,
					name: 'Stale concurrent project',
					description: 'This delayed response must not win.',
				},
			},
		});
		await new Promise((resolve) => setTimeout(resolve, 0));
		channel.close();
	}, projectId);

	await expect(panes.getByRole('heading', { name: 'Authoritative project' })).toHaveCount(2);
	await expect(otherPane.getByRole('heading', { name: 'Authoritative project' })).toBeVisible();
	await expect(indexPage.getByRole('heading', { name: 'Authoritative project' })).toBeVisible();
	await expect(indexPage.locator('aside').first().getByText('Authoritative project', { exact: true })).toBeVisible();
	await expect(newWorkProject).toHaveText('Authoritative project');
	await expect(syncProject.getByRole('heading', { name: 'Authoritative project' })).toBeVisible();
	await expect(conversationProjectLink).toHaveText('Authoritative project');
	expect(context.pages()).toHaveLength(6);
	for (const openPage of context.pages()) {
		await expect(openPage.getByText('Stale concurrent project', { exact: true })).toHaveCount(0);
	}

	await panes.first().getByRole('button', { name: 'Project settings' }).click();
	dialog = page.getByRole('dialog');
	await dialog.getByLabel('Project name').fill('Unsaved local project name');
	await dialog.getByLabel('Description').fill('Keep this draft while remote updates arrive.');
	await otherPage.evaluate(async (id) => {
		const response = await fetch(`/api/projects/${id}`, {
			method: 'PATCH',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				name: 'Remote project rename',
				description: 'The remote update should not dismiss an active editor.',
			}),
		});
		const remoteProject = await response.json();
		const channel = new BroadcastChannel('xpressclaw:project-mutations:v1');
		channel.postMessage({ mutation: { kind: 'updated', project: remoteProject } });
		await new Promise((resolve) => setTimeout(resolve, 0));
		channel.close();
	}, projectId);

	await expect(dialog).toBeVisible();
	await expect(dialog.getByLabel('Project name')).toHaveValue('Unsaved local project name');
	await expect(dialog.getByLabel('Description')).toHaveValue('Keep this draft while remote updates arrive.');
	await expect(panes.getByRole('heading', { name: 'Remote project rename' })).toHaveCount(2);
	await expect(conversationProjectLink).toHaveText('Remote project rename');
	await dialog.getByRole('button', { name: 'Cancel' }).click();
	await conversationPage.close();

	await panes.first().getByRole('button', { name: 'Project settings' }).click();
	dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Delete project' }).click();
	await dialog.getByLabel('Type Remote project rename to confirm').fill('Remote project rename');
	await dialog.getByRole('button', { name: 'Permanently delete project' }).click();

	await expect.poll(() => projectDeleteRequests).toEqual([projectId]);
	await expect(page).toHaveURL(/\/projects$/);
	await expect(otherPage).toHaveURL(/\/projects$/);
	await expect(otherPage.getByRole('heading', { name: 'Projects' })).toBeVisible();
	await expect(indexPage.getByRole('heading', { name: 'Éclair project' })).toBeVisible();
	await expect(indexPage.getByRole('heading', { name: 'Create your first project' })).toHaveCount(0);
	await expect(indexPage.locator(`a[href="/projects/${projectId}"]`)).toHaveCount(0);
	await expect(newWorkProject).toHaveCount(0);
	await expect(syncProject).toHaveCount(0);
	await expect(syncPage.getByText('No Projects yet', { exact: true })).toBeVisible();
	await expect(page.locator(`[data-workspace-tab][data-workspace-tab-title="Synchronized project"]`)).toHaveCount(0);
	await expect(otherPage.locator('[data-workspace-tab-title="Synchronized project"], [data-workspace-tab-title="Browser collaboration project"]')).toHaveCount(0);
	await expect(otherPage.locator('[data-workspace-pane] [data-workspace-tab-title="Projects"]')).toBeVisible();

	const lateProjectGetRequests: string[] = [];
	const lateNewWorkPage = await context.newPage();
	await mockApi(lateNewWorkPage, {
		preserveWorkspace: true,
		projectGetRequests: lateProjectGetRequests,
		sharedProjectState,
	});
	await lateNewWorkPage.goto('/?_xpressclaw_window=workspace-12345-6');
	const lateNewWorkProject = lateNewWorkPage.getByLabel('Project').locator(`option[value="${projectId}"]`);
	await expect(lateNewWorkProject).toHaveCount(0);

	await lateNewWorkPage.evaluate(async ({ id, staleProject }) => {
		const modulePath = '/src/lib/projectEvents.ts';
		const projectEvents = await import(modulePath);
		projectEvents.publishProjectMutation({
			kind: 'updated',
			project: { ...staleProject, id, name: 'Delayed deleted project' },
		});
	}, { id: projectId, staleProject: sharedProjectState.project! });

	await expect.poll(() => lateProjectGetRequests).toContain(projectId);
	await expect(lateNewWorkProject).toHaveCount(0);
	await expect(newWorkProject).toHaveCount(0);
	await expect(syncProject).toHaveCount(0);
	await expect(indexPage.locator(`a[href="/projects/${projectId}"]`)).toHaveCount(0);
	await lateNewWorkPage.close();
	await syncPage.close();
	await newWorkPage.close();
	await indexPage.close();
	await otherPage.close();
});

test('project settings keep deletion errors visible and actionable', async ({ page }) => {
	const projectDeleteRequests: string[] = [];
	const projectDeleteError = 'Project work was cancelled, but Docker or Podman is unavailable. Start it and retry deletion.';
	await mockApi(page, {
		projectDeleteRequests,
		projectDeleteError,
		projectDeletionStartedAt: timestamp(60),
	});
	await page.goto(`/projects/${projectId}`);
	await expect(page.getByText('Deletion was interrupted after this Project stopped accepting new work.', { exact: false })).toBeVisible();

	await page.getByRole('button', { name: 'Project settings' }).click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Delete project' }).click();
	await expect(dialog.getByText('A previous deletion attempt was interrupted', { exact: false })).toBeVisible();
	await dialog.getByLabel('Type Browser collaboration project to confirm').fill('Browser collaboration project');
	await dialog.getByRole('button', { name: 'Permanently delete project' }).click();

	await expect.poll(() => projectDeleteRequests).toEqual([projectId]);
	await expect(dialog.getByRole('alert')).toHaveText(projectDeleteError);
	await expect(page).toHaveURL(new RegExp(`/projects/${projectId}$`));
	await expect(dialog.getByRole('button', { name: 'Permanently delete project' })).toBeEnabled();
});

test('project deletion shows progress, ignores duplicate confirmation, and opens the next project', async ({ page }) => {
	let releaseDelete!: () => void;
	const projectDeleteGate = new Promise<void>((resolve) => (releaseDelete = resolve));
	const projectDeleteRequests: string[] = [];
	await mockApi(page, {
		projectCount: 2,
		secondaryProjectName: 'Next project',
		projectDeleteRequests,
		projectDeleteGate,
	});
	await page.goto(`/projects/${projectId}`);

	await page.getByRole('button', { name: 'Project settings' }).click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Delete project' }).click();
	await dialog.getByLabel('Type Browser collaboration project to confirm').fill('Browser collaboration project');
	const deleteButton = dialog.getByRole('button', { name: 'Permanently delete project' });
	await deleteButton.evaluate((element) => {
		(element as HTMLButtonElement).click();
		(element as HTMLButtonElement).click();
	});

	await expect.poll(() => projectDeleteRequests).toEqual([projectId]);
	await expect(dialog.getByRole('status')).toHaveText('Cancelling active work and removing Project data…');
	await expect(dialog.getByRole('button', { name: 'Deleting…' })).toBeDisabled();

	releaseDelete();
	await expect(page).toHaveURL(/\/projects\/project-mobile-2$/);
});

test('opening a conversation reveals its newest messages after media loads', async ({ page }) => {
	const conversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Long release history',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(60),
		last_message_at: timestamp(60),
		participants: [
			{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
			{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(2) },
		],
	};
	const conversationMessages = Array.from({ length: 40 }, (_, index) => ({
		id: index + 1,
		conversation_id: conversationId,
		sender_type: index % 2 === 0 ? 'agent' : 'user',
		sender_id: index % 2 === 0 ? agentId : 'local',
		sender_name: index % 2 === 0 ? 'Browser-tested workspace' : 'You',
		content: `Conversation entry ${index + 1}. ${'This history is long enough to require scrolling. '.repeat(3)}`,
		message_type: 'message',
		linked_task_id: null,
		metadata: {},
		attachments: index === 39 ? [{
			id: 'delayed-history-image',
			message_id: index + 1,
			name: 'delayed-history.png',
			mime_type: 'image/png',
			size: 68,
			source_task_id: null,
			created_at: timestamp(index + 1),
		}] : [],
		created_at: timestamp(index + 1),
	}));
	await mockApi(page, { conversations: [conversation], conversationMessages });
	await page.route(`**/api/conversations/${conversationId}/attachments/delayed-history-image`, async (route) => {
		await new Promise((resolve) => setTimeout(resolve, 300));
		await route.fulfill({
			status: 200,
			contentType: 'image/png',
			body: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl2nH0AAAAASUVORK5CYII=', 'base64'),
		});
	});
	await page.goto(`/projects/${projectId}`);
	await page.locator(`[data-project-conversation="${conversationId}"]`).click();

	const messagePane = page.locator('[data-conversation-message-pane]');
	const delayedImage = page.getByAltText('delayed-history.png');
	await expect(messagePane).toBeVisible();
	await expect.poll(() => messagePane.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
	await expect.poll(() => delayedImage.evaluate((element) => {
		const image = element as HTMLImageElement;
		return image.complete && image.naturalHeight > 0;
	})).toBe(true);
	await expect.poll(() => messagePane.evaluate((element) => (
		element.scrollHeight - element.clientHeight - element.scrollTop
	))).toBeLessThan(2);
	await expect.poll(() => messagePane.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
});

test('running agents can be guided at a safe break or interrupted immediately', async ({ page }) => {
	const postedMessages: Record<string, unknown>[] = [];
	const interruptedAttempts: string[] = [];
	await mockApi(page, { live: true, postedMessages, interruptedAttempts });
	await page.goto(`/tasks/${taskId}`);

	const composer = page.locator(`#task-message-input-${taskId}`);
	await expect(page.getByRole('button', { name: 'Interrupt agent now' })).toBeVisible();
	await composer.fill('Use the smaller API instead');
	await page.getByRole('button', { name: 'Send message' }).click();
	await expect.poll(() => postedMessages.length).toBe(1);
	expect(postedMessages[0].delivery).toBe('after_tool');

	await composer.fill('Stop and apply this correction now');
	await page.getByRole('button', { name: 'Interrupt and send now' }).click();
	await expect.poll(() => postedMessages.length).toBe(2);
	expect(postedMessages[1].delivery).toBe('immediate');

	await page.getByRole('button', { name: 'Interrupt agent now' }).click();
	await expect.poll(() => interruptedAttempts).toEqual(['attempt-browser-test']);
});

test('Codex plugin install elicitations can be accepted', async ({ page }) => {
	const elicitationResponses: { elicitationId: string; payload: Record<string, unknown> }[] = [];
	await mockApi(page, {
		pendingElicitation: {
			mode: 'form',
			message: 'The Stripe plugin can verify coupon and promotion-code configuration.',
			requestedSchema: {
				type: 'object',
				properties: {},
			},
			_meta: {
				codex_approval_kind: 'tool_suggestion',
				persist: 'always',
				tool_type: 'plugin',
				suggest_type: 'install',
				suggest_reason: 'The Stripe plugin can verify coupon and promotion-code configuration.',
				tool_id: 'stripe@openai-curated-remote',
				tool_name: 'Stripe',
				remote_plugin_id: 'stripe',
				app_connector_ids: ['stripe'],
			},
		},
		elicitationResponses,
	});
	await page.goto(`/tasks/${taskId}`);

	const request = page.locator('[data-tool-install-elicitation]');
	await expect(request).toBeVisible();
	await expect(request).toContainText('Plugin installation requested');
	await expect(request).toContainText('Install Stripe?');
	await expect(request).toContainText('The Stripe plugin can verify coupon and promotion-code configuration.');
	await expect(page.locator('[data-unsupported-elicitation]')).toHaveCount(0);
	await expect(page.locator(`#task-message-input-${taskId}`)).toBeDisabled();
	await request.getByRole('button', { name: 'Install plugin' }).click();
	await expect.poll(() => elicitationResponses).toEqual([{
		elicitationId: 'elicitation-browser-test',
		payload: {
			action: 'accept',
			content: {},
			message: 'Approved installing the Stripe plugin.',
		},
	}]);
	await expect(request).toHaveCount(0);
});

test('Codex connector install elicitations can be declined', async ({ page }) => {
	const elicitationResponses: { elicitationId: string; payload: Record<string, unknown> }[] = [];
	await mockApi(page, {
		pendingElicitation: {
			mode: 'form',
			message: 'Connect GitHub to inspect pull requests.',
			requestedSchema: {
				type: 'object',
				properties: {},
			},
			_meta: {
				codex_approval_kind: 'tool_suggestion',
				persist: 'always',
				tool_type: 'connector',
				suggest_type: 'install',
				suggest_reason: 'Connect GitHub to inspect pull requests.',
				tool_id: 'github',
				tool_name: 'GitHub',
			},
		},
		elicitationResponses,
	});
	await page.goto(`/tasks/${taskId}`);

	const request = page.locator('[data-tool-install-elicitation]');
	await expect(request).toContainText('Connector installation requested');
	await expect(request.getByRole('button', { name: 'Install connector' })).toBeVisible();
	await request.getByRole('button', { name: 'Not now' }).click();
	await expect.poll(() => elicitationResponses).toEqual([{
		elicitationId: 'elicitation-browser-test',
		payload: { action: 'decline' },
	}]);
	await expect(request).toHaveCount(0);
});

test('unsupported ACP elicitations stay visible and actionable', async ({ page }) => {
	const elicitationResponses: { elicitationId: string; payload: Record<string, unknown> }[] = [];
	const interruptedAttempts: string[] = [];
	const postedMessages: Record<string, unknown>[] = [];
	await mockApi(page, {
		pendingElicitation: {
			mode: 'form',
			message: 'Install the Figma plugin to continue?',
			requestedSchema: {
				type: 'object',
				properties: {
					plugin: { type: 'string', oneOf: [null] },
				},
			},
		},
		elicitationResponses,
		interruptedAttempts,
		postedMessages,
	});
	await page.goto(`/tasks/${taskId}`);

	const fallback = page.locator('[data-unsupported-elicitation]');
	await expect(fallback).toBeVisible();
	await expect(fallback).toContainText('Install the Figma plugin to continue?');
	await expect(fallback).toContainText('cannot display safely');
	await expect(fallback.getByRole('button', { name: 'Cancel request' })).toBeVisible();
	await expect(fallback.getByRole('button', { name: 'Decline request' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Start' })).toHaveCount(0);

	// Stopping with an empty composer remains a pure interruption. It does not
	// turn the elicitation into a task message or silently enqueue more work.
	await page.getByRole('button', { name: 'Interrupt agent now' }).click();
	await expect.poll(() => interruptedAttempts).toEqual(['attempt-browser-test']);
	expect(postedMessages).toEqual([]);

	await fallback.getByRole('button', { name: 'Decline request' }).click();
	await expect.poll(() => elicitationResponses).toEqual([{
		elicitationId: 'elicitation-browser-test',
		payload: { action: 'decline' },
	}]);
	await expect(fallback).toHaveCount(0);
});

test('a normal reply can replace an unsupported elicitation', async ({ page }) => {
	const postedMessages: Record<string, unknown>[] = [];
	await mockApi(page, {
		pendingElicitation: {
			mode: '_codex_plugin_install',
			message: 'Install a plugin to continue?',
		},
		postedMessages,
	});
	await page.goto(`/tasks/${taskId}`);

	const composer = page.locator(`#task-message-input-${taskId}`);
	await expect(composer).toBeEnabled();
	await expect(composer).toHaveAttribute('placeholder', 'Reply to continue with different guidance...');
	await composer.fill('Continue without installing the plugin.');
	await page.getByRole('button', { name: 'Send message' }).click();
	await expect.poll(() => postedMessages).toHaveLength(1);
	expect(postedMessages[0]).toMatchObject({
		content: 'Continue without installing the plugin.',
		delivery: 'after_tool',
	});
});

test('supported ACP forms still require structured answers', async ({ page }) => {
	const elicitationResponses: { elicitationId: string; payload: Record<string, unknown> }[] = [];
	await mockApi(page, {
		pendingElicitation: {
			mode: 'form',
			message: 'Choose an approach',
			requestedSchema: {
				type: 'object',
				properties: {
					approach: {
						type: 'string',
						title: 'Approach',
						description: 'Which approach should the agent use?',
						oneOf: [{ const: 'safe', title: 'Safe approach' }],
					},
				},
			},
		},
		elicitationResponses,
	});
	await page.goto(`/tasks/${taskId}`);

	await expect(page.locator('[data-unsupported-elicitation]')).toHaveCount(0);
	await expect(page.locator('[data-approval-card]')).toBeVisible();
	await expect(page.getByText('Agent question')).toBeVisible();
	await expect(page.locator(`#task-message-input-${taskId}`)).toBeDisabled();
	await page.getByRole('button', { name: 'Safe approach' }).click();
	await page.getByRole('button', { name: 'Review' }).click();
	await page.getByRole('button', { name: 'Send answers' }).click();
	await expect.poll(() => elicitationResponses).toEqual([{
		elicitationId: 'elicitation-browser-test',
		payload: expect.objectContaining({
			action: 'accept',
			content: { approach: 'safe' },
		}),
	}]);
});

test('task drafts survive reloads and clear after a successful send', async ({ page }) => {
	const postedMessages: Record<string, unknown>[] = [];
	await mockApi(page, { postedMessages });
	await page.goto(`/tasks/${taskId}`);

	const composer = page.locator(`#task-message-input-${taskId}`);
	await composer.fill('Keep this draft through a reload');
	await page.reload();
	await expect(composer).toHaveValue('Keep this draft through a reload');

	await page.getByRole('button', { name: 'Send message' }).click();
	await expect.poll(() => postedMessages.length).toBe(1);
	await expect(composer).toHaveValue('');
	await page.reload();
	await expect(composer).toHaveValue('');
});

test('new-work drafts restore the agent they were written for', async ({ page }) => {
	const queuedSessionMessages: { agentId: string; payload: Record<string, unknown> }[] = [];
	await mockApi(page, { multipleAgents: true, queuedSessionMessages });
	await page.goto('/');

	const composer = page.getByPlaceholder('Describe the outcome you want…');
	const projectPicker = page.getByLabel('Agent', { exact: true });
	await expect(composer).toBeVisible();
	await expect(page.locator('[data-new-work-composer]').getByLabel('Project', { exact: true })).toHaveCount(0);
	await expect(page.locator('[data-new-work-context]').getByLabel('Project', { exact: true })).toBeVisible();
	await projectPicker.selectOption('project-secondary-test');
	await composer.fill('Keep this work with the secondary project');
	await page.reload();

	await expect(projectPicker).toHaveValue('project-secondary-test');
	await expect(composer).toHaveValue('Keep this work with the secondary project');
	await composer.press('Enter');
	await expect.poll(() => queuedSessionMessages.length).toBe(1);
	expect(queuedSessionMessages[0].agentId).toBe('project-secondary-test');
	expect(queuedSessionMessages[0].payload.content).toBe('Keep this work with the secondary project');
});

test('new work binds reusable workflow agent roles across projects', async ({ page }) => {
	const queuedSessionMessages: { agentId: string; payload: Record<string, unknown> }[] = [];
	const workflowRunRequests: { id: string; inputs: Record<string, unknown>; projectId?: string }[] = [];
	await mockApi(page, {
		multipleAgents: true,
		queuedSessionMessages,
		workflowRunRequests,
		workflows: [{
			id: 'workflow-review-loop',
			name: 'Code Review Loop',
			description: 'Implement, review, and repeat until approved.',
			yaml_content: `name: code-review-loop
inputs:
  goal:
    type: string
    required: true
  prompt_fragment:
    type: string
    required: true
  implementer:
    type: agent
    required: true
    primary: true
  reviewer:
    type: agent
    required: true
  retries:
    type: number
    required: true
  review_context:
    type: json
    required: true
  notify:
    type: boolean
    default: false
flows:
  main:
    steps:
      - id: implement
        agent: "@implementer"
        prompt: "Implement @goal"
      - id: review
        agent: "@reviewer"
        prompt: "Review @goal"
`,
			enabled: true,
			version: 1,
			created_at: timestamp(1),
			updated_at: timestamp(2),
			last_triggered_at: null,
			trigger_count: 0,
			trigger_error: null,
		}],
	});
	await page.goto('/');

	await expect(page.getByRole('button', { name: 'Agent mode' })).toHaveAttribute('aria-pressed', 'true');
	await page.getByRole('button', { name: 'Workflow mode' }).click();
	await expect(page.getByRole('button', { name: 'Workflow mode' })).toHaveAttribute('aria-pressed', 'true');
	const agentPicker = page.getByLabel('Agent role implementer');
	const workflowPicker = page.getByLabel('Workflow', { exact: true });
	await expect(workflowPicker).toHaveValue('workflow-review-loop');
	await expect(workflowPicker.locator('option')).toHaveText(['Code Review Loop']);

	await expect(page.getByLabel('Agent role reviewer')).toHaveValue('project-secondary-test');
	await agentPicker.selectOption('project-secondary-test');
	await expect(workflowPicker).toHaveValue('workflow-review-loop');
	await expect(workflowPicker.locator('option')).toHaveText(['Code Review Loop']);

	await agentPicker.selectOption(agentId);
	await page.getByLabel('Agent role reviewer').selectOption('project-secondary-test');
	await page.getByPlaceholder('Describe the outcome you want…').fill('Add workflow selection to New Work');
	await page.getByLabel('Workflow input prompt_fragment').fill('  Keep this spacing.  ');
	await page.getByLabel('Workflow input retries').fill('3');
	await page.getByLabel('Workflow input review_context').fill('{"focus":"accessibility"}');
	await page.getByLabel('Workflow input notify').selectOption('true');
	await page.reload();
	await expect(page.getByRole('button', { name: 'Workflow mode' })).toHaveAttribute('aria-pressed', 'true');
	await expect(agentPicker).toHaveValue(agentId);
	await expect(workflowPicker).toHaveValue('workflow-review-loop');
	await expect(page.getByPlaceholder('Describe the outcome you want…')).toHaveValue('Add workflow selection to New Work');
	await expect(page.getByLabel('Workflow input prompt_fragment')).toHaveValue('  Keep this spacing.  ');
	await expect(page.getByLabel('Workflow input retries')).toHaveValue('3');
	await expect(page.getByLabel('Workflow input review_context')).toHaveValue('{"focus":"accessibility"}');
	await expect(page.getByLabel('Workflow input notify')).toHaveValue('true');
	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	await expectVerticalScroll(page.locator('[data-new-work-scroll]'));
	await expect(page.locator('[data-new-work-context]').getByLabel('Project', { exact: true })).toBeVisible();
	await page.getByPlaceholder('Describe the outcome you want…').press('Enter');

	await expect.poll(() => workflowRunRequests).toEqual([{
		id: 'workflow-review-loop',
		projectId,
		inputs: {
			goal: 'Add workflow selection to New Work',
			prompt_fragment: '  Keep this spacing.  ',
			implementer: agentId,
			reviewer: 'project-secondary-test',
			retries: 3,
			review_context: { focus: 'accessibility' },
			notify: true,
		},
	}]);
	expect(queuedSessionMessages).toEqual([]);
	await expect(page).toHaveURL(`/tasks/${taskId}`);
});

test('new work opens the task created by a conversation workflow', async ({ page }) => {
	const conversationTaskRequests: Record<string, unknown>[] = [];
	await mockApi(page, {
		conversations: [{
			id: conversationId,
			project_id: projectId,
			title: 'Release planning',
			icon: null,
			created_at: timestamp(1),
			updated_at: timestamp(2),
			last_message_at: timestamp(2),
			participants: [
				{ participant_type: 'user', participant_id: 'local', joined_at: timestamp(1) },
				{ participant_type: 'agent', participant_id: agentId, joined_at: timestamp(1) },
			],
		}],
		conversationTaskRequests,
		workflows: [{
			id: 'workflow-review-loop',
			name: 'Code Review Loop',
			description: 'Implement and review linked Conversation work.',
			yaml_content: `name: code-review-loop
inputs:
  goal:
    type: string
    required: true
flows:
  main:
    steps:
      - id: implement
        agent: "${agentId}"
        prompt: "Implement @goal"
`,
			enabled: true,
			version: 1,
			created_at: timestamp(1),
			updated_at: timestamp(2),
			last_triggered_at: null,
			trigger_count: 0,
			trigger_error: null,
		}],
	});
	await page.goto('/');

	await page.getByLabel('Conversation', { exact: true }).selectOption(conversationId);
	await page.getByRole('button', { name: 'Workflow mode' }).click();
	await page.getByLabel('Workflow', { exact: true }).selectOption('workflow-review-loop');
	await page.getByPlaceholder('Describe the outcome you want…').fill('Review the release changes');
	await page.getByPlaceholder('Describe the outcome you want…').press('Enter');

	await expect.poll(() => conversationTaskRequests).toEqual([{
		title: 'Review the release changes',
		workflow_id: 'workflow-review-loop',
		workflow_inputs: { goal: 'Review the release changes' },
	}]);
	await expect(page).toHaveURL(`/tasks/${taskId}`);
});

test('agent Work shows only the five most recently updated tasks', async ({ page }) => {
	await mockApi(page, { projectTaskUpdates: [10, 70, 30, 90, 50, 110, 20] });
	const recentRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks' && url.searchParams.get('sort') === 'recent';
	});
	await page.goto(`/agents/${agentId}`);

	const recentUrl = new URL((await recentRequest).url());
	expect(recentUrl.searchParams.get('agent_id')).toBe(agentId);
	expect(recentUrl.searchParams.get('limit')).toBe('5');
	expect(recentUrl.searchParams.get('exclude_statuses')).toBe('waiting_for_input,blocked');
	const workItems = page.locator('[data-project-work-list] [data-project-work-item]');
	await expect(workItems).toHaveCount(5);
	expect(await workItems.evaluateAll((items) => items.map((item) => item.getAttribute('href')))).toEqual([
		'/tasks/project-task-5',
		'/tasks/project-task-3',
		'/tasks/project-task-1',
		'/tasks/project-task-4',
		'/tasks/project-task-2',
	]);
	await expect(page.getByRole('link', { name: 'All tasks' })).toHaveAttribute('href', '/tasks');
});

test('agent Work reports the pinned Codex presentation capability', async ({ page }) => {
	await mockApi(page, {
		runnerReadiness: {
			ready: true,
			kind: 'codex',
			presentation_artifacts: {
				supported: true,
				available: true,
				capability: 'xpressclaw-pptx-v1',
				runtime: 'PptxGenJS 4.0.1',
				reason: null,
			},
		},
	});
	await page.goto(`/agents/${agentId}`);

	const readiness = page.locator('[data-presentation-readiness]');
	await expect(readiness).toContainText('PowerPoint artifacts ready');
	await expect(readiness).toContainText('PptxGenJS 4.0.1 is pinned in this runner');
});

test('agent files browse Git changes and save Monaco edits with a revision', async ({ page }) => {
	const workspaceSaveRequests: { path: string; content: string; expected_revision: string }[] = [];
	await mockApi(page, { workspaceSaveRequests });
	await page.goto(`/agents/${agentId}?tab=files&path=src%2Fmain.ts`);

	await expect(page.locator('[data-workspace-files]')).toBeVisible();
	await expect(page.getByText('feature/workspace-browser · 2 changed')).toBeVisible();
	await expect(page.getByRole('button', { name: 'src/main.ts' })).toBeVisible();
	await expect(page.locator('[data-monaco-editor]')).toBeVisible({ timeout: 20_000 });
	await expect(page.getByText('Loading editor…')).toBeHidden({ timeout: 20_000 });

	const editor = page.locator('[data-monaco-editor]');
	await editor.locator('.view-lines').click();
	await page.keyboard.press('Control+A');
	await page.keyboard.type('export const greeting = "edited";');
	const saveButton = page.getByRole('button', { name: 'Save' });
	await expect(saveButton).toBeEnabled();
	await saveButton.click();

	await expect.poll(() => workspaceSaveRequests).toEqual([{
		path: 'src/main.ts',
		content: 'export const greeting = "edited";',
		expected_revision: 'revision-before-save',
	}]);
	await expect(page.getByText('Saved')).toBeVisible();

	await page.getByRole('button', { name: 'Diff' }).click();
	await expect(page.locator('[data-monaco-editor]')).toBeVisible();
	const tree = page.locator('[data-workspace-tree]');
	await tree.locator('button[title="src"]').click();
	await expect(tree.locator('button[title="src/main.ts"]')).toBeVisible();

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(tree).toBeVisible();
	await expect(page.locator('[data-monaco-editor]')).toBeVisible();
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('workspace save preserves edits typed while the request is in flight', async ({ page }) => {
	const workspaceSaveRequests: { path: string; content: string; expected_revision: string }[] = [];
	await mockApi(page, { workspaceSaveRequests, workspaceSaveDelayMs: 400 });
	await page.goto(`/agents/${agentId}?tab=files&path=src%2Fmain.ts`);

	const editor = page.locator('[data-monaco-editor]');
	await expect(editor).toBeVisible({ timeout: 20_000 });
	await expect(page.getByText('Loading editor…')).toBeHidden({ timeout: 20_000 });
	await editor.locator('.view-lines').click();
	await page.keyboard.press('Control+A');
	await page.keyboard.insertText('export const greeting = "submitted";');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect.poll(() => workspaceSaveRequests.length).toBe(1);

	await editor.locator('.view-lines').click();
	await page.keyboard.press('Control+End');
	await page.keyboard.insertText('\nexport const typedDuringSave = true;');
	await expect(page.getByText('Saved')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Save' })).toBeEnabled();

	await page.getByRole('button', { name: 'Save' }).click();
	await expect.poll(() => workspaceSaveRequests).toHaveLength(2);
	expect(workspaceSaveRequests[0]).toEqual({
		path: 'src/main.ts',
		content: 'export const greeting = "submitted";',
		expected_revision: 'revision-before-save',
	});
	expect(workspaceSaveRequests[1]).toEqual({
		path: 'src/main.ts',
		content: 'export const greeting = "submitted";\nexport const typedDuringSave = true;',
		expected_revision: 'revision-after-save',
	});
});

test('deleted workspace changes remain available as diff-only selections', async ({ page }) => {
	await mockApi(page, { includeDeletedWorkspaceFile: true });
	await page.goto(`/agents/${agentId}?tab=files`);

	await expect(page.getByText('feature/workspace-browser · 3 changed')).toBeVisible();
	await page.getByRole('button', { name: 'src/removed.ts' }).click();
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fremoved.ts`);
	await expect(page.getByRole('button', { name: 'Code' })).toBeDisabled();
	await expect(page.locator('[data-monaco-editor]')).toContainText('deleted file mode', { timeout: 20_000 });
	await expect(page.getByText('workspace path was not found')).toBeHidden();
});

test('task changed files open beside the task with the file tree collapsed', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	const changedFiles = page.locator('[data-task-changed-files]');
	await expect(changedFiles.getByRole('heading', { name: 'Changed files' })).toBeVisible();
	await expect(changedFiles.getByRole('link', { name: 'src/main.ts' })).toHaveAttribute(
		'href',
		`/agents/${agentId}?tab=files&path=src%2Fmain.ts`,
	);
	await changedFiles.getByRole('link', { name: 'src/main.ts' }).click();
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts&tree=collapsed`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(2);
	await expect(page.locator(`#task-message-input-${taskId}`)).toBeVisible();
	await expect(page.locator('[data-task-details-sidebar]')).toBeVisible();
	await expect(page.locator('[data-workspace-tree]')).toHaveCount(0);
	await expect(page.locator('[data-monaco-editor]')).toBeVisible({ timeout: 20_000 });
	await expect(page.getByText('Loading editor…')).toBeHidden({ timeout: 20_000 });
	await page.getByRole('button', { name: 'Show files' }).click();
	await expect(page.locator('[data-workspace-tree]')).toBeVisible();
});

test('keyboard-activated task files open beside their source pane', async ({ page }) => {
	await page.setViewportSize({ width: 1800, height: 900 });
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	await page.getByRole('button', { name: 'Split active tab right' }).click();
	const panes = page.locator('[data-workspace-pane]');
	await expect(panes).toHaveCount(2);

	const changedFile = panes.first().locator('[data-task-changed-files]').getByRole('link', { name: 'src/main.ts' });
	await changedFile.focus();
	await page.keyboard.press('Enter');

	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts&tree=collapsed`);
	await expect(panes).toHaveCount(3);
	await expect(panes.nth(0).locator(`#task-message-input-${taskId}`)).toBeVisible();
	await expect(panes.nth(1).locator('[data-monaco-editor]')).toBeVisible({ timeout: 20_000 });
	await expect(panes.nth(2).locator(`#task-message-input-${taskId}`)).toBeVisible();
});

test('task changed files use normal navigation when the workspace is too narrow to split', async ({ page }) => {
	await page.setViewportSize({ width: 1024, height: 768 });
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	const changedFile = page.locator('[data-task-changed-files]').getByRole('link', { name: 'src/main.ts' });
	await expect(changedFile).toBeVisible();
	await changedFile.click();

	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts&tree=collapsed`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(1);
	await expect(page.locator('[data-monaco-editor]')).toBeVisible({ timeout: 20_000 });
	await expect(page.getByRole('button', { name: 'Show files' })).toBeVisible();
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('task changed-file modified clicks retain normal link behavior', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	const changedFile = page.locator('[data-task-changed-files]').getByRole('link', { name: 'src/main.ts' });
	const preserved = await changedFile.evaluate((element) => [
		{ ctrlKey: true },
		{ metaKey: true },
		{ shiftKey: true },
		{ altKey: true },
	].every((modifiers) => {
		let defaultPreventedByHandler = true;
		const observeThenSuppressNavigation = (event: MouseEvent) => {
			defaultPreventedByHandler = event.defaultPrevented;
			event.preventDefault();
		};
		document.addEventListener('click', observeThenSuppressNavigation, { once: true });
		element.dispatchEvent(new MouseEvent('click', {
			bubbles: true,
			cancelable: true,
			...modifiers,
		}));
		return !defaultPreventedByHandler;
	}));

	expect(preserved).toBe(true);
	await expect(page).toHaveURL(`/tasks/${taskId}`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(1);
});

test('task changed files use normal navigation at the workspace pane limit', async ({ page }) => {
	const workspaceReadRequests: string[] = [];
	const workspaceReadDelaysMs: Record<string, number> = {};
	await page.setViewportSize({ width: 2200, height: 1000 });
	await mockApi(page, { workspaceReadRequests, workspaceReadDelaysMs });
	await page.goto(`/tasks/${taskId}`);

	for (let paneCount = 2; paneCount <= 4; paneCount += 1) {
		await page.getByRole('button', { name: 'Split active tab right' }).last().click();
		await expect(page.locator('[data-workspace-pane]')).toHaveCount(paneCount);
	}
	await expect(page.getByRole('button', { name: 'Split active tab right' }).last()).toBeDisabled();

	const focusedPane = page.locator('[data-workspace-pane]').first();
	const changedFile = focusedPane.locator('[data-task-changed-files]').getByRole('link', { name: 'src/main.ts' });
	await changedFile.focus();
	await page.keyboard.press('Enter');

	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts&tree=collapsed`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(4);
	await expect(focusedPane.locator('[data-monaco-editor]')).toBeVisible({ timeout: 20_000 });
	await expect(page.locator('[data-workspace-pane]').last().locator(`#task-message-input-${taskId}`)).toBeVisible();
	await focusedPane.getByRole('button', { name: 'Show files' }).click();
	await expect(focusedPane.locator('[data-workspace-tree]')).toBeVisible();

	const editor = focusedPane.locator('[data-monaco-editor]');
	await editor.locator('.view-lines').click({ position: { x: 20, y: 20 } });
	await page.keyboard.press('Control+A');
	await page.keyboard.insertText('export const unsaved = true;');
	await expect(focusedPane.getByRole('button', { name: 'Save' })).toBeEnabled();

	const requestFile = (path?: string) => page.evaluate(({ requestedAgentId, requestedPath }) => {
		window.dispatchEvent(new CustomEvent('xpressclaw:workspace-open-split', {
			detail: {
				path: `/agents/${requestedAgentId}?tab=files${requestedPath ? `&path=${encodeURIComponent(requestedPath)}` : ''}&tree=collapsed`,
			},
		}));
	}, { requestedAgentId: agentId, requestedPath: path });

	page.once('dialog', (dialog) => void dialog.dismiss());
	await requestFile('README.md');
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts`);
	await expect(focusedPane.locator('[data-workspace-files] span[title="src/main.ts"]')).toBeVisible();
	await expect(focusedPane.locator('[data-workspace-tree]')).toBeVisible();
	await expect(focusedPane.getByRole('button', { name: 'Save' })).toBeEnabled();

	page.once('dialog', (dialog) => void dialog.accept());
	await requestFile('README.md');

	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=README.md&tree=collapsed`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(4);
	await expect(focusedPane.locator('[data-workspace-files] span[title="README.md"]')).toBeVisible();
	await expect(focusedPane.locator('[data-workspace-tree]')).toHaveCount(0);

	workspaceReadDelaysMs['src/main.ts'] = 500;
	const mainReadCount = workspaceReadRequests.filter((path) => path === 'src/main.ts').length;
	await requestFile('src/main.ts');
	await expect.poll(() => workspaceReadRequests.filter((path) => path === 'src/main.ts').length).toBe(mainReadCount + 1);
	await focusedPane.getByRole('button', { name: 'Show files' }).click();
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=src%2Fmain.ts`);
	await expect(focusedPane.locator('[data-workspace-files] span[title="src/main.ts"]')).toBeVisible();
	await expect(focusedPane.locator('[data-workspace-tree]')).toBeVisible();

	workspaceReadDelaysMs['README.md'] = 500;
	const readmeReadCount = workspaceReadRequests.filter((path) => path === 'README.md').length;
	await requestFile('README.md');
	await expect.poll(() => workspaceReadRequests.filter((path) => path === 'README.md').length).toBe(readmeReadCount + 1);
	await requestFile('docs/guide.md');
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&path=docs%2Fguide.md&tree=collapsed`);
	await expect(focusedPane.locator('[data-workspace-files] span[title="docs/guide.md"]')).toBeVisible();
	await page.waitForTimeout(600);
	await expect(focusedPane.locator('[data-workspace-files] span[title="docs/guide.md"]')).toBeVisible();

	await requestFile();
	await expect(page).toHaveURL(`/agents/${agentId}?tab=files&tree=collapsed`);
	await expect(focusedPane.getByText('Choose a file to browse or edit it with Monaco.')).toBeVisible();
	await expect(focusedPane.locator('[data-monaco-editor]')).toHaveCount(0);
});

test('narrow manual task splits preserve the compact task layout', async ({ page }) => {
	await page.setViewportSize({ width: 1024, height: 768 });
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	await page.getByRole('button', { name: 'Split active tab right' }).click();
	const panes = page.locator('[data-workspace-pane]');
	await expect(panes).toHaveCount(2);
	await expect(panes.locator('[data-task-details-sidebar]')).toHaveCount(2);
	await expect(panes.locator('[data-task-details-sidebar]').first()).toBeHidden();
	await expect(panes.locator('[data-task-details-sidebar]').last()).toBeHidden();
	await expect(panes.locator(`#task-message-input-${taskId}`)).toHaveCount(2);

	const transcriptWidths = await panes.locator('[data-task-transcript-scroll]').evaluateAll((elements) =>
		elements.map((element) => element.getBoundingClientRect().width),
	);
	expect(transcriptWidths.every((width) => width >= 350)).toBe(true);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('ambiguous cloned repositories require an explicit durable selection', async ({ page }) => {
	const repositorySelections: (string | null)[] = [];
	await mockApi(page, {
		repositorySelections,
		repositoryStatus: {
			state: 'ambiguous',
			message: 'Multiple repositories were found. Select the one this Agent should use.',
			bootstrap_root: '/srv/workspaces/browser-tested',
			active: null,
			candidates: [
				{ relative_path: 'alpha', root: '/srv/workspaces/browser-tested/alpha', github_repository: 'example/alpha' },
				{ relative_path: 'product', root: '/srv/workspaces/browser-tested/product', github_repository: 'XpressAI/product' },
			],
			discovery_truncated: false,
			selected_relative_path: null,
			pending_relative_path: null,
			pending_action: null,
			github_status: 'unavailable',
			github_repository: null,
			restart_required: false,
		},
	});
	await page.goto(`/agents/${agentId}?tab=workspace`);

	const card = page.locator('[data-active-repository]');
	await expect(card.getByText('Multiple repositories were found. Select the one this Agent should use.')).toBeVisible();
	await card.getByText('product', { exact: true }).locator('..').locator('..').getByRole('button', { name: 'Use' }).click();
	await expect.poll(() => repositorySelections).toEqual(['product']);
	await expect(card.getByText('The repository change is pending and will be applied at the next safe turn boundary.')).toBeVisible();
	await expect(card.getByText('The current turn keeps its existing process. The next turn starts a fresh session with the pending repository change.')).toBeVisible();
});

test('clearing an active repository is queued for a safe turn boundary', async ({ page }) => {
	const repositorySelections: (string | null)[] = [];
	await mockApi(page, { repositorySelections });
	await page.goto(`/agents/${agentId}?tab=workspace`);

	const card = page.locator('[data-active-repository]');
	await card.getByRole('button', { name: 'Clear active repository' }).click();
	await expect.poll(() => repositorySelections).toEqual([null]);
	await expect(card.getByText('The repository change is pending and will be applied at the next safe turn boundary.')).toBeVisible();
});

test('active repository reports a missing GitHub credential without hiding Git access', async ({ page }) => {
	await mockApi(page, {
		repositoryStatus: {
			state: 'attached',
			message: 'The active GitHub repository has no matching connector, GH_TOKEN, or host gh credential.',
			bootstrap_root: '/srv/workspaces/browser-tested',
			active: { relative_path: 'product', root: '/srv/workspaces/browser-tested/product', github_repository: 'XpressAI/product' },
			candidates: [
				{ relative_path: 'product', root: '/srv/workspaces/browser-tested/product', github_repository: 'XpressAI/product' },
			],
			discovery_truncated: false,
			selected_relative_path: 'product',
			pending_relative_path: null,
			pending_action: null,
			github_status: 'missing_credential',
			github_repository: 'XpressAI/product',
			restart_required: false,
		},
	});
	await page.goto(`/agents/${agentId}?tab=workspace`);

	const card = page.locator('[data-active-repository]');
	await expect(card.getByText('GitHub credential missing')).toBeVisible();
	await expect(card.getByText('XpressAI/product').first()).toBeVisible();
	await expect(card.getByText('The active GitHub repository has no matching connector, GH_TOKEN, or host gh credential.')).toBeVisible();
	await expect(card.getByText('/srv/workspaces/browser-tested/product')).toBeVisible();
});

test('DeepSeek Harness is preserved in Agent runner settings', async ({ page }) => {
	await mockApi(page, { agentRunnerKind: 'deepseek-harness' });
	await page.goto(`/agents/${agentId}?tab=runner`);

	await expect(page.getByRole('heading', { name: 'Browser-tested workspace' })).toBeVisible();
	await expect(page.locator('#runner-kind')).toHaveValue('deepseek-harness');
	await expect(page.locator('#runner-image')).toHaveValue(
		'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest',
	);
	await page.getByLabel('Host Docker or Podman access').check();
	await expect(page.locator('#runner-image')).toHaveValue(
		'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness-docker:latest',
	);
});

test('custom runner kinds containing a built-in name stay custom in Agent settings', async ({ page }) => {
	await mockApi(page, { agentRunnerKind: 'codex-proxy' });
	await page.goto(`/agents/${agentId}?tab=runner`);

	await expect(page.getByRole('heading', { name: 'Browser-tested workspace' })).toBeVisible();
	await expect(page.locator('#runner-kind')).toHaveValue('codex-proxy');
	await expect(page.locator('#runner-kind').locator('option:checked')).toHaveText(
		'Other ACP harness (codex-proxy)',
	);
	await expect(page.locator('#runner-image')).toHaveValue('xpressclaw-runner-codex-proxy:latest');
});

test('mobile connection recovery stays non-blocking and does not reload the workspace', async ({ page }) => {
	const connection = { online: true };
	await page.setViewportSize({ width: 390, height: 844 });
	await mockApi(page, { connection });
	await page.goto(`/tasks/${taskId}`);

	const composer = page.locator(`#task-message-input-${taskId}`);
	await composer.fill('Do not lose this mobile draft');
	await page.evaluate(() => ((window as typeof window & { disconnectTestMarker?: boolean }).disconnectTestMarker = true));
	connection.online = false;

	const connectionStatus = page.locator('[data-connection-status]');
	await expect(connectionStatus).toBeVisible({ timeout: 20_000 });
	await composer.fill('I can keep typing while disconnected');
	await expect(composer).toHaveValue('I can keep typing while disconnected');

	connection.online = true;
	await expect(connectionStatus).toBeHidden({ timeout: 5_000 });
	expect(await page.evaluate(() => (window as typeof window & { disconnectTestMarker?: boolean }).disconnectTestMarker)).toBe(true);
	await expect(composer).toHaveValue('I can keep typing while disconnected');
});

test('workspace panes split on wide screens and collapse cleanly on mobile', async ({ page, browser }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(1);
	await expect(page.locator('[data-workspace-pane] .scrollbar-hide').first()).toHaveCSS('scrollbar-width', 'none');
	await page.getByRole('button', { name: 'Split active tab right' }).click();
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(2);
	await expect(page.locator(`#task-message-input-${taskId}`)).toHaveCount(2);

	await page.locator('aside a[href="/settings"]').click();
	await expect(page).toHaveURL('/settings');
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	await expect(page.getByText('Connections', { exact: true })).toHaveCount(0);

	const mobile = await browser.newPage({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
	await mockApi(mobile, { live: true });
	await mobile.goto(`/tasks/${taskId}`);
	await expect(mobile.locator('[data-workspace-pane]:visible')).toHaveCount(1);
	await expect(mobile.getByRole('button', { name: 'Interrupt agent now' })).toBeVisible();
	expect(await mobile.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);

	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	await mobile.locator('aside:visible').getByRole('button', { name: 'Close' }).click();
	await mobile.locator('nav a[href="/projects"]').click();
	await expect(mobile).toHaveURL('/projects');
	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	let mobileProject = mobile.locator(`aside a[href="/agents/${agentId}"]:visible`);
	await mobileProject.click();
	await expect(mobile).toHaveURL(`/agents/${agentId}`);
	await mobile.getByRole('tab', { name: 'Automations' }).click();
	await expect(mobile).toHaveURL(`/agents/${agentId}?tab=schedules`);

	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	mobileProject = mobile.locator(`aside a[href="/agents/${agentId}"]:visible`);
	await mobileProject.evaluate((element) => element.dispatchEvent(new MouseEvent('contextmenu', {
		bubbles: true,
		cancelable: true,
		button: 2,
		clientX: 380,
		clientY: 830,
	})));
	const projectMenu = mobile.getByRole('menu', { name: 'Browser-tested workspace actions' });
	await expect(projectMenu).toBeVisible();
	const mobileViewport = await mobile.evaluate(() => ({ width: window.innerWidth, height: window.innerHeight }));
	await expect.poll(async () => {
		const bounds = await projectMenu.boundingBox();
		return bounds ? bounds.y + bounds.height : Number.POSITIVE_INFINITY;
	}).toBeLessThanOrEqual(mobileViewport.height);
	const menuBounds = await projectMenu.boundingBox();
	expect(menuBounds).not.toBeNull();
	expect(menuBounds!.x).toBeGreaterThanOrEqual(0);
	expect(menuBounds!.y).toBeGreaterThanOrEqual(0);
	expect(menuBounds!.x + menuBounds!.width).toBeLessThanOrEqual(mobileViewport.width);
	expect(menuBounds!.y + menuBounds!.height).toBeLessThanOrEqual(mobileViewport.height);
	await projectMenu.getByRole('menuitem', { name: 'Open Environment' }).click();
	await expect(mobile).toHaveURL(`/agents/${agentId}?tab=workspace`);

	const mobileTabs = mobile.locator('[data-workspace-tab]:visible');
	await expect(mobileTabs).toHaveCount(3);
	await mobileTabs.nth(1).evaluate((element) => element.dispatchEvent(new MouseEvent('contextmenu', {
		bubbles: true,
		cancelable: true,
		button: 2,
		clientX: 380,
		clientY: 20,
	})));
	await mobile.getByRole('menuitem', { name: 'Close Other Tabs' }).click();
	await expect(mobileTabs).toHaveCount(1);
	expect(await mobile.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	await mobile.close();
});

test('workspace keeps the ten most recently used tabs and clearly marks the active tab', async ({ page }) => {
	const conversation = {
		id: conversationId,
		project_id: projectId,
		title: 'Release planning',
		icon: null,
		created_at: timestamp(1),
		updated_at: timestamp(2),
		last_message_at: timestamp(2),
		participants: [],
	};
	const workflow = {
		id: 'workflow-limit-test',
		name: 'Limit test workflow',
		description: 'Exercises the workspace tab limit.',
		yaml_content: 'name: limit-test\nflows:\n  main:\n    steps: []\n',
		enabled: true,
		version: 1,
		created_at: timestamp(1),
		updated_at: timestamp(2),
	};
	await page.addInitScript(({ storageKey, state }) => {
		if (!localStorage.getItem(storageKey)) localStorage.setItem(storageKey, JSON.stringify(state));
	}, {
		storageKey: 'xpressclaw.workspace.v1',
		state: {
			focusedPaneId: 'seed-pane',
			panes: [{
				id: 'seed-pane',
				activeTabId: 'seed-tab-10',
				width: 1,
				tabs: [
					{ id: 'seed-tab-1', path: '/', kind: 'home', title: 'New work', resourceId: null, status: null, lastActiveAt: 1 },
					{ id: 'seed-tab-2', path: '/projects', kind: 'projects', title: 'Projects', resourceId: null, status: null, lastActiveAt: 2 },
					{ id: 'seed-tab-3', path: `/projects/${projectId}`, kind: 'project', title: 'Browser collaboration project', resourceId: projectId, status: null, lastActiveAt: 3 },
					{ id: 'seed-tab-4', path: `/conversations/${conversationId}`, kind: 'conversation', title: 'Release planning', resourceId: conversationId, status: null, lastActiveAt: 4 },
					{ id: 'seed-tab-5', path: '/agents', kind: 'agents', title: 'Agents', resourceId: null, status: null, lastActiveAt: 5 },
					{ id: 'seed-tab-6', path: `/agents/${agentId}`, kind: 'agent', title: 'Browser-tested workspace', resourceId: agentId, status: null, lastActiveAt: 6 },
					{ id: 'seed-tab-7', path: '/tasks', kind: 'tasks', title: 'Tasks', resourceId: null, status: null, lastActiveAt: 7 },
					{ id: 'seed-tab-8', path: `/tasks/${taskId}`, kind: 'task', title: 'Browser-tested workspace', resourceId: taskId, status: null, lastActiveAt: 8 },
					{ id: 'seed-tab-9', path: '/workflows/new', kind: 'workflow-new', title: 'New workflow', resourceId: null, status: null, lastActiveAt: 9 },
					{ id: 'seed-tab-10', path: '/workflows/workflow-limit-test', kind: 'workflow', title: 'Limit test workflow', resourceId: 'workflow-limit-test', status: null, lastActiveAt: 10 },
				],
			}],
		},
	});
	await mockApi(page, {
		preserveWorkspace: true,
		conversations: [conversation],
		workflows: [workflow],
	});
	await page.goto('/workflows/workflow-limit-test');

	const tabs = page.locator('[data-workspace-pane] [data-workspace-tab]');
	await expect(tabs).toHaveCount(10);
	await tabs.filter({ has: page.locator('button[title="New work"]') }).locator('button').first().click();
	await expect(page).toHaveURL('/');

	await page.locator('aside').first().locator('a[href="/settings"]').click();
	await expect(page).toHaveURL('/settings');
	await expect(tabs).toHaveCount(10);
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-title="New work"]')).toHaveCount(1);
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-title="Projects"]')).toHaveCount(0);

	const activeTab = page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-active="true"]');
	const inactiveTab = page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-active="false"]').first();
	await expect(activeTab).toHaveCount(1);
	await expect(activeTab).toHaveAttribute('data-workspace-tab-title', 'Settings');
	await expect(activeTab.locator('button').first()).toHaveAttribute('aria-current', 'page');
	await expect(activeTab.locator('[data-active-tab-indicator]')).toBeVisible();
	const tabStrip = page.locator('[data-workspace-pane] [data-workspace-tab-strip]');
	await expect.poll(async () => {
		const [stripBounds, activeBounds] = await Promise.all([tabStrip.boundingBox(), activeTab.boundingBox()]);
		return Boolean(stripBounds && activeBounds
			&& activeBounds.x >= stripBounds.x
			&& activeBounds.x + activeBounds.width <= stripBounds.x + stripBounds.width + 1);
	}).toBe(true);
	const [activeStyle, inactiveStyle] = await Promise.all([
		activeTab.evaluate((element) => ({
			background: getComputedStyle(element).backgroundColor,
			color: getComputedStyle(element).color,
		})),
		inactiveTab.evaluate((element) => ({
			background: getComputedStyle(element).backgroundColor,
			color: getComputedStyle(element).color,
		})),
	]);
	expect(activeStyle).not.toEqual(inactiveStyle);

	const persistedTabs = await page.evaluate(() => {
		const state = JSON.parse(localStorage.getItem('xpressclaw.workspace.v1') ?? '{}') as {
			panes?: { tabs?: { lastActiveAt?: number }[] }[];
		};
		return state.panes?.flatMap((pane) => pane.tabs ?? []) ?? [];
	});
	expect(persistedTabs).toHaveLength(10);
	expect(persistedTabs.every((tab) => Number.isFinite(tab.lastActiveAt))).toBe(true);

	await page.reload();
	await expect(tabs).toHaveCount(10);
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-title="New work"]')).toHaveCount(1);
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab][data-workspace-tab-title="Projects"]')).toHaveCount(0);
});

test('workspace renders when session storage is unavailable', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'sessionStorage', {
			configurable: true,
			get: () => {
				throw new DOMException('Storage access is disabled.', 'SecurityError');
			},
		});
	});
	await mockApi(page);
	await page.goto(`/tasks/${taskId}?_xpressclaw_window=workspace-123-1`);

	await expect(page).toHaveURL(`/tasks/${taskId}`);
	await expect(page.locator('[data-workspace-pane]')).toHaveCount(1);
	await expect(page.locator('[data-workspace-pane] [data-workspace-tab]')).toHaveCount(1);
	await expect.poll(() => page.evaluate(() => localStorage.getItem('xpressclaw.workspace.v1') !== null)).toBe(true);
});

test('agent context menus open sections and separate windows', async ({ page }) => {
	await mockApi(page);
	await page.goto('/');

	const projectLink = page.locator('aside').first().locator(`a[href="/agents/${agentId}"]`);
	await projectLink.click({ button: 'right' });
	const projectMenu = page.getByRole('menu', { name: 'Browser-tested workspace actions' });
	await expect(projectMenu).toBeVisible();
	await expect(projectMenu.getByRole('menuitem')).toHaveText([
		'Open in New Window',
		'Open Tasks',
		'Open Automations',
		'Open Harness',
		'Open Environment',
	]);
	await projectMenu.getByRole('menuitem', { name: 'Open Automations' }).click();
	await expect(page).toHaveURL(`/agents/${agentId}?tab=schedules`);
	await expect(page.getByRole('tab', { name: 'Automations' })).toHaveAttribute('aria-selected', 'true');

	await projectLink.click({ button: 'right' });
	const popupPromise = page.waitForEvent('popup');
	await page.getByRole('menuitem', { name: 'Open in New Window' }).click();
	const popup = await popupPromise;
	await expect.poll(() => new URL(popup.url()).pathname).toBe(`/agents/${agentId}`);
	await expect(popup.locator('[data-workspace-pane] [data-workspace-tab]')).toHaveCount(1);
	await popup.close();
});

test('tab context menus close one, other, or all tabs within a pane', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);
	const sidebar = page.locator('aside').first();
	const tabs = page.locator('[data-workspace-pane] [data-workspace-tab]');

	await sidebar.locator('a[href="/projects"]').click();
	await sidebar.locator('a[href="/settings"]').click();
	await expect(tabs).toHaveCount(3);

	await tabs.filter({ has: page.locator('[title="Projects"]') }).click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Close Other Tabs' }).click();
	await expect(tabs).toHaveCount(1);
	await expect(tabs).toHaveAttribute('data-workspace-tab-title', 'Projects');
	await expect(page).toHaveURL('/projects');

	await tabs.click({ button: 'right' });
	await expect(page.getByRole('menuitem', { name: 'Close Other Tabs' })).toBeDisabled();
	await page.keyboard.press('Escape');

	await sidebar.locator('a[href="/settings"]').click();
	await tabs.filter({ has: page.locator('[title="Settings"]') }).click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Close Tab', exact: true }).click();
	await expect(tabs).toHaveCount(1);
	await expect(page).toHaveURL('/projects');

	await sidebar.locator('a[href="/settings"]').click();
	await tabs.filter({ has: page.locator('[title="Settings"]') }).click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Close All Tabs' }).click();
	await expect(tabs).toHaveCount(1);
	await expect(tabs).toHaveAttribute('data-workspace-tab-title', 'New work');
	await expect(page).toHaveURL('/');
});

test('tab context menus open isolated browser windows', async ({ page }) => {
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	await page.locator('[data-workspace-pane] [data-workspace-tab]').click({ button: 'right' });
	const popupPromise = page.waitForEvent('popup');
	await page.getByRole('menuitem', { name: 'Open in New Window' }).click();
	const popup = await popupPromise;
	await expect.poll(() => new URL(popup.url()).pathname).toBe(`/tasks/${taskId}`);
	await expect(popup.locator('[data-workspace-pane] [data-workspace-tab]')).toHaveCount(1);
	await popup.close();
});

test('tab context menus create native webview windows in the desktop app', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'isTauri', { value: true });
		(window as unknown as { __workspaceWindowCalls: unknown[] }).__workspaceWindowCalls = [];
		(window as unknown as {
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		}).__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'matched', local: true };
				}
				(window as unknown as { __workspaceWindowCalls: unknown[] }).__workspaceWindowCalls.push({ command, args });
				return null;
			},
		};
	});
	await mockApi(page);
	await page.goto(`/tasks/${taskId}`);

	await page.locator('[data-workspace-pane] [data-workspace-tab]').click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Open in New Window' }).click();
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __workspaceWindowCalls: unknown[] }
	).__workspaceWindowCalls.length)).toBe(1);
	const call = await page.evaluate(() => (
		window as unknown as { __workspaceWindowCalls: { command: string; args: { options: { label: string; url: string } } }[] }
	).__workspaceWindowCalls[0]);
	expect(call.command).toBe('plugin:webview|create_webview_window');
	expect(call.args.options.label).toMatch(/^workspace-/);
	expect(new URL(call.args.options.url).pathname).toBe(`/tasks/${taskId}`);
	expect(new URL(call.args.options.url).searchParams.get('_xpressclaw_window')).toBe(call.args.options.label);
});

test('task pages show five recent tasks per project in the sidebar', async ({ page }) => {
	await mockApi(page, { multipleAgents: true });
	const sidebarTasks = [
		{ id: 'primary-oldest', title: 'Primary oldest', agentId, updatedAt: 10, status: 'completed' },
		{ id: 'primary-recent', title: 'Primary recent', agentId, updatedAt: 80, status: 'waiting_for_input' },
		{ id: 'secondary-older', title: 'Secondary older', agentId: 'project-secondary-test', updatedAt: 20, status: 'completed' },
		{ id: taskId, title: 'Current task', agentId, updatedAt: 50, status: 'in_progress' },
		{ id: 'primary-newest', title: 'Primary newest', agentId, updatedAt: 100, status: 'completed' },
		{ id: 'primary-middle', title: 'Primary middle', agentId, updatedAt: 60, status: 'completed' },
		{ id: 'secondary-newest', title: 'Secondary newest', agentId: 'project-secondary-test', updatedAt: 70, status: 'pending' },
		{ id: 'primary-second', title: 'Primary second', agentId, updatedAt: 90, status: 'completed' },
	];
	const recentRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks/recent-by-agent';
	});
	await page.route('**/api/tasks**', async (route) => {
		const url = new URL(route.request().url());
		if (!['/api/tasks', '/api/tasks/recent-by-agent'].includes(url.pathname) || url.searchParams.has('parent_task_id')) {
			await route.fallback();
			return;
		}
		const allTasks = sidebarTasks.map((sidebarTask, index) => ({
			id: sidebarTask.id,
			title: sidebarTask.title,
			description: null,
			status: sidebarTask.status,
			priority: 0,
			agent_id: sidebarTask.agentId,
			parent_task_id: null,
			sop_id: null,
			created_at: timestamp(index),
			updated_at: timestamp(sidebarTask.updatedAt),
			completed_at: sidebarTask.status === 'completed' ? timestamp(sidebarTask.updatedAt) : null,
			context: {},
			depends_on: [],
			dependents: [],
			blocked_by: [],
			ready: true,
		}));
		const tasks = url.pathname === '/api/tasks/recent-by-agent'
			? allTasks.filter((listedTask) => listedTask.id !== 'primary-oldest')
			: allTasks.filter((listedTask) => [taskId, 'primary-oldest', 'secondary-older'].includes(listedTask.id));
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				tasks,
				counts: { pending: 1, in_progress: 1, waiting_for_input: 1, blocked: 0, completed: 5, cancelled: 0 },
			}),
		});
	});
	await page.goto(`/tasks/${taskId}`);
	const recentUrl = new URL((await recentRequest).url());
	expect(recentUrl.searchParams.get('limit')).toBe('5');

	const sidebar = page.locator('aside').first();
	const taskSidebar = sidebar.locator('[data-sidebar-mode="tasks"]');
	await expect(taskSidebar).toBeVisible();

	const projectGroup = taskSidebar.locator(`[data-sidebar-project-group="${projectId}"]`);
	await expect(projectGroup.getByRole('heading', { name: 'Browser collaboration project' })).toBeVisible();
	const projectTasks = projectGroup.locator('[data-sidebar-task]');
	await expect(projectTasks).toHaveCount(5);
	expect(await projectTasks.evaluateAll((items) => items.map((item) => item.getAttribute('href')))).toEqual([
		'/tasks/primary-newest',
		'/tasks/primary-second',
		'/tasks/primary-recent',
		'/tasks/secondary-newest',
		'/tasks/primary-middle',
	]);
	await expect(projectGroup.locator(`a[href="/tasks/${taskId}"]`)).toHaveCount(0);

	await sidebar.locator('a[href="/projects"]').click();
	await expect(sidebar.locator('[data-sidebar-mode="tasks"]')).toHaveCount(0);
	await expect(sidebar.locator(`a[href="/agents/${agentId}"]`)).toBeVisible();
});

test('task list scrolls and requests one filtered page at a time', async ({ page }) => {
	await mockApi(page, { completedTaskCount: 45 });
	await page.goto('/tasks');

	const doneFilter = page.getByRole('button', { name: 'Done 45' });
	await doneFilter.click();

	const taskScroller = page.locator('[data-tasks-scroll]');
	const taskRows = page.locator('[data-task-list] [data-task-row]');
	await expect(taskScroller).toHaveCSS('overflow-y', 'auto');
	await expect(taskRows).toHaveCount(20);
	await expect(page.getByText('Page 1 of 3')).toBeVisible();
	await expect.poll(() => taskScroller.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);

	const secondPageRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks'
			&& url.searchParams.get('statuses') === 'completed,cancelled'
			&& url.searchParams.get('offset') === '20';
	});
	await page.getByRole('button', { name: 'Next' }).click();
	const secondPageUrl = new URL((await secondPageRequest).url());
	expect(secondPageUrl.searchParams.get('limit')).toBe('20');
	await expect(taskRows).toHaveCount(20);
	await expect(page.getByText('Page 2 of 3')).toBeVisible();

	const finalPageRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks'
			&& url.searchParams.get('statuses') === 'completed,cancelled'
			&& url.searchParams.get('offset') === '40';
	});
	await page.getByRole('button', { name: 'Next' }).click();
	await finalPageRequest;
	await expect(taskRows).toHaveCount(5);
	await expect(page.getByText('Page 3 of 3')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Next' })).toBeDisabled();
});

test('idle completed turns do not look active and future plan items are deferred', async ({ page }) => {
	await mockApi(page, {
		taskStatus: 'in_progress',
		taskActivityStatus: 'idle',
		taskSubtasks: [{
			id: 'future-plan-step',
			title: 'Address any further review feedback through approval or merge',
			description: null,
			status: 'cancelled',
			priority: -1,
			agent_id: null,
			parent_task_id: taskId,
			sop_id: null,
			conversation_id: null,
			created_at: timestamp(10),
			updated_at: timestamp(61),
			completed_at: null,
			context: { origin: 'native_plan', plan_disposition: 'deferred' },
			provenance: 'native_plan',
			blocks_parent: false,
			activity_status: 'cancelled',
		}],
	});

	await page.goto('/tasks');
	const row = page.locator(`[data-task-row][href="/tasks/${taskId}"]`);
	await expect(row).toHaveAttribute('data-task-activity-status', 'idle');
	await expect(row.getByText('Not running')).toBeVisible();
	await expect(row.getByText('Working')).toHaveCount(0);

	await row.click();
	await expect(page.locator('[data-task-activity-status="idle"]').first()).toBeVisible();
	await expect(page.locator('[data-agent-loading]')).toHaveCount(0);
	await expect(page.getByText('No worker or required subtask is currently running.')).toBeVisible();
	await expect(page.getByText('Deferred · does not block completion')).toBeVisible();
});

test('task search filters the full history with server-side counts', async ({ page }) => {
	await mockApi(page, { completedTaskCount: 45 });
	await page.goto('/tasks');
	await page.getByRole('button', { name: 'Done 45' }).click();

	const searchRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks' && url.searchParams.get('search') === 'COMPLETED 42';
	});
	await page.getByRole('searchbox', { name: 'Search tasks' }).fill('COMPLETED 42');
	const searchUrl = new URL((await searchRequest).url());
	expect(searchUrl.searchParams.get('statuses')).toBe('completed,cancelled');
	expect(searchUrl.searchParams.get('sort')).toBe('recent');

	await expect(page.locator('[data-task-list] [data-task-row]')).toHaveCount(1);
	await expect(page.locator('[data-task-list] [data-task-row]')).toHaveAttribute('href', '/tasks/completed-task-42');
	await expect(page.getByText('1 matching task')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Done 1' })).toBeVisible();

	await page.getByRole('button', { name: 'Clear task search' }).click();
	await expect(page.getByRole('button', { name: 'Done 45' })).toBeVisible();
	await expect(page.locator('[data-task-list] [data-task-row]')).toHaveCount(20);
});

test('task search waits for Japanese IME composition to finish', async ({ page }) => {
	await mockApi(page, {
		taskTitle: '日本語の検索を確認',
		taskDescription: '入力メソッドで見つけるタスク',
	});
	const searches: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.pathname === '/api/tasks' && url.searchParams.has('search')) {
			searches.push(url.searchParams.get('search') ?? '');
		}
	});
	await page.goto('/tasks');
	await page.getByRole('button', { name: 'Done 1' }).click();
	const search = page.getByRole('searchbox', { name: 'Search tasks' });

	await search.dispatchEvent('compositionstart', { data: '' });
	await search.evaluate((element) => {
		const input = element as HTMLInputElement;
		input.value = 'けんさく';
		input.dispatchEvent(new InputEvent('input', {
			bubbles: true,
			data: 'けんさく',
			inputType: 'insertCompositionText',
			isComposing: true,
		}));
	});
	await search.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 229, isComposing: true });
	await page.waitForTimeout(350);
	expect(searches).toEqual([]);

	const searchRequest = page.waitForRequest((request) => {
		const url = new URL(request.url());
		return url.pathname === '/api/tasks' && url.searchParams.get('search') === '検索';
	});
	await search.evaluate((element) => {
		const input = element as HTMLInputElement;
		input.value = '検索';
		input.dispatchEvent(new InputEvent('input', {
			bubbles: true,
			data: '検索',
			inputType: 'insertCompositionText',
			isComposing: true,
		}));
		input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '検索' }));
		input.dispatchEvent(new InputEvent('input', {
			bubbles: true,
			data: '検索',
			inputType: 'insertText',
			isComposing: false,
		}));
	});
	await searchRequest;
	const resultRows = page.locator('[data-task-list] [data-task-row]');
	await expect(resultRows).toHaveCount(1);
	await expect(resultRows).toContainText('日本語の検索を確認');
	expect(searches).toEqual(['検索']);
});

test('agent, task, and automation lists remain scrollable on mobile', async ({ browser }) => {
	const mobile = await browser.newPage({
		viewport: { width: 390, height: 844 },
		isMobile: true,
		hasTouch: true,
	});
	const mobileWorkflows = Array.from({ length: 30 }, (_, index) => ({
		id: `workflow-mobile-${index + 1}`,
		name: `Mobile workflow ${index + 1}`,
		description: `Workflow ${index + 1} verifies the mobile list can scroll.`,
		yaml_content: 'flows: {}',
		enabled: index % 2 === 0,
		version: 1,
		created_at: timestamp(index),
		updated_at: timestamp(index),
	}));
	await mockApi(mobile, {
		completedTaskCount: 45,
		projectCount: 30,
		workflows: mobileWorkflows,
	});

	await mobile.goto('/agents');
	await expect(mobile.locator('[data-projects-scroll] [data-project-card]')).toHaveCount(30);
	await expect(mobile.locator('[data-project-card]').first()).toContainText('Codex · xpressclaw');
	await expectVerticalScroll(mobile.locator('[data-projects-scroll]'));
	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	await expectVerticalScroll(mobile.locator('aside:visible [data-mobile-sidebar-scroll]'));
	await mobile.locator('aside:visible').getByRole('button', { name: 'Close' }).click();

	await mobile.goto('/tasks');
	await mobile.getByRole('button', { name: 'Done 45' }).click();
	await expect(mobile.locator('[data-task-list] [data-task-row]')).toHaveCount(20);
	await expectVerticalScroll(mobile.locator('[data-tasks-scroll]'));
	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	await expectVerticalScroll(mobile.locator('aside:visible [data-mobile-sidebar-scroll]'));
	await mobile.locator('aside:visible').getByRole('button', { name: 'Close' }).click();

	await mobile.goto('/automations');
	await expect(mobile.locator('[data-workflows-scroll] [data-workflow-card]')).toHaveCount(30);
	await expectVerticalScroll(mobile.locator('[data-workflows-scroll]'));
	await mobile.getByRole('button', { name: 'Open agent switcher' }).click();
	await expectVerticalScroll(mobile.locator('aside:visible [data-mobile-sidebar-scroll]'));

	expect(await mobile.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	await mobile.close();
});

test('MCP verification reports authentication and runner executable failures in context', async ({ page }) => {
	const verificationRequests: { name: string; agent_id: string | null }[] = [];
	await mockApi(page, {
		mcpServers: [
			{
				name: 'JIRA',
				type: 'http',
				command: null,
				args: [],
				url: 'https://mcp.atlassian.com/v1/mcp/authv2',
				env: {},
				headers: {},
			},
			{
				name: 'playwright',
				type: 'stdio',
				command: '/usr/bin/npx',
				args: ['playwright', 'run-test-mcp-server'],
				url: null,
				env: {},
				headers: {},
			},
		],
		mcpVerificationRequests: verificationRequests,
		mcpVerificationResults: {
			JIRA: {
				ok: false,
				status: 'authentication_required',
				message: 'The MCP endpoint responded with 401 Unauthorized; authentication is required.',
				suggestion: 'Add a valid Authorization header, then verify again.',
			},
			playwright: {
				ok: false,
				status: 'command_path_incorrect',
				message: "/usr/bin/npx is not executable in this project's runner image.",
				suggestion: 'Use /usr/local/bin/npx instead.',
			},
		},
	});

	await page.goto('/settings/mcp');
	const jira = page.locator('[data-mcp-server="JIRA"]');
	await jira.getByRole('button', { name: 'Verify' }).click();
	await expect(jira).toContainText('authentication is required');

	await page.goto(`/agents/${agentId}?tab=runner`);
	const playwright = page.locator('[data-mcp-server="playwright"]');
	await playwright.getByRole('button', { name: 'Verify' }).click();
	await expect(playwright).toContainText('Use /usr/local/bin/npx instead.');

	expect(verificationRequests).toEqual([
		{ name: 'JIRA', agent_id: null },
		{ name: 'playwright', agent_id: agentId },
	]);

	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('skills and MCP settings surface official discovery documentation', async ({ page }) => {
	const openUrlRequests: string[] = [];
	await mockApi(page, { mcpServers: [], openUrlRequests, agentKind: { value: 'claude' } });

	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/settings/mcp');
	await expect(page.getByRole('button', { name: 'Official MCP Registry' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Add server' })).toBeVisible();
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	await expectExternalPopup(page, 'Official MCP Registry', 'https://registry.modelcontextprotocol.io/');
	await expect(page.getByText('Find published packages and remote endpoints')).toBeVisible();

	await page.goto(`/agents/${agentId}?tab=runner`);
	await expectExternalPopup(page, 'Registry', 'https://registry.modelcontextprotocol.io/');
	await expectExternalPopup(page, 'Claude Agent skills guide', 'https://code.claude.com/docs/en/skills');

	expect(openUrlRequests).toEqual([]);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('Harness discovery links use the identity-checked opener for remote Desktop profiles', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'isTauri', { value: true });
		(window as unknown as { __externalOpenCalls: { command: string; args: Record<string, unknown> }[] }).__externalOpenCalls = [];
		(window as unknown as {
			__TAURI_INTERNALS__: { invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };
		}).__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: Record<string, unknown>) => {
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'matched', navigation_status: 'ready', local: false };
				}
				(window as unknown as { __externalOpenCalls: { command: string; args: Record<string, unknown> }[] }).__externalOpenCalls.push({ command, args });
				return null;
			},
		};
	});
	await mockApi(page, { mcpServers: [] });
	await page.goto(`/agents/${agentId}?tab=runner`);
	await page.getByRole('button', { name: 'Registry', exact: true }).click();

	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __externalOpenCalls: { command: string; args: Record<string, unknown> }[] }
	).__externalOpenCalls)).toEqual([{
		command: 'open_external_url',
		args: { url: 'https://registry.modelcontextprotocol.io/' },
	}]);
});

test('every supported harness resolves the intended documentation and custom stays neutral', async ({ page }) => {
	const agentKind = { value: 'codex' };
	const catalog = [
		['codex', 'Codex', 'https://developers.openai.com/codex/cli/'],
		['claude', 'Claude Agent', 'https://docs.anthropic.com/en/docs/claude-code/setup'],
		['github-copilot', 'GitHub Copilot', 'https://docs.github.com/en/copilot/github-copilot-in-the-cli'],
		['opencode', 'OpenCode', 'https://opencode.ai/docs/'],
		['qwen', 'Qwen Code', 'https://qwenlm.github.io/qwen-code-docs/'],
		['cline', 'Cline', 'https://docs.cline.bot/'],
		['cursor', 'Cursor', 'https://docs.cursor.com/agent/overview'],
	].map(([kind, name, install_url]) => ({
		kind,
		name,
		install_url,
		image: `ghcr.io/xpressai/xpressclaw-runner-${kind}:latest`,
		host_image: `ghcr.io/xpressai/xpressclaw-runner-${kind}-docker:latest`,
	}));
	await mockApi(page, { agentCatalog: catalog, agentKind });

	const expected = [
		['codex', 'Codex skills guide', 'https://learn.chatgpt.com/docs/build-skills'],
		['claude', 'Claude Agent skills guide', 'https://code.claude.com/docs/en/skills'],
		['github-copilot', 'GitHub Copilot skills guide', 'https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-skills'],
		['opencode', 'OpenCode skills guide', 'https://opencode.ai/docs/skills/'],
		['qwen', 'Qwen Code skills guide', 'https://qwenlm.github.io/qwen-code-docs/en/users/features/skills/'],
		['cline', 'Cline skills guide', 'https://docs.cline.bot/customization/skills'],
		['cursor', 'Cursor docs', 'https://docs.cursor.com/agent/overview'],
	] as const;

	for (const [kind, buttonName, url] of expected) {
		agentKind.value = kind;
		await page.goto(`/agents/${agentId}?tab=runner`);
		await expect(page.locator('#runner-kind')).toHaveValue(kind);
		await expectExternalPopup(page, buttonName, url);
	}

	agentKind.value = 'custom';
	await page.goto(`/agents/${agentId}?tab=runner`);
	await expect(page.locator('#runner-kind')).toHaveValue('custom');
	await expect(page.getByRole('button', { name: /(?:skills guide|docs)$/ })).toHaveCount(0);
});

test('automation and settings pages show context-specific sidebar lists', async ({ page }) => {
	await mockApi(page, {
		workflows: [
			{
				id: 'workflow-older',
				name: 'Nightly maintenance',
				description: null,
				yaml_content: 'flows: {}',
				enabled: false,
				version: 1,
				created_at: timestamp(1),
				updated_at: timestamp(10),
			},
			{
				id: 'workflow-newer',
				name: 'Review pull requests',
				description: null,
				yaml_content: 'flows: {}',
				enabled: true,
				version: 2,
				created_at: timestamp(2),
				updated_at: timestamp(20),
			},
		],
		schedules: [{
			id: 'schedule-nightly',
			name: 'Nightly task sweep',
			cron: '0 2 * * *',
			agent_id: agentId,
			title: 'Sweep queued tasks',
			description: null,
			enabled: true,
			last_run: null,
			run_count: 0,
			created_at: timestamp(30),
			schedule_type: 'cron',
			run_at: null,
			continuation_task_id: null,
			conversation_id: null,
		}],
	});
	await page.goto('/automations');

	const sidebar = page.locator('aside').first();
	const automationSidebar = sidebar.locator('[data-sidebar-mode="automations"]');
	await expect(automationSidebar).toBeVisible();
	expect(await automationSidebar.locator('[data-sidebar-workflow]').evaluateAll((items) =>
		items.map((item) => item.getAttribute('href'))
	)).toEqual(['/workflows/workflow-newer', '/workflows/workflow-older']);
	await expect(automationSidebar.locator('[data-sidebar-schedule]')).toContainText('Nightly task sweep');
	await expect(sidebar.locator(`a[href="/agents/${agentId}"]`)).toHaveCount(0);
	await expect(page.locator('[data-automations-scroll]')).toHaveCSS('overflow-y', 'auto');
	await expect(page.locator('[data-workflow-card]')).toHaveCount(2);
	await expect(page.locator('[data-schedule-card]')).toHaveCount(1);

	await sidebar.locator('a[href="/settings"]').click();
	await expect(page).toHaveURL('/settings');
	const settingsSidebar = sidebar.locator('[data-sidebar-mode="settings"]');
	await expect(settingsSidebar).toBeVisible();
	await expect(settingsSidebar.locator('[data-sidebar-setting]')).toHaveText([
		'P Profile',
		'↕ Project sync',
		'C Local collaboration',
		'M MCP servers',
		'I Instance',
	]);
	await expect(settingsSidebar.locator('[data-sidebar-setting="settings"]')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	await settingsSidebar.locator('a[href="/settings/mcp"]').click();
	await expect(page).toHaveURL('/settings/mcp');
	await expect(settingsSidebar.locator('[data-sidebar-setting="settings-mcp"]')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	await expect(sidebar.locator(`a[href="/agents/${agentId}"]`)).toHaveCount(0);

	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/automations');
	await page.getByRole('button', { name: 'Open agent switcher' }).click();
	await expect(page.locator('aside:visible [data-sidebar-mode="automations"] [data-sidebar-workflow]')).toHaveCount(2);
	await expect(page.locator('aside:visible [data-sidebar-mode="automations"] [data-sidebar-schedule]')).toHaveCount(1);
	await page.locator('aside:visible').getByRole('button', { name: 'Close' }).click();
	await page.locator('nav a[href="/settings"]:visible').click();
	await page.getByRole('button', { name: 'Open agent switcher' }).click();
	await expect(page.locator('aside:visible [data-sidebar-mode="settings"] [data-sidebar-setting]')).toHaveCount(5);
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('the new-schedule sidebar shortcut remains reusable after cancellation and creation', async ({ page }) => {
	await mockApi(page);
	await page.goto('/automations');

	const sidebar = page.locator('aside').first();
	const shortcut = sidebar.getByTitle('New schedule');
	await shortcut.click();
	await expect(page.locator('[data-schedule-form]')).toBeVisible();
	await expect(page).toHaveURL('/automations?new=schedule#schedules');

	await page.locator('[data-schedule-form]').getByRole('button', { name: 'Cancel' }).click();
	await expect(page.locator('[data-schedule-form]')).toHaveCount(0);
	await expect(page).toHaveURL('/automations#schedules');

	await shortcut.click();
	const reopenedForm = page.locator('[data-schedule-form]');
	await expect(reopenedForm).toBeVisible();
	await expect(page).toHaveURL('/automations?new=schedule#schedules');
	await reopenedForm.getByLabel('Name').fill('Reusable shortcut');
	await reopenedForm.getByRole('textbox', { name: 'Cron', exact: true }).fill('0 9 * * 1');
	await reopenedForm.getByLabel('Task title').fill('Check the queue');
	await reopenedForm.getByRole('button', { name: 'Create schedule' }).click();
	await expect(reopenedForm).toHaveCount(0);
	await expect(page).toHaveURL('/automations#schedules');
});

test('workflow gallery has six primary templates, distinct selection, and unique default names', async ({ page }) => {
	const existing = (name: string, index: number) => ({
		id: `existing-workflow-${index}`,
		name,
		description: null,
		yaml_content: 'name: existing\nflows:\n  main:\n    steps: []',
		enabled: true,
		version: 1,
		created_at: timestamp(index),
		updated_at: timestamp(index),
		last_triggered_at: null,
		trigger_count: 0,
		trigger_error: null,
	});
	await mockApi(page, {
		workflows: [
			existing('Goal Loop', 1),
			existing('Goal Loop 2', 2),
			existing('UI Regression Test', 3),
		],
	});
	await page.goto('/workflows/new');

	const cards = page.locator('[data-workflow-template-card]');
	await expect(cards).toHaveCount(6);
	expect(await cards.evaluateAll((items) => items.map((item) => item.getAttribute('data-workflow-template-card')))).toEqual([
		'goal-loop',
		'code-review',
		'repository-caretaker',
		'backlog-processor',
		'requirements-specification',
		'ui-regression',
	]);
	await expect(cards.filter({ hasText: 'Goal loop' })).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByLabel('Name')).toHaveValue('Goal Loop 3');
	await expect(page.getByRole('button', { name: /Start blank/ })).not.toHaveAttribute('data-workflow-template-card');

	const reviewCard = cards.filter({ hasText: 'Implementation + independent review' });
	await reviewCard.focus();
	await page.keyboard.press('Enter');
	await expect(reviewCard).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByLabel('Name')).toHaveValue('Code Review Loop');

	await page.getByRole('button', { name: /UI regression tester/ }).click();
	await expect(cards.filter({ hasText: 'UI regression tester' })).toHaveAttribute('aria-pressed', 'true');
	await expect(cards.filter({ hasText: 'Goal loop' })).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByLabel('Name')).toHaveValue('UI Regression Test 2');

	await page.getByRole('button', { name: /Start blank/ }).click();
	await expect(page.locator('[data-workflow-template-card][aria-pressed="true"]')).toHaveCount(0);
	await expect(page.getByLabel('Name')).toHaveValue('New Workflow');
	await expect(page.getByText('One-step starter')).toBeVisible();
});

test('scheduled workflow templates bind a real agent and editable cron schedule', async ({ page }) => {
	const workflowCreateRequests: { name: string; description?: string; yaml_content: string }[] = [];
	await mockApi(page, { workflowCreateRequests });
	await page.goto('/workflows/new');

	await page.getByRole('button', { name: /Scheduled repository caretaker/ }).click();
	await expect(page.getByLabel('Workflow schedule')).toHaveValue('0 9 * * 1');
	await expect(page.getByLabel('Scheduled agent')).toHaveValue(agentId);
	await page.getByLabel('Workflow schedule').fill('30 8 * * 2');
	await page.getByRole('button', { name: 'Create scheduled workflow' }).click();

	await expect.poll(() => workflowCreateRequests.length).toBe(1);
	const yaml = workflowCreateRequests[0].yaml_content;
	expect(yaml).toContain('cron: "30 8 * * 2"');
	expect(yaml).toContain(`caretaker: "${agentId}"`);
	expect(yaml).toContain('agent: "@caretaker"');
	expect(yaml).toContain('match: healthy');
	expect(yaml).toContain('match: changes');
	expect(yaml).toContain('match: blocked');
	expect(yaml).not.toContain('trigger:');
	expect(yaml).not.toContain('type: sink');
});

test('specialized templates generate bounded backlog, specification, and UI evidence flows', async ({ page }) => {
	const workflowCreateRequests: { name: string; description?: string; yaml_content: string }[] = [];
	await mockApi(page, { workflowCreateRequests });

	await page.goto('/workflows/new');
	await page.getByRole('button', { name: /Periodic issue\/backlog processor/ }).click();
	await page.getByRole('button', { name: 'Create scheduled workflow' }).click();
	await expect.poll(() => workflowCreateRequests.length).toBe(1);
	const backlog = workflowCreateRequests[0].yaml_content;
	expect(backlog).toContain(`processor: "${agentId}"`);
	expect(backlog).toContain('type: loop');
	expect(backlog).toContain('over: "@fetch_batch.items"');
	expect(backlog).toContain('Jira, GitHub, Linear, or another system');
	expect(backlog).toContain('write_back: false');
	expect(backlog).not.toContain('trigger:');
	expect(backlog).not.toContain('type: sink');

	await page.goto('/workflows/new');
	await page.getByRole('button', { name: /Requirements → detailed specification/ }).click();
	await page.getByRole('button', { name: 'Create workflow' }).click();
	await expect.poll(() => workflowCreateRequests.length).toBe(2);
	const specification = workflowCreateRequests[1].yaml_content;
	expect(specification).toContain('drafter:\n    type: agent');
	expect(specification).toContain('challenger:\n    type: agent');
	expect(specification).toContain('new_session: true');
	expect(specification).toContain('acceptance_criteria:');
	expect(specification).toContain('implementation_slices:');

	await page.goto('/workflows/new');
	await page.getByRole('button', { name: /UI regression tester/ }).click();
	await page.getByRole('button', { name: 'Create workflow' }).click();
	await expect.poll(() => workflowCreateRequests.length).toBe(3);
	const regression = workflowCreateRequests[2].yaml_content;
	expect(regression).toContain('target_url:\n    type: string');
	expect(regression).toContain('test_scope:\n    type: string');
	expect(regression).toContain('switch: "@allow_fix"');
	expect(regression).toContain('switch: "@apply_fix.outcome"');
	expect(regression).toContain('Capture fresh evidence');
	expect(regression).toContain('Do not commit, push, open or update a pull request');
});

test('goal-loop workflow template is bounded and reusable across agents', async ({ page }) => {
	const workflowCreateRequests: { name: string; description?: string; yaml_content: string }[] = [];
	await mockApi(page, { workflowCreateRequests });
	await page.goto('/workflows/new');

	await page.getByRole('button', { name: /Goal loop/ }).click();
	await expect(page.getByText('Each run chooses one worker Agent. The same definition can be reused in any Project.')).toBeVisible();
	await page.getByRole('button', { name: 'Create workflow' }).click();

	await expect.poll(() => workflowCreateRequests.length).toBe(1);
	const created = workflowCreateRequests[0];
	expect(created.name).toBe('Goal Loop');
	expect(created.description).toContain('bounded loop');
	expect(created.yaml_content).toContain('type: agent');
	expect(created.yaml_content).toContain('primary: true');
	expect(created.yaml_content).toContain('agent: "@worker"');
	expect(created.yaml_content).toContain('switch: "@pursue_goal.status"');
	expect(created.yaml_content).toContain('goto: step pursue_goal');
	expect(created.yaml_content).toContain('inputs:');
	expect(created.yaml_content).toContain('required: true');
	await expect(page).toHaveURL('/workflows/workflow-created');
});

test('code-review template waits durably for human GitHub activity', async ({ page }) => {
	const workflowCreateRequests: { name: string; description?: string; yaml_content: string }[] = [];
	await mockApi(page, { workflowCreateRequests });
	await page.goto('/workflows/new');

	await page.getByRole('button', { name: /Implementation \+ independent review/ }).click();
	await page.getByRole('button', { name: 'Create workflow' }).click();
	await expect.poll(() => workflowCreateRequests.length).toBe(1);
	const yaml = workflowCreateRequests[0].yaml_content;
	expect(yaml).toContain('implementer:\n    type: agent');
	expect(yaml).toContain('reviewer:\n    type: agent');
	expect(yaml).toContain('agent: "@implementer"');
	expect(yaml).toContain('agent: "@reviewer"');
	expect(yaml).toContain('new_session: true');
	expect(yaml).toContain('create or\n          update a DRAFT GitHub pull request');
	expect(yaml).toContain('Independently review the actual pull request at:');
	expect(yaml).toContain('@implement.pull_request_url');
	expect(yaml).toContain('id: revise');
	expect(yaml).toContain('target: step review');
	expect(yaml).toContain('type: wait');
	expect(yaml).toContain('event: github.pull_request.activity');
	expect(yaml).toContain('resource: "@mark_ready.pull_request_url"');
	expect(yaml).toContain('on_timeout: flow timed_out');
	expect(yaml).toContain('goto: step wait_for_review');
});

test('workflows can be run with typed inputs and show their automatic trigger', async ({ page }) => {
	const workflowRunRequests: { id: string; inputs: Record<string, unknown> }[] = [];
	await mockApi(page, {
		workflowRunRequests,
		workflows: [{
			id: 'workflow-report',
			name: 'Release report',
			description: 'Build a release-readiness report.',
			yaml_content: `name: release-report
inputs:
  goal:
    type: string
    description: What should the report investigate?
    required: true
  retries:
    type: number
    default: 2
  options:
    type: json
  worker:
    type: agent
    required: true
    primary: true
variables:
  internal_retry_delay: 30
schedule:
  cron: "0 9 * * 1"
  inputs:
    goal: Weekly release report
    worker: ${agentId}
flows:
  main:
    steps:
      - id: report
        agent: "@worker"
        prompt: "Build @goal with @options"
`,
			enabled: true,
			version: 1,
			created_at: timestamp(1),
			updated_at: timestamp(2),
			last_triggered_at: null,
			trigger_count: 0,
			trigger_error: null,
		}],
	});

	await page.goto('/automations');
	const card = page.locator('[data-workflow-card]');
	await expect(card).toContainText('Scheduled · 0 9 * * 1');
	await expect(card).toContainText('4 inputs');
	await expect(card).toContainText('1 flow');
	await card.getByRole('link', { name: 'Run' }).click();

	await expect(page).toHaveURL('/workflows/workflow-report');
	await expect(page.locator('[data-workflow-configuration]')).toContainText('0 9 * * 1');
	await expect(page.getByLabel(/internal retry delay/i)).toHaveCount(0);
	await page.getByLabel(/goal/i).fill('Prepare version 0.3');
	await expect(page.getByLabel(/retries/i)).toHaveValue('2');
	await page.getByLabel(/options/i).fill('{"include_ci":true}');
	await expect(page.getByLabel(/worker/i)).toHaveValue(agentId);
	await page.getByRole('button', { name: 'Start workflow' }).click();

	await expect.poll(() => workflowRunRequests).toEqual([{
		id: 'workflow-report',
		inputs: { goal: 'Prepare version 0.3', retries: 2, options: { include_ci: true }, worker: agentId },
	}]);
});

test('appearance follows the saved light, dark, and system preference', async ({ page }) => {
	await page.emulateMedia({ colorScheme: 'light' });
	await mockApi(page);
	await page.goto('/settings');

	const root = page.locator('html');
	const system = page.locator('[data-theme-option="system"] input');
	const light = page.locator('[data-theme-option="light"] input');
	const dark = page.locator('[data-theme-option="dark"] input');

	await expect(system).toBeChecked();
	await expect(root).not.toHaveClass(/dark/);
	await expect(root).toHaveAttribute('data-theme', 'system');

	const secondPage = await page.context().newPage();
	await mockApi(secondPage);
	await secondPage.goto('/settings');
	await secondPage.locator('[data-theme-option="dark"]').click();
	await expect(dark).toBeChecked();
	await expect(root).toHaveClass(/dark/);
	await secondPage.evaluate(() => localStorage.removeItem('xpressclaw.theme'));
	await expect(system).toBeChecked();
	await expect(root).not.toHaveClass(/dark/);
	await secondPage.close();

	await page.locator('[data-theme-option="dark"]').click();
	await expect(dark).toBeChecked();
	await expect(root).toHaveClass(/dark/);
	await expect(root).toHaveAttribute('data-theme', 'dark');
	expect(await page.evaluate(() => localStorage.getItem('xpressclaw.theme'))).toBe('dark');

	await page.reload();
	await expect(dark).toBeChecked();
	await expect(root).toHaveClass(/dark/);

	await page.locator('[data-theme-option="light"]').click();
	await expect(light).toBeChecked();
	await expect(root).not.toHaveClass(/dark/);
	await page.emulateMedia({ colorScheme: 'dark' });
	await expect(root).not.toHaveClass(/dark/);

	await page.locator('[data-theme-option="system"]').click();
	await expect(system).toBeChecked();
	await expect(root).toHaveClass(/dark/);
	await page.emulateMedia({ colorScheme: 'light' });
	await expect(root).not.toHaveClass(/dark/);

	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('project sync settings fetch, acknowledge conflicts, and publish explicitly', async ({ page }) => {
	const projectSyncRequests: { projectId: string; operation: 'fetch' | 'publish'; force: boolean }[] = [];
	await mockApi(page, {
		projectSyncRequests,
		projectSyncFetchConflictOnce: true,
		projectSyncStatuses: [
			{
				project_id: projectId,
				project_name: 'Browser collaboration project',
				project_icon: null,
				status: 'ready',
				project_dir: '/srv/repos/xpressclaw',
				remote: 'git@github.com:XpressAI/project-data.git',
				branch: 'main',
				store_path: `projects/${projectId}`,
				share_project_memory: true,
				last_commit: 'abcdef1234567890',
				last_synced_at: '2026-07-19 00:01:01',
				message: null,
				warnings: [
					'No .xpressclaw.yml was found in these assigned Agent workspaces: /srv/repos/temporary-clone.',
				],
			},
			{
				project_id: 'project-without-manifest',
				project_name: 'Local-only project',
				project_icon: 'L',
				status: 'unconfigured',
				project_dir: '/srv/repos/local-only',
				remote: null,
				branch: null,
				store_path: null,
				share_project_memory: null,
				last_commit: null,
				last_synced_at: null,
				message: 'No .xpressclaw.yml was found in this Project workspace. Run `xpressclaw sync init` there first.',
				warnings: [],
			},
			{
				project_id: 'project-with-conflict',
				project_name: 'Conflicting project',
				project_icon: null,
				status: 'conflict',
				project_dir: null,
				remote: null,
				branch: null,
				store_path: null,
				share_project_memory: null,
				last_commit: null,
				last_synced_at: null,
				message: 'Project sync configuration conflict. Differing fields: store.branch.\n- branch=main: /srv/repos/one\n- branch=release: /srv/repos/two',
				warnings: [],
			},
		],
	});
	await page.goto('/settings/sync');

	await expect(page.getByRole('heading', { name: 'Project sync' })).toBeVisible();
	await expect(page.locator('[data-sidebar-setting="settings-sync"]')).toHaveAttribute('aria-current', 'page');
	const ready = page.locator(`[data-project-sync="${projectId}"]`);
	await expect(ready).toContainText('git@github.com:XpressAI/project-data.git');
	await expect(ready).toContainText(`projects/${projectId}`);
	await expect(ready).toContainText('Included');
	await expect(ready).toContainText('Ready');
	await expect(ready.locator('[data-project-sync-warnings]')).toContainText('/srv/repos/temporary-clone');

	const unconfigured = page.locator('[data-project-sync="project-without-manifest"]');
	await expect(unconfigured).toContainText('Needs setup');
	await expect(unconfigured.getByRole('button', { name: 'Fetch' })).toBeDisabled();
	await expect(unconfigured.getByRole('button', { name: 'Publish' })).toBeDisabled();

	const conflict = page.locator('[data-project-sync="project-with-conflict"]');
	await expect(conflict).toContainText('Configuration conflict');
	await expect(conflict).toContainText('Differing fields: store.branch');
	await expect(conflict).toContainText('/srv/repos/one');
	await expect(conflict.getByRole('button', { name: 'Fetch' })).toBeDisabled();
	await expect(conflict.getByRole('button', { name: 'Publish' })).toBeDisabled();

	await ready.getByRole('button', { name: 'Fetch' }).click();
	await expect(ready).toContainText('rerun with --force');
	await ready.getByRole('button', { name: 'Merge remote changes' }).click();
	await expect.poll(() => projectSyncRequests).toEqual([
		{ projectId, operation: 'fetch', force: false },
		{ projectId, operation: 'fetch', force: true },
	]);
	await expect(ready).toContainText('Fetched 2 Agents, 8 tasks, 3 Conversations, and 1 workflow at 12345678.');

	await ready.getByRole('button', { name: 'Publish' }).click();
	await expect.poll(() => projectSyncRequests.at(-1)).toEqual({ projectId, operation: 'publish', force: false });
	await expect(ready).toContainText('Published 2 Agents, 8 tasks, 3 Conversations, and 1 workflow at 87654321.');

	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('local collaboration settings stay opt-in and expose safe lifecycle controls', async ({ page }) => {
	const collaborationActions: string[] = [];
	const collaborationSettings = {
		config: {
			enabled: true, bind_address: '127.0.0.1', gitbucket_port: 8088, jenkins_port: 8089,
			gitbucket_image: 'ghcr.io/gitbucket/gitbucket:4.46.1', jenkins_image: 'jenkins/jenkins:2.568.1-jdk21',
			authorized_agents: ['browser-tested-workspace'],
		},
		status: {
			configured: true, docker_available: true, network: 'xpressclaw-collaboration-test-network', data_path: '/data/collaboration',
			gitbucket: { state: 'running', health: 'healthy', image: 'ghcr.io/gitbucket/gitbucket:4.46.1', version: '4.46.1', host_url: 'http://127.0.0.1:8088', internal_url: 'http://gitbucket:8080', volume: 'gitbucket-data', error: null },
			jenkins: { state: 'running', health: 'healthy', image: 'jenkins/jenkins:2.568.1-jdk21', version: '2.568.1-jdk21', host_url: 'http://127.0.0.1:8089', internal_url: 'http://jenkins:8080', volume: 'jenkins-data', error: null },
		},
		credentials_configured: true,
		reset_confirmation: 'RESET LOCAL COLLABORATION',
	};
	await mockApi(page, { collaborationActions, collaborationSettings });
	await page.goto('/settings/collaboration');

	await expect(page.getByRole('heading', { name: 'Local collaboration services' })).toBeVisible();
	await expect(page.locator('[data-sidebar-setting="settings-collaboration"]')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByText('Healthy', { exact: true })).toHaveCount(2);
	await expect(page.getByText('http://gitbucket:8080')).toBeVisible();
	await expect(page.getByText('http://jenkins:8080')).toBeVisible();
	await expect(page.getByText('browser-tested-workspace', { exact: true })).toBeVisible();
	await expect(page.getByText(/no host Docker socket/i)).toBeVisible();

	await page.getByRole('button', { name: 'stop', exact: true }).click();
	await expect.poll(() => collaborationActions).toContain('stop');
	await page.getByText('Permanently reset local collaboration data').click();
	const destructive = page.getByRole('button', { name: 'Delete services and persistent data' });
	await expect(destructive).toBeDisabled();
	await page.getByLabel(/Type RESET LOCAL COLLABORATION/).fill('RESET LOCAL COLLABORATION');
	await expect(destructive).toBeEnabled();

	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('local collaboration install persists the visible enabled configuration first', async ({ page }) => {
	const collaborationActions: string[] = [];
	const collaborationSettings = {
		config: {
			enabled: false, bind_address: '127.0.0.1', gitbucket_port: 8088, jenkins_port: 8089,
			gitbucket_image: 'ghcr.io/gitbucket/gitbucket:4.46.1', jenkins_image: 'jenkins/jenkins:2.568.1-jdk21',
			authorized_agents: [],
		},
		status: {
			configured: false, docker_available: true, network: 'xpressclaw-collaboration-test-network', data_path: '/data/collaboration',
			gitbucket: { state: 'not_installed', health: 'unknown', image: 'ghcr.io/gitbucket/gitbucket:4.46.1', version: '4.46.1', host_url: 'http://127.0.0.1:8088', internal_url: 'http://gitbucket:8080', volume: 'gitbucket-data', error: null },
			jenkins: { state: 'not_installed', health: 'unknown', image: 'jenkins/jenkins:2.568.1-jdk21', version: '2.568.1-jdk21', host_url: 'http://127.0.0.1:8089', internal_url: 'http://jenkins:8080', volume: 'jenkins-data', error: null },
		},
		credentials_configured: false,
		reset_confirmation: 'RESET LOCAL COLLABORATION',
	};
	await mockApi(page, { collaborationActions, collaborationSettings });
	await page.goto('/settings/collaboration');

	const install = page.getByRole('button', { name: 'Install services' });
	const restart = page.getByRole('button', { name: 'Restart & apply configuration' });
	const upgrade = page.getByRole('button', { name: 'Upgrade pinned images' });
	await expect(install).toBeDisabled();
	await expect(restart).toBeDisabled();
	await expect(upgrade).toBeDisabled();
	await page.getByLabel('Enable local collaboration configuration').check();
	await expect(install).toBeEnabled();
	await expect(restart).toBeEnabled();
	await expect(upgrade).toBeEnabled();
	await install.click();

	await expect.poll(() => collaborationActions).toEqual(['save', 'install']);
	await expect(page.getByText('Install completed.')).toBeVisible();
});

test('local collaboration restart saves and applies visible port changes', async ({ page }) => {
	const collaborationActions: string[] = [];
	const collaborationSettings = {
		config: {
			enabled: true, bind_address: '127.0.0.1', gitbucket_port: 8088, jenkins_port: 8089,
			gitbucket_image: 'ghcr.io/gitbucket/gitbucket:4.46.1', jenkins_image: 'jenkins/jenkins:2.568.1-jdk21',
			authorized_agents: [],
		},
		status: {
			configured: true, docker_available: true, network: 'xpressclaw-collaboration-test-network', data_path: '/data/collaboration',
			gitbucket: { state: 'running', health: 'healthy', image: 'ghcr.io/gitbucket/gitbucket:4.46.1', version: '4.46.1', host_url: 'http://127.0.0.1:8088', internal_url: 'http://gitbucket:8080', volume: 'gitbucket-data', error: null },
			jenkins: { state: 'running', health: 'healthy', image: 'jenkins/jenkins:2.568.1-jdk21', version: '2.568.1-jdk21', host_url: 'http://127.0.0.1:8089', internal_url: 'http://jenkins:8080', volume: 'jenkins-data', error: null },
		},
		credentials_configured: true,
		reset_confirmation: 'RESET LOCAL COLLABORATION',
	};
	await mockApi(page, { collaborationActions, collaborationSettings });
	await page.goto('/settings/collaboration');

	await page.getByLabel('Jenkins port').fill('9089');
	await page.getByRole('button', { name: 'Restart & apply configuration' }).click();

	await expect.poll(() => collaborationActions).toEqual(['save', 'restart']);
	await expect(page.getByLabel('Jenkins port')).toHaveValue('9089');
	await expect(page.getByText('Restart completed.')).toBeVisible();
});

test('local collaboration upgrade saves the visible pinned images first', async ({ page }) => {
	const collaborationActions: string[] = [];
	const collaborationSettings = {
		config: {
			enabled: true, bind_address: '127.0.0.1', gitbucket_port: 8088, jenkins_port: 8089,
			gitbucket_image: 'ghcr.io/gitbucket/gitbucket:4.46.1', jenkins_image: 'jenkins/jenkins:2.568.1-jdk21',
			authorized_agents: [],
		},
		status: {
			configured: true, docker_available: true, network: 'xpressclaw-collaboration-test-network', data_path: '/data/collaboration',
			gitbucket: { state: 'running', health: 'healthy', image: 'ghcr.io/gitbucket/gitbucket:4.46.1', version: '4.46.1', host_url: 'http://127.0.0.1:8088', internal_url: 'http://gitbucket:8080', volume: 'gitbucket-data', error: null },
			jenkins: { state: 'running', health: 'healthy', image: 'jenkins/jenkins:2.568.1-jdk21', version: '2.568.1-jdk21', host_url: 'http://127.0.0.1:8089', internal_url: 'http://jenkins:8080', volume: 'jenkins-data', error: null },
		},
		credentials_configured: true,
		reset_confirmation: 'RESET LOCAL COLLABORATION',
	};
	await mockApi(page, { collaborationActions, collaborationSettings });
	await page.goto('/settings/collaboration');

	await page.getByText('Pinned images and upgrade policy').click();
	await page.getByLabel('GitBucket image').fill('ghcr.io/gitbucket/gitbucket:4.47.0');
	await page.getByRole('button', { name: 'Upgrade pinned images' }).click();

	await expect.poll(() => collaborationActions).toEqual(['save', 'upgrade']);
	await expect(page.getByLabel('GitBucket image')).toHaveValue('ghcr.io/gitbucket/gitbucket:4.47.0');
	await expect(page.getByText('Upgrade completed.')).toBeVisible();
});
