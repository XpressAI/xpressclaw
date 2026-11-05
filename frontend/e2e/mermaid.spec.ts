import { expect, test, type Locator, type Page } from '@playwright/test';

test.use({ serviceWorkers: 'block' });

const timestamp = '2026-11-05T12:00:00Z';
const task = { id: 'diagrams', title: 'Inline diagrams', description: '', status: 'completed', activity_status: 'completed', agent_id: 'agent', created_at: timestamp, updated_at: timestamp, context: {}, depends_on: [], dependents: [], blocked_by: [], ready: true };
const conversation = { id: 'diagrams', title: 'Inline diagrams', project_id: null, participants: [], created_at: timestamp, updated_at: timestamp };
const message = (content: string, extra = {}) => ({ id: 1, role: 'assistant', agent_id: 'agent', sender_type: 'agent', sender_id: 'agent', sender_name: 'Agent', content, attachments: [], created_at: timestamp, timestamp, ...extra });
const fence = (source: string, language = 'mermaid') => `\`\`\`${language}\n${source}\n\`\`\``;
const architecture = [
	'flowchart LR',
	'  P["Guest profile and personal context<br/>DynamoDB + vector index"]',
	'  B["AI Buddy"]',
	'  L["Iceberg / Athena<br/>Profile features + financial measures"]',
	'  C["Versioned cohort statistics"]',
	'  P -->|"Guest-scoped context retrieval"| B',
	'  P -->|"Selected profile features"| L',
	'  L -->|"Cohort calculation"| C',
	'  C -->|"Benchmark lookup"| B',
].join('\n');
const sequence = 'sequenceDiagram\n  User->>Agent: Draw a diagram\n  Agent-->>User: Inline result';

async function mockApi(page: Page, state: { messages: object[]; activity?: boolean }) {
	await page.route('**/api/**', (route) => {
		const path = new URL(route.request().url()).pathname;
		const json = (body: unknown) => route.fulfill({ json: body });
		if (path === '/api/auth/bootstrap') return json({ authenticated: true, authentication_enabled: true, csrf_token: 'test' });
		if (path === '/api/health') return json({ status: 'ok', version: 'test', build: 'test' });
		if (path === '/api/setup/check-docker') return json({ available: true, installed: true });
		if (path === '/api/setup/status') return json({ setup_complete: true });
		if (path === '/api/setup/config') return json({ llm: { providers: [] }, agents: [], mcp_servers: [], system: { budget: { daily: '0' } } });
		if (path === '/api/settings/profile') return json({ name: 'You', avatar: null });
		if (path === '/api/settings/speech') return json({ enabled: false });
		if (path === '/api/agents') return json([{ id: 'agent', name: 'Agent', backend: 'codex', status: 'stopped', config: {}, created_at: timestamp }]);
		if (path === '/api/conversations') return json([conversation]);
		if (path === '/api/conversations/diagrams') return json(conversation);
		if (path === '/api/tasks' || path === '/api/tasks/recent-by-agent' || path.endsWith('/subtasks')) return json({ tasks: [], counts: {} });
		if (path === '/api/tasks/counts') return json({});
		if (path === '/api/tasks/diagrams') return json(task);
		if (path === '/api/tasks/diagrams/activity') return json({
			attempts: state.activity ? [{ id: 'attempt', session_id: 'agent', task_id: task.id, kind: 'message', runner: 'codex', status: 'completed', prompt: '', result: `Final result\n\n${fence(architecture)}`, created_at: timestamp, completed_at: timestamp }] : [],
			events: state.activity ? ['runner_progress', 'agent_thought'].map((event_type, id) => ({ id, session_id: 'agent', attempt_id: 'attempt', task_id: task.id, event_type, source_type: 'runner', summary: fence(sequence), payload: { item_type: 'agent_message', message_id: `diagram-${id}` }, created_at: timestamp })) : [],
			has_more_before: false, has_more_after: false,
		});
		if (path.endsWith('/messages')) return json(state.messages);
		if (path.endsWith('/events')) return route.fulfill({ contentType: 'text/event-stream', body: ': connected\n\n' });
		return json([]);
	});
}

async function expectDiagram(image: Locator) {
	await expect(image).toBeVisible();
	await expect.poll(() => image.evaluate((node: HTMLImageElement) => node.complete && node.naturalWidth > 0 && node.naturalHeight > 0)).toBe(true);
}

async function captureEvents(page: Page) {
	await page.addInitScript(() => {
		const NativeEventSource = window.EventSource;
		window.EventSource = class extends NativeEventSource {
			constructor(url: string | URL, config?: EventSourceInit) {
				super(url, config);
				if (String(url).includes('/conversations/')) (window as typeof window & { diagramEvents?: EventSource }).diagramEvents = this;
			}
		};
	});
}

