import { expect, test, type Locator, type Page } from '@playwright/test';

const taskId = 'task-browser-test';
const agentId = 'project-browser-test';
const startTime = Date.parse('2026-07-19T00:00:00.000Z');

function timestamp(second: number): string {
	return new Date(startTime + second * 1_000).toISOString();
}

async function expectVerticalScroll(scroller: Locator) {
	await expect(scroller).toBeVisible();
	await expect(scroller).toHaveCSS('overflow-y', 'auto');
	await expect.poll(() => scroller.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
	await scroller.evaluate((element) => element.scrollTo({ top: element.scrollHeight }));
	await expect.poll(() => scroller.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
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

function attempt(status: string, contextUsed = 128_000) {
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
		result: null,
		error_message: null,
		created_at: timestamp(0),
		started_at: timestamp(1),
		completed_at: status === 'completed' ? timestamp(61) : null,
		context_used: contextUsed,
		context_size: 256_000,
	};
}

async function mockApi(
	page: Page,
	options: {
		live?: boolean;
		agentTimeline?: boolean;
		richToolActivity?: boolean;
		postedMessages?: Record<string, unknown>[];
		interruptedAttempts?: string[];
		connection?: { online: boolean };
		multipleAgents?: boolean;
		projectCount?: number;
		queuedSessionMessages?: { agentId: string; payload: Record<string, unknown> }[];
		projectTaskUpdates?: number[];
		completedTaskCount?: number;
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
		workflows?: {
			id: string;
			name: string;
			description: string | null;
			yaml_content: string;
			enabled: boolean;
			version: number;
			created_at: string;
			updated_at: string;
		}[];
	} = {},
) {
	let liveEvent = 0;
	let contextUsed = 128_000;
	const status = options.live ? 'in_progress' : 'completed';
	const attemptStatus = options.live ? 'running' : 'completed';
	const task = {
		id: taskId,
		title: 'Browser-tested workspace',
		description: 'Inspect the project and report what you find.',
		status,
		priority: 0,
		agent_id: agentId,
		parent_task_id: null,
		sop_id: null,
		created_at: timestamp(0),
		updated_at: timestamp(61),
		completed_at: status === 'completed' ? timestamp(61) : null,
		context: {},
		depends_on: [],
		dependents: [],
		blocked_by: [],
		ready: true,
	};
	const agent = {
		id: agentId,
		name: 'browser-tested-workspace',
		title: 'Browser-tested workspace',
		backend: 'codex',
		status: options.live ? 'running' : 'stopped',
		desired_status: options.live ? 'running' : 'stopped',
		observed_status: options.live ? 'running' : 'stopped',
		container_id: options.live ? 'container-browser-test' : null,
		config: { runner: { session_config: {} } },
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
		})
		: options.multipleAgents ? [agent, secondaryAgent] : [agent];
	const listedTasks = options.completedTaskCount
		? Array.from({ length: options.completedTaskCount }, (_, index) => ({
			...task,
			id: `completed-task-${index + 1}`,
			title: `Completed task ${index + 1}`,
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

	await page.addInitScript(() => localStorage.removeItem('xpressclaw.workspace.v1'));
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		let response: unknown;

		if (path === '/api/health') {
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
		} else if (path === '/api/agents') {
			response = availableAgents;
		} else if (path === `/api/agents/${agentId}`) {
			response = agent;
		} else if (path === '/api/workflows') {
			response = options.workflows ?? [];
		} else if (path === '/api/schedules') {
			response = [];
		} else if (path === '/api/setup/config') {
			response = {
				llm: { providers: [] },
				agents: [{
					name: agent.name,
					title: agent.title,
					backend: agent.backend,
					model: null,
					runner: {
						kind: 'codex',
						image: 'xpressclaw-runner-codex:latest',
						workspace: '/workspace',
						model: null,
						session_config: {},
						mcp_servers: [],
						environment: {},
						command: [],
						subscription_auth: true,
						container_engine: 'none',
					},
					tools: [],
					skills: [],
					volumes: [],
				}],
				system: { budget: { daily: '0', monthly: null, on_exceeded: 'warn' } },
				mcp_servers: [],
			};
		} else if (path === '/api/tasks') {
			if (url.searchParams.has('parent_task_id')) {
				response = { tasks: [], counts: { ...counts, completed: 0 } };
			} else {
				const includedStatuses = new Set((url.searchParams.get('statuses') ?? '').split(',').filter(Boolean));
				const excludedStatuses = new Set((url.searchParams.get('exclude_statuses') ?? '').split(',').filter(Boolean));
				const limit = Number.parseInt(url.searchParams.get('limit') ?? '100', 10);
				const offset = Number.parseInt(url.searchParams.get('offset') ?? '0', 10);
				const filteredTasks = [...listedTasks]
					.filter((listedTask) => includedStatuses.size === 0 || includedStatuses.has(listedTask.status))
					.filter((listedTask) => !excludedStatuses.has(listedTask.status));
				if (url.searchParams.get('sort') === 'recent') {
					filteredTasks.sort((left, right) =>
						Date.parse(right.updated_at) - Date.parse(left.updated_at)
						|| Date.parse(right.created_at) - Date.parse(left.created_at)
						|| right.id.localeCompare(left.id)
					);
				}
				response = {
					tasks: filteredTasks.slice(offset, offset + limit),
					counts,
				};
			}
		} else if (path === '/api/tasks/recent-by-agent') {
			const limit = Number.parseInt(url.searchParams.get('limit') ?? '5', 10);
			response = {
				tasks: [...listedTasks]
					.sort((left, right) =>
						Date.parse(right.updated_at) - Date.parse(left.updated_at)
						|| Date.parse(right.created_at) - Date.parse(left.created_at)
						|| right.id.localeCompare(left.id)
					)
					.slice(0, limit),
			};
		} else if (path === `/api/tasks/${taskId}`) {
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
				response = [
					{ id: 1, task_id: taskId, role: 'assistant', content: 'First answer', attachments: [], timestamp: timestamp(25) },
					{ id: 2, task_id: taskId, role: 'user', content: 'Please continue', attachments: [], timestamp: timestamp(40) },
					{ id: 3, task_id: taskId, role: 'assistant', content: 'Second answer', attachments: [], timestamp: timestamp(55) },
				];
			}
		} else if (path === `/api/tasks/${taskId}/activity`) {
			if (options.agentTimeline) {
				response = {
					attempts: [attempt(attemptStatus)],
					events: [
						timelineEvent(1, 10, 'runner_progress', "Yes, I'll inspect the project.", { item_type: 'agent_message', message_id: 'status-1' }),
						timelineEvent(2, 15, 'tool_call', 'Read the project', { toolCallId: 'tool-1', status: 'in_progress' }),
						timelineEvent(3, 20, 'runner_progress', 'Tests are running.', { item_type: 'agent_message', message_id: 'status-2' }),
						timelineEvent(4, 24, 'runner_progress', 'First answer', { item_type: 'agent_message', message_id: 'final-1' }),
					],
					has_more_before: false,
					has_more_after: false,
				};
			} else if (url.searchParams.has('before')) {
				response = {
					attempts: [attempt(attemptStatus, contextUsed)],
					events: Array.from({ length: 20 }, (_, index) => activityEvent(index + 1)),
					has_more_before: false,
					has_more_after: true,
				};
			} else if (url.searchParams.has('after')) {
				liveEvent += 1;
				contextUsed += 1_000;
				response = {
					attempts: [attempt(attemptStatus, contextUsed)],
					events: options.live ? [activityEvent(60 + liveEvent, 'New background activity')] : [],
					has_more_before: false,
					has_more_after: false,
				};
			} else {
				response = {
					attempts: [attempt(attemptStatus, contextUsed)],
					events: options.richToolActivity ? [
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
					] : Array.from({ length: 40 }, (_, index) => activityEvent(index + 21)),
					has_more_before: !options.richToolActivity,
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
			response = { ready: true };
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

test('agent updates stay beside their tools while the final reply is shown once', async ({ page }) => {
	await mockApi(page, { agentTimeline: true });
	await page.goto(`/tasks/${taskId}`);

	const transcript = page.locator('[data-task-transcript]');
	await expect(transcript).toBeVisible();
	await expect(page.getByRole('button', { name: /Yes, I'll inspect the project\./ })).toBeVisible();
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
	await page.addInitScript(() => {
		Object.defineProperty(window, 'isTauri', { value: true });
		(window as unknown as { __TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> } }).__TAURI_INTERNALS__ = {
			invoke: async (command: string) => {
				if (command === 'plugin:clipboard-manager|read_image') return 42;
				if (command === 'plugin:image|rgba') return [255, 0, 0, 255];
				if (command === 'plugin:image|size') return { width: 1, height: 1 };
				if (command === 'plugin:resources|close') return null;
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
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

test('new-work drafts restore the project they were written for', async ({ page }) => {
	const queuedSessionMessages: { agentId: string; payload: Record<string, unknown> }[] = [];
	await mockApi(page, { multipleAgents: true, queuedSessionMessages });
	await page.goto('/');

	const composer = page.getByPlaceholder('Describe the outcome you want…');
	const projectPicker = page.getByRole('combobox');
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

test('project Work shows only the five most recently updated tasks', async ({ page }) => {
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

	await mobile.getByRole('button', { name: 'Open project switcher' }).click();
	let mobileProject = mobile.locator(`aside a[href="/agents/${agentId}"]:visible`);
	await mobileProject.click();
	await expect(mobile).toHaveURL(`/agents/${agentId}`);
	await mobile.getByRole('tab', { name: 'Automations' }).click();
	await expect(mobile).toHaveURL(`/agents/${agentId}?tab=schedules`);

	await mobile.getByRole('button', { name: 'Open project switcher' }).click();
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
	await expect(mobileTabs).toHaveCount(2);
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

test('project context menus open sections and separate windows', async ({ page }) => {
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
		'Open Agent',
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

	await sidebar.locator('a[href="/agents"]').click();
	await sidebar.locator('a[href="/settings"]').click();
	await expect(tabs).toHaveCount(3);

	await tabs.filter({ has: page.locator('[title="Projects"]') }).click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Close Other Tabs' }).click();
	await expect(tabs).toHaveCount(1);
	await expect(tabs).toHaveAttribute('data-workspace-tab-title', 'Projects');
	await expect(page).toHaveURL('/agents');

	await tabs.click({ button: 'right' });
	await expect(page.getByRole('menuitem', { name: 'Close Other Tabs' })).toBeDisabled();
	await page.keyboard.press('Escape');

	await sidebar.locator('a[href="/settings"]').click();
	await tabs.filter({ has: page.locator('[title="Settings"]') }).click({ button: 'right' });
	await page.getByRole('menuitem', { name: 'Close Tab', exact: true }).click();
	await expect(tabs).toHaveCount(1);
	await expect(page).toHaveURL('/agents');

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

	const primaryGroup = taskSidebar.locator(`[data-sidebar-project-group="${agentId}"]`);
	await expect(primaryGroup.getByRole('heading', { name: 'Browser-tested workspace' })).toBeVisible();
	const primaryTasks = primaryGroup.locator('[data-sidebar-task]');
	await expect(primaryTasks).toHaveCount(5);
	expect(await primaryTasks.evaluateAll((items) => items.map((item) => item.getAttribute('href')))).toEqual([
		'/tasks/primary-newest',
		'/tasks/primary-second',
		'/tasks/primary-recent',
		'/tasks/primary-middle',
		`/tasks/${taskId}`,
	]);
	await expect(primaryGroup.locator(`a[href="/tasks/${taskId}"]`)).toHaveAttribute('aria-current', 'page');
	await expect(primaryGroup.locator(`a[href="/tasks/${taskId}"] [data-task-status="in_progress"]`)).toBeVisible();

	const secondaryGroup = taskSidebar.locator('[data-sidebar-project-group="project-secondary-test"]');
	await expect(secondaryGroup.getByRole('heading', { name: 'Secondary browser workspace' })).toBeVisible();
	expect(await secondaryGroup.locator('[data-sidebar-task]').evaluateAll((items) => items.map((item) => item.getAttribute('href')))).toEqual([
		'/tasks/secondary-newest',
		'/tasks/secondary-older',
	]);

	await sidebar.locator('a[href="/agents"]').click();
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

test('project, task, and workflow lists remain scrollable on mobile', async ({ browser }) => {
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
	await expectVerticalScroll(mobile.locator('[data-projects-scroll]'));
	await mobile.getByRole('button', { name: 'Open project switcher' }).click();
	await expectVerticalScroll(mobile.locator('aside:visible [data-mobile-sidebar-scroll]'));
	await mobile.locator('aside:visible').getByRole('button', { name: 'Close' }).click();

	await mobile.goto('/tasks');
	await mobile.getByRole('button', { name: 'Done 45' }).click();
	await expect(mobile.locator('[data-task-list] [data-task-row]')).toHaveCount(20);
	await expectVerticalScroll(mobile.locator('[data-tasks-scroll]'));
	await mobile.getByRole('button', { name: 'Open project switcher' }).click();
	await expectVerticalScroll(mobile.locator('aside:visible [data-mobile-sidebar-scroll]'));
	await mobile.locator('aside:visible').getByRole('button', { name: 'Close' }).click();

	await mobile.goto('/workflows');
	await expect(mobile.locator('[data-workflows-scroll] [data-workflow-card]')).toHaveCount(30);
	await expectVerticalScroll(mobile.locator('[data-workflows-scroll]'));
	await mobile.getByRole('button', { name: 'Open project switcher' }).click();
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

test('workflow and settings pages show context-specific sidebar lists', async ({ page }) => {
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
	});
	await page.goto('/workflows');

	const sidebar = page.locator('aside').first();
	const workflowSidebar = sidebar.locator('[data-sidebar-mode="workflows"]');
	await expect(workflowSidebar).toBeVisible();
	expect(await workflowSidebar.locator('[data-sidebar-workflow]').evaluateAll((items) =>
		items.map((item) => item.getAttribute('href'))
	)).toEqual(['/workflows/workflow-newer', '/workflows/workflow-older']);
	await expect(sidebar.locator(`a[href="/agents/${agentId}"]`)).toHaveCount(0);
	await expect(page.locator('[data-workflows-scroll]')).toHaveCSS('overflow-y', 'auto');

	await sidebar.locator('a[href="/settings"]').click();
	await expect(page).toHaveURL('/settings');
	const settingsSidebar = sidebar.locator('[data-sidebar-mode="settings"]');
	await expect(settingsSidebar).toBeVisible();
	await expect(settingsSidebar.locator('[data-sidebar-setting]')).toHaveText([
		'P Profile',
		'M MCP servers',
		'S Server',
	]);
	await expect(settingsSidebar.locator('[data-sidebar-setting="settings"]')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	await settingsSidebar.locator('a[href="/settings/mcp"]').click();
	await expect(page).toHaveURL('/settings/mcp');
	await expect(settingsSidebar.locator('[data-sidebar-setting="settings-mcp"]')).toHaveAttribute('aria-current', 'page');
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	await expect(sidebar.locator(`a[href="/agents/${agentId}"]`)).toHaveCount(0);

	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/workflows');
	await page.getByRole('button', { name: 'Open project switcher' }).click();
	await expect(page.locator('aside:visible [data-sidebar-mode="workflows"] [data-sidebar-workflow]')).toHaveCount(2);
	await page.locator('aside:visible').getByRole('button', { name: 'Close' }).click();
	await page.locator('nav a[href="/settings"]:visible').click();
	await page.getByRole('button', { name: 'Open project switcher' }).click();
	await expect(page.locator('aside:visible [data-sidebar-mode="settings"] [data-sidebar-setting]')).toHaveCount(3);
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toHaveCount(0);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});
