import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { expect, test as base, type Locator, type Page } from '@playwright/test';

// WebKit needs a real HTTP attachment response to emit a download event.
const test = base.extend<{}, { downloadServer: string }>({
	downloadServer: [async ({}, use) => {
		const server = createServer((_request, response) => {
			response.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': 'attachment; filename="report.txt"' });
			response.end('the original Agent file');
		}).listen(0, '127.0.0.1');
		await once(server, 'listening');
		try {
			await use(`http://127.0.0.1:${(server.address() as { port: number }).port}`);
		} finally {
			await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
		}
	}, { scope: 'worker' }],
});

test.use({ serviceWorkers: 'block' });

const timestamp = '2026-09-27T12:00:00Z';
const author = 'original-agent';
const replacement = 'replacement-agent';
const task = { id: 'download-task', title: 'Downloads', description: '', status: 'completed', activity_status: 'completed', agent_id: replacement, created_at: timestamp, updated_at: timestamp, context: {}, depends_on: [], dependents: [], blocked_by: [], ready: true };
const conversation = { id: 'download-chat', title: 'Downloads', project_id: null, participants: [], created_at: timestamp, updated_at: timestamp };
const message = (content: string, extra = {}) => ({ id: 1, role: 'assistant', agent_id: author, sender_type: 'agent', sender_id: author, sender_name: 'Original Agent', content, attachments: [], created_at: timestamp, timestamp, ...extra });

async function clickDownload(page: Page, link: Locator) {
	await expect(link).toHaveAttribute('download', '', { timeout: 20_000 });
	await expect(link).not.toHaveAttribute('target', '_blank');
	// Download-attribute requests bypass Playwright routing (#22650). As in
	// the existing Files test, assert the attribute then let Content-Disposition
	// trigger the mocked download, keeping SvelteKit out of the navigation.
	await link.evaluate((element) => { element.removeAttribute('download'); element.setAttribute('data-sveltekit-reload', ''); });
	const waiting = page.waitForEvent('download');
	await link.click();
	return waiting;
}

async function mockApi(page: Page, options: { messages?: object[]; activity?: boolean; downloadServer?: string } = {}) {
	const downloads: { agent: string; path: string | null }[] = [];
	await page.route('**/api/**', async (route) => {
		const url = new URL(route.request().url());
		const path = url.pathname;
		const json = (body: unknown) => route.fulfill({ json: body });
		const download = path.match(/^\/api\/environments\/([^/]+)\/download$/);
		if (download) {
			downloads.push({ agent: decodeURIComponent(download[1]), path: url.searchParams.get('path') });
			return route.continue({ url: `${options.downloadServer}${url.pathname}${url.search}` });
		}
		if (path === '/api/auth/bootstrap') return json({ authenticated: true, authentication_enabled: true, csrf_token: 'test' });
		if (path === '/api/health') return json({ status: 'ok', version: 'test', build: 'test' });
		if (path === '/api/setup/check-docker') return json({ available: true, installed: true });
		if (path === '/api/setup/status') return json({ setup_complete: true });
		if (path === '/api/setup/config') return json({ llm: { providers: [] }, agents: [], mcp_servers: [], system: { budget: { daily: '0' } } });
		if (path === '/api/settings/profile') return json({ name: 'You', avatar: null });
		if (path === '/api/settings/speech') return json({ enabled: false });
		if (path === '/api/agents') return json([author, replacement].map((id) => ({ id, name: id, backend: 'codex', status: 'stopped', config: {}, created_at: timestamp })));
		if (path === '/api/conversations') return json([conversation]);
		if (path === '/api/conversations/download-chat') return json(conversation);
		if (path === '/api/tasks' || path === '/api/tasks/recent-by-agent' || path.endsWith('/subtasks')) return json({ tasks: [], counts: {} });
		if (path === '/api/tasks/counts') return json({});
		if (path === '/api/tasks/download-task') return json(task);
		if (path === '/api/tasks/download-task/activity') return json({
			attempts: options.activity ? [{ id: 'old-attempt', session_id: author, task_id: task.id, kind: 'message', runner: 'codex', status: 'completed', prompt: '', result: '[Result file](/tmp/result.txt)', created_at: timestamp, completed_at: timestamp }] : [],
			events: options.activity ? [{ id: 2, session_id: author, attempt_id: 'old-attempt', task_id: task.id, event_type: 'runner_progress', source_type: 'runner', summary: '[Update file](/tmp/update.txt)', payload: { item_type: 'agent_message' }, created_at: timestamp }] : [],
			has_more_before: false, has_more_after: false,
		});
		if (path.endsWith('/messages')) return json(options.messages ?? []);
		if (path.endsWith('/events')) return route.fulfill({ contentType: 'text/event-stream', body: ': connected\n\n' });
		return json([]);
	});
	return downloads;
}