const refreshMessages = (page: Page) => page.evaluate(() => (window as typeof window & { diagramEvents?: EventSource }).diagramEvents!.dispatchEvent(new MessageEvent('message', { data: '{}' })));

for (const surface of ['tasks', 'conversations']) {
	test(`Mermaid renders inline in ${surface} with source, theme changes, and mobile scrolling`, async ({ page }, testInfo) => {
		const content = `**Architecture**\n\n${fence(architecture)}\n\n[Reference](https://example.com/docs)\n\n${fence(sequence, 'Mermaid')}`;
		await mockApi(page, { messages: [message(content), message(fence('flowchart TD\n  A[User diagram] --> B[Shown inline]'), { id: 2, role: 'user', sender_type: 'user', sender_id: 'local' })] });
		await page.goto(`/${surface}/diagrams`);
		const assistant = page.locator('[data-message-role="assistant"]');
		const images = assistant.locator('.mermaid-diagram img');
		await expect(images).toHaveCount(2);
		for (const image of await images.all()) await expectDiagram(image);
		await expectDiagram(page.locator('[data-message-role="user"] .mermaid-diagram img'));
		const labels = await images.first().evaluate((image: HTMLImageElement) => {
			const svg = new DOMParser().parseFromString(decodeURIComponent(image.src.split(',')[1]), 'image/svg+xml');
			return { text: [...svg.querySelectorAll('tspan.text-outer-tspan')].map((node) => node.textContent).join(' '), htmlLabels: svg.querySelectorAll('foreignObject').length };
		});
		expect(labels.text).toContain('Guest profile and personal context');
		expect(labels.text).toContain('DynamoDB + vector index');
		expect(labels.htmlLabels).toBe(0);
		await expect(assistant.locator('strong')).toHaveText('Architecture');
		await expect(assistant.getByRole('link', { name: 'Reference' })).toHaveAttribute('href', 'https://example.com/docs');
		await expect(assistant.locator('pre').first()).toBeHidden();
		await assistant.getByText('Diagram source', { exact: true }).first().click();
		await expect(assistant.locator('pre code').first()).toHaveText(`${architecture}\n`);
		await expect(assistant.locator('pre').first()).toBeVisible();
		await assistant.getByText('Diagram source', { exact: true }).first().click();

		for (const dark of [true, false]) {
			const before = await images.first().getAttribute('src');
			await page.evaluate((dark) => document.documentElement.classList.toggle('dark', dark), dark);
			await expect(images.first()).not.toHaveAttribute('src', before!);
			await expectDiagram(images.first());
			await images.first().scrollIntoViewIfNeeded();
			await page.screenshot({ path: testInfo.outputPath(`${surface}-${dark ? 'dark' : 'light'}.png`) });
		}
		await page.setViewportSize({ width: 390, height: 844 });
		const viewport = assistant.locator('.mermaid-diagram-viewport').first();
		await viewport.scrollIntoViewIfNeeded();
		await expect(viewport).toHaveCSS('overflow-x', 'auto');
		expect(await viewport.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
		await page.screenshot({ path: testInfo.outputPath(`${surface}-phone.png`) });
		await assistant.getByRole('button', { name: 'Actual size' }).first().click();
		expect(await viewport.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
		await viewport.evaluate((node) => { node.scrollLeft = node.scrollWidth; });
		expect(await viewport.evaluate((node) => node.scrollLeft)).toBeGreaterThan(0);
		await assistant.getByRole('button', { name: 'Fit diagram' }).first().click();
		expect(await viewport.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true);
	});
}

test('Mermaid renders in task updates, expanded thoughts, and standalone results', async ({ page }) => {
	await mockApi(page, { messages: [], activity: true });
	await page.goto('/tasks/diagrams');
	await expectDiagram(page.locator('[data-agent-update-content] .mermaid-diagram img'));
	await expectDiagram(page.locator('[data-task-result-content] .mermaid-diagram img'));
	await page.locator('[data-transcript-kind="activity"]').getByRole('button', { name: /Thinking/ }).click();
	await expectDiagram(page.locator('[data-activity-rich-content] .mermaid-diagram img'));
});

test('Mermaid leaves invalid and ordinary code readable and keeps diagram markup inert', async ({ page }) => {
	const execution = 'window.__diagramExecuted = true';
	const unsafe = `%%{init: {"securityLevel":"loose","htmlLabels":true,"themeCSS":"body { opacity: 0 !important; }"}}%%\nflowchart TD\n  A["<img src=x onerror='${execution}'>"] --> B[Safe]\n  click B "javascript:alert(1)"`;
	const invalid = 'flowchart LR\n  A[Unfinished';
	await mockApi(page, { messages: [message([
		fence(invalid), fence('const diagram = "mermaid";', 'javascript'), fence(architecture, ''),
		`<svg onload="${execution}">literal raw HTML</svg>`, fence(unsafe), fence(sequence),
	].join('\n\n'))] });
	await page.goto('/conversations/diagrams');
	const content = page.locator('[data-message-role="assistant"] .prose-chat');
	await expect(content.locator('.mermaid-diagram')).toHaveCount(2);
	await expect(content.locator(':scope > pre > code')).toHaveText([`${invalid}\n`, 'const diagram = "mermaid";\n', `${architecture}\n`]);
	for (const image of await content.locator('.mermaid-diagram img').all()) await expectDiagram(image);
	await expect(content).toContainText(`<svg onload="${execution}">literal raw HTML</svg>`);
	await expect(content.locator('svg, script, iframe, [onload], [onerror], a[href^="javascript:"]')).toHaveCount(0);
	expect(await page.evaluate(() => (window as typeof window & { __diagramExecuted?: boolean }).__diagramExecuted)).toBeUndefined();
	await expect(page.locator('body')).toHaveCSS('opacity', '1');
	await expect(page.locator('body > div[aria-hidden="true"]')).toHaveCount(0);
});

test('Mermaid updates streamed content and reuses completed diagrams as prose changes', async ({ page }) => {
	await captureEvents(page);
	const state = { messages: [message(fence('flowchart TD\n  A[Unfinished'))] };
	await mockApi(page, state);
	await page.goto('/conversations/diagrams');
	const content = page.locator('[data-message-role="assistant"] .prose-chat');
	await expect(content.locator('pre')).toBeVisible();
	state.messages = [message(fence(sequence))];
	await refreshMessages(page);
	const image = content.locator('.mermaid-diagram img');
	await expectDiagram(image);
	const rendered = await image.getAttribute('src');
	state.messages = [message(`${fence(sequence)}\n\nMore streamed prose.`)];
	await refreshMessages(page);
	await expect(content).toContainText('More streamed prose.');
	await expect(image).toHaveAttribute('src', rendered!);
	state.messages = [message(fence(architecture))];
	await refreshMessages(page);
	await expect(image).not.toHaveAttribute('src', rendered!);
	await expectDiagram(image);
});

test('Mermaid ignores replaced and removed messages while the renderer is loading', async ({ page }) => {
	await captureEvents(page);
	let releaseImport!: () => void;
	let startedImport!: () => void;
	const imported = new Promise<void>((resolve) => { startedImport = resolve; });
	const released = new Promise<void>((resolve) => { releaseImport = resolve; });
	await page.route((url) => url.pathname.endsWith('/mermaid.js'), async (route) => {
		startedImport();
		await released;
		await route.continue();
	});
	const state = { messages: [message(fence(architecture)), message(fence(architecture), { id: 2 })] };
	await mockApi(page, state);
	await page.goto('/conversations/diagrams');
	await imported;
	state.messages = [message(fence(sequence))];
	await refreshMessages(page);
	await expect(page.locator('[data-message-role="assistant"]')).toHaveCount(1);
	await expect(page.locator('[data-message-role="assistant"] pre code')).toHaveText(`${sequence}\n`);
	await page.evaluate(() => document.documentElement.classList.add('dark'));
	releaseImport();
	const image = page.locator('.mermaid-diagram img');
	await expectDiagram(image);
	await expect(image).toHaveCount(1);
	const svg = decodeURIComponent((await image.getAttribute('src'))!.split(',')[1]);
	expect(svg.includes('Guest')).toBe(false);
	expect(svg.includes('Inline result')).toBe(true);
	await expect(page.locator('body > div[aria-hidden="true"]')).toHaveCount(0);
});

test('ordinary messages do not load Mermaid', async ({ page }) => {
	const imports: string[] = [];
	page.on('request', (request) => {
		if (new URL(request.url()).pathname.endsWith('/mermaid.js')) imports.push(request.url());
	});
	await mockApi(page, { messages: [message(`**Ordinary message**\n\n${fence('const answer = 42;', 'javascript')}`)] });
	await page.goto('/conversations/diagrams');
	await expect(page.locator('[data-message-role="assistant"] pre code')).toHaveText('const answer = 42;\n');
	// Allow the asynchronous enhancement pass to finish before checking requests.
	await page.waitForTimeout(300);
	expect(imports).toEqual([]);
});
