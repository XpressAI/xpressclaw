import { expect, test, type Page } from '@playwright/test';

const taskId = 'task-browser-test';
const agentId = 'project-browser-test';
const startTime = Date.parse('2026-07-19T00:00:00.000Z');

function timestamp(second: number): string {
	return new Date(startTime + second * 1_000).toISOString();
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

function attempt(status: string) {
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
	};
}

async function mockApi(
	page: Page,
	options: { live?: boolean; agentTimeline?: boolean; postedMessages?: Record<string, unknown>[] } = {},
) {
	let liveEvent = 0;
	const status = options.live ? 'in_progress' : 'completed';
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
	const counts = {
		pending: 0,
		in_progress: options.live ? 1 : 0,
		waiting_for_input: 0,
		blocked: 0,
		completed: options.live ? 0 : 1,
		cancelled: 0,
	};

	await page.addInitScript(() => localStorage.removeItem('xpressclaw.workspace.v1'));
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		const path = url.pathname;
		let response: unknown;

		if (path === '/api/health') {
			response = { status: 'ok' };
		} else if (path === '/api/setup/check-docker') {
			response = { available: true, installed: true, can_start: false };
		} else if (path === '/api/agents') {
			response = [agent];
		} else if (path === '/api/workflows') {
			response = [];
		} else if (path === '/api/tasks') {
			response = url.searchParams.has('parent_task_id')
				? { tasks: [], counts: { ...counts, completed: 0 } }
				: { tasks: [task], counts };
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
					attempts: [attempt(status)],
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
					attempts: [attempt(status)],
					events: Array.from({ length: 20 }, (_, index) => activityEvent(index + 1)),
					has_more_before: false,
					has_more_after: true,
				};
			} else if (url.searchParams.has('after')) {
				liveEvent += 1;
				response = {
					attempts: [attempt(status)],
					events: options.live ? [activityEvent(60 + liveEvent, 'New background activity')] : [],
					has_more_before: false,
					has_more_after: false,
				};
			} else {
				response = {
					attempts: [attempt(status)],
					events: Array.from({ length: 40 }, (_, index) => activityEvent(index + 21)),
					has_more_before: true,
					has_more_after: false,
				};
			}
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
		element.dispatchEvent(new ClipboardEvent('paste', {
			clipboardData: transfer,
			bubbles: true,
			cancelable: true,
		}));
	});
	await expect(page.getByAltText('pasted.png')).toBeVisible();

	await page.getByRole('button', { name: 'Send message' }).click();
	await expect.poll(() => postedMessages.length).toBe(1);
	const attachments = postedMessages[0].attachments as { name: string; mime_type: string; data: string }[];
	expect(postedMessages[0].content).toBe('');
	expect(attachments.map((attachment) => attachment.name)).toEqual(['selected.png', 'pasted.png']);
	expect(attachments.every((attachment) => attachment.mime_type === 'image/png' && attachment.data.length > 0)).toBe(true);
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
	await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeVisible();
	await expect(page.getByText('Connections', { exact: true })).toHaveCount(0);

	const mobile = await browser.newPage({ viewport: { width: 390, height: 844 }, isMobile: true });
	await mockApi(mobile);
	await mobile.goto(`/tasks/${taskId}`);
	await expect(mobile.locator('[data-workspace-pane]:visible')).toHaveCount(1);
	expect(await mobile.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	await expect(mobile.getByRole('button', { name: 'Open project switcher' })).toBeVisible();
	await mobile.close();
});