for (const surface of ['tasks/download-task', 'conversations/download-chat']) {
	test(`downloads from the message author in ${surface}`, async ({ page, downloadServer }) => {
		const downloads = await mockApi(page, { downloadServer, messages: [message('[Download report](/tmp/report%20%23%3F%25%20caf%C3%A9.txt#L20)')] });
		await page.goto(`/${surface}`);
		const link = page.getByRole('link', { name: 'Download report', exact: true });
		const download = await clickDownload(page, link);
		expect(download.suggestedFilename()).toBe('report.txt');
		expect(await readFile((await download.path())!, 'utf8')).toBe('the original Agent file');
		expect(downloads).toEqual([{ agent: author, path: '/tmp/report #?% café.txt' }]);
		expect(page.url()).toContain(`/${surface}`);
		expect(page.context().pages()).toHaveLength(1);
	});
}

test('activity and standalone results use their producing attempt after reassignment', async ({ page, downloadServer }) => {
	const downloads = await mockApi(page, { downloadServer, activity: true });
	await page.goto('/tasks/download-task');
	for (const label of ['Update file', 'Result file']) {
		const download = await clickDownload(page, page.getByRole('link', { name: label, exact: true }));
		await download.path();
	}
	expect(downloads).toEqual([{ agent: author, path: '/tmp/update.txt' }, { agent: author, path: '/tmp/result.txt' }]);
});

test('file schemes download while application, API and external links keep their destinations', async ({ page }) => {
	const fileLinks = [
		['Root file', '/home/agent/report%2520one.txt', '/home/agent/report%20one.txt'],
		['File URI', 'file:///projects/report.txt', '/projects/report.txt'],
		['Sandbox file', 'sandbox:/mnt/data/report.pdf', '/mnt/data/report.pdf'],
	];
	const ordinaryLinks = [
		['Task link', '/tasks/other-task'], ['Project link', '/projects/project-id'], ['Anchor', '#section'],
		['Attachment', '/api/conversations/chat/attachments/file'],
		['Existing download', '/api/environments/another-agent/download?path=%2Ftmp%2Ffile.txt'],
		['External', 'https://example.com/report.pdf'], ['Network URL', '//example.com/report.pdf'],
		['Relative URL', 'guide.html'],
	];
	const content = [...fileLinks, ...ordinaryLinks].map(([name, href]) => `[${name}](${href})`).join('\n\n')
		+ '\n\n`/tmp/code.txt`\n\n<a href="file:///tmp/raw.txt">Raw HTML</a>\n\n[Unsafe](javascript:alert(1))';
	await mockApi(page, { messages: [message(content)] });
	await page.goto('/conversations/download-chat');
	for (const [name, , path] of fileLinks) {
		await expect(page.getByRole('link', { name, exact: true })).toHaveAttribute('href', `/api/environments/${author}/download?path=${encodeURIComponent(path)}`, { timeout: 20_000 });
	}
	for (const [name, href] of ordinaryLinks) {
		await expect(page.getByRole('link', { name, exact: true })).toHaveAttribute('href', href);
	}
	const contentElement = page.locator('[data-message-role="assistant"] .prose-chat');
	await expect(contentElement.locator('code')).toHaveText('/tmp/code.txt');
	await expect(contentElement).toContainText('<a href="file:///tmp/raw.txt">Raw HTML</a>');
	await expect(contentElement.locator('a[href^="javascript:"]')).toHaveCount(0);
});

test('user messages and old replies without a known author never guess a container', async ({ page }) => {
	await mockApi(page, { messages: [
		message('[Unknown author](/tmp/old.txt)', { agent_id: null }),
		message('[User file](/tmp/user.txt)', { id: 2, role: 'user' }),
	] });
	await page.goto('/tasks/download-task');
	await expect(page.getByRole('link', { name: 'Unknown author' })).toHaveAttribute('href', '/tmp/old.txt', { timeout: 20_000 });
	await expect(page.getByRole('link', { name: 'User file' })).toHaveAttribute('href', '/tmp/user.txt');
	await expect(page.locator('[data-transcript-kind="message"] a[download]')).toHaveCount(0);
});
