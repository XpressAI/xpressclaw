import { expect, test, type Page } from '@playwright/test';

const projectId = 'project-platform';
const now = Date.now();
const livePreview = process.env.DASHBOARD_VISUAL_QA
	? 'Updated live response preview'
	: 'Updated <img src=x onerror="window.dashboardPwned=1"> response';

function iso(minutesAgo = 0) {
	return new Date(now - minutesAgo * 60_000).toISOString();
}

function event(overrides: Record<string, unknown> = {}) {
	return {
		cursor: 40,
		event_id: 'evt-existing',
		event_kind: 'agent_response',
		occurred_at: iso(1),
		project_id: projectId,
		project_name: 'Platform',
		agent_id: 'platform-agent',
		agent_name: 'Platform Agent',
		source_kind: 'agent',
		source_label: 'Platform Agent',
		target_type: 'task',
		target_id: 'task-dashboard',
		target_title: 'Build the control center',
		href: '/tasks/task-dashboard',
		severity: 'info',
		needs_attention: false,
		preview: 'Initial response preview',
		work_kind: 'attempt',
		work_id: 'attempt-dashboard',
		...overrides,
	};
}

function snapshot(empty = false, cursorBase = 40) {
	const series = Array.from({ length: 13 }, (_, index) => ({
		timestamp: new Date(now - (12 - index) * 5 * 60_000).toISOString(),
		context_used: empty ? 0 : 20_000 + index * 1_100,
		context_size: empty ? 0 : 258_400,
		tool_calls: empty ? 0 : index % 4,
		code_additions: empty ? 0 : index * 3,
		code_deletions: empty ? 0 : index,
		git_state: empty ? 'none' : index === 10 ? 'partial' : 'available',
	}));
	return {
		generated_at: iso(),
		cursor: cursorBase + 10,
		projects: [{ id: projectId, name: 'Platform' }, { id: 'project-docs', name: 'Docs' }],
		counters: empty
			? { working_agents: 0, active_work: 0, needs_attention: 0, tool_calls: 0 }
			: { working_agents: 2, active_work: 3, needs_attention: 1, tool_calls: 17 },
		token_usage: { total_tokens: empty ? null : 125_400, input_tokens: empty ? null : 100_000, output_tokens: empty ? null : 25_400, cached_read_tokens: empty ? null : 60_000, cached_write_tokens: null, thought_tokens: null, reported_responses: empty ? 0 : 5, unreported_responses: empty ? 0 : 2, unclassified_responses: 0, recording_started_at: iso(24 * 60) },
		series,
		active_work: empty ? [] : [{
			work_kind: 'attempt',
			work_id: 'attempt-dashboard',
			project_id: projectId,
			project_name: 'Platform',
			agent_id: 'platform-agent',
			agent_name: 'Platform Agent',
			target_type: 'task',
			target_id: 'task-dashboard',
			target_title: 'Build the control center',
			href: '/tasks/task-dashboard',
			phase: 'working',
			queued_at: iso(3),
			started_at: iso(2),
			activity: 'Verifying the dashboard stream',
		}],
		attention: empty ? [] : [{
			id: 'task:task-attention',
			kind: 'waiting_for_input',
			project_id: projectId,
			project_name: 'Platform',
			agent_id: 'platform-agent',
			agent_name: 'Platform Agent',
			target_type: 'task',
			target_id: 'task-attention',
			target_title: 'Confirm release scope',
			href: '/tasks/task-attention',
			summary: 'The Agent needs your input',
			updated_at: iso(1),
			work_kind: 'task',
			work_id: 'task-attention',
		}],
		feed: {
			events: empty ? [] : [event({ cursor: cursorBase }), event({
				cursor: cursorBase - 1,
				event_id: 'evt-attention',
				event_kind: 'waiting_for_input',
				target_id: 'task-attention',
				target_title: 'Confirm release scope',
				href: '/tasks/task-attention',
				severity: 'warning',
				needs_attention: true,
				preview: 'The Agent needs your input',
			})],
			next_before: empty ? null : cursorBase - 1,
			has_more: !empty,
		},
	};
}

async function mockDashboard(page: Page, options: {
	empty?: boolean;
	delaySnapshot?: boolean;
	delayFeed?: boolean;
	failScopedSnapshot?: boolean;
	failFirstRefresh?: boolean;
	manyFeedEvents?: boolean;
	outOfOrderRefresh?: boolean;
	delayDeletedSnapshot?: boolean;
	refreshProjects?: boolean;
	rollingWindow?: boolean;
	deleteRecentEventOnRefresh?: boolean;
	deleteOlderEventOnRefresh?: boolean;
	stream?: boolean;
	resourceUnavailable?: boolean;
	resourceFailureAfter?: number;
	scopedTokens?: boolean;
} = {}) {
	const scopes: string[] = [];
	let snapshotRequests = 0;
	let resourceRequests = 0;
	let feedRequests = 0;
	let selectedProjectDeleted = false;
	const attentionActions: string[] = [];
	let releaseFirstRefresh = () => {};
	let releaseDeletedSnapshot = () => {};
	const firstRefreshGate = new Promise<void>((resolve) => {
		releaseFirstRefresh = resolve;
	});
	const deletedSnapshotGate = new Promise<void>((resolve) => {
		releaseDeletedSnapshot = resolve;
	});
	await page.route('**/api/**', async (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/api/dashboard/resources') {
			resourceRequests++;
			if (options.resourceFailureAfter != null && resourceRequests > options.resourceFailureAfter) return route.fulfill({ status: 503, json: { error: 'Resource monitoring unavailable' } });
			const capacity = { total_bytes: 16 * 1024 ** 3, available_bytes: 4 * 1024 ** 3, used_bytes: 12 * 1024 ** 3, used_percent: 75 };
			return route.fulfill({ json: { sampled_at: new Date().toISOString(), cpu_percent: options.resourceUnavailable ? null : resourceRequests === 1 ? 32 : 48, cpu_count: 8, memory: options.resourceUnavailable ? null : capacity,
				disks: [{ locations: ['Data', 'Workspaces'], mount_point: '/', capacity: options.resourceUnavailable ? null : { ...capacity, total_bytes: 512 * 1024 ** 3, used_bytes: 460.8 * 1024 ** 3, available_bytes: 51.2 * 1024 ** 3, used_percent: 90 } }] } });
		}
		if (url.pathname === '/api/dashboard/snapshot') {
			const requestNumber = ++snapshotRequests;
			const requestedProject = url.searchParams.get('project_id');
			scopes.push(requestedProject ?? 'all');
			if (options.delaySnapshot && requestNumber === 1) await new Promise((resolve) => setTimeout(resolve, 800));
			if (options.failScopedSnapshot && requestedProject) {
				return route.fulfill({ status: 503, json: { error: 'Scoped dashboard unavailable' } });
			}
			if (selectedProjectDeleted && requestedProject === projectId) {
				if (options.delayDeletedSnapshot) await deletedSnapshotGate;
				return route.fulfill({ status: 404, json: { error: `Project not found: ${projectId}` } });
			}
			if (options.outOfOrderRefresh && requestNumber === 2) await firstRefreshGate;
			if (options.failFirstRefresh && requestNumber === 2) {
				return route.fulfill({ status: 503, json: { error: 'Summary refresh unavailable' } });
			}
			const response = snapshot(
				Boolean(options.empty || selectedProjectDeleted || (options.rollingWindow && requestNumber > 1)),
				options.manyFeedEvents ? 1_000 : 40,
			);
			if (options.scopedTokens) {
				response.token_usage.total_tokens = requestedProject ? (url.searchParams.get('range') === '7d' ? 321000 : 12345) : 125400;
			}
			if (selectedProjectDeleted) {
				response.projects = response.projects.filter((project) => project.id !== projectId);
				response.feed = {
					events: [event({
						cursor: 35,
						event_id: 'evt-docs-survives',
						project_id: 'project-docs',
						project_name: 'Docs',
						target_id: 'task-docs',
						target_title: 'Refresh the documentation',
						href: '/tasks/task-docs',
						preview: 'Docs activity remains available',
					})],
					next_before: 35,
					has_more: false,
				};
			}
			if (options.rollingWindow && requestNumber === 1) {
				response.feed.events = response.feed.events.map((item) => ({
					...item,
					occurred_at: iso(24 * 60 - 0.5),
				}));
			}
			if (options.outOfOrderRefresh && requestNumber > 1) {
				response.counters.working_agents = requestNumber === 2 ? 4 : 9;
			}
			if (options.refreshProjects && requestNumber > 1) {
				response.projects = [
					{ id: projectId, name: 'Platform renamed' },
					{ id: 'project-new', name: 'New Project' },
				];
			}
			if (options.deleteRecentEventOnRefresh && requestNumber > 1) {
				response.feed.events = response.feed.events.filter((item) => item.event_id !== 'evt-existing');
			}
			if (options.deleteOlderEventOnRefresh && requestNumber > 1) {
				response.feed.events = [event({
					cursor: 50,
					event_id: 'evt-older',
					event_kind: 'conversation_message_deleted',
					preview: '',
				}), ...response.feed.events];
			}
			return route.fulfill({ json: response });
		}
		if (url.pathname === '/api/dashboard/feed') {
			feedRequests += 1;
			if (options.delayFeed) await new Promise((resolve) => setTimeout(resolve, 300));
			if (options.manyFeedEvents) {
				const start = 998 - (feedRequests - 1) * 100;
				const events = Array.from({ length: 100 }, (_, index) => event({
					cursor: start - index,
					event_id: `evt-page-${feedRequests}-${index}`,
					preview: `Earlier activity ${feedRequests}-${index}`,
					occurred_at: iso(45 + feedRequests),
				}));
				return route.fulfill({ json: {
					events,
					next_before: start - 99,
					has_more: true,
				} });
			}
			return route.fulfill({ json: {
				events: [event({ cursor: 20, event_id: 'evt-older', preview: 'Earlier bounded activity', occurred_at: iso(45) })],
				next_before: null,
				has_more: false,
			} });
		}
		if (url.pathname === '/api/dashboard/stream') {
			if (options.stream === false) return route.abort('connectionrefused');
			const live = event({ cursor: 51, preview: livePreview, occurred_at: iso(0) });
			return route.fulfill({
				status: 200,
				headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
				body: `id: 51\nevent: dashboard\ndata: ${JSON.stringify(live)}\n\n`,
			});
		}
		if (url.pathname === '/api/health') return route.fulfill({ json: { status: 'ok' } });
		if (url.pathname === '/api/tasks/task-attention/status' && route.request().method() === 'PATCH') {
			attentionActions.push('task-attention');
			return route.fulfill({ json: { id: 'task-attention', status: 'cancelled' } });
		}
		if (url.pathname === '/api/setup/check-docker') return route.fulfill({ json: { available: true, installed: true, can_start: false } });
		if (url.pathname === '/api/projects') return route.fulfill({ json: [] });
		if (url.pathname === '/api/conversations') return route.fulfill({ json: [] });
		if (url.pathname === '/api/agents') return route.fulfill({ json: [] });
		if (url.pathname === '/api/tasks/recent-by-agent') return route.fulfill({ json: { tasks: [] } });
		if (url.pathname === '/api/tasks') return route.fulfill({ json: { tasks: [], counts: {} } });
		if (url.pathname === '/api/workflows' || url.pathname === '/api/schedules') return route.fulfill({ json: [] });
		return route.fulfill({ json: {} });
	});
	return {
		scopes,
		resourceRequestCount: () => resourceRequests,
		snapshotRequestCount: () => snapshotRequests,
		releaseFirstRefresh,
		releaseDeletedSnapshot,
		attentionActions,
		deleteSelectedProject: () => (selectedProjectDeleted = true),
	};
}

async function installMockEventSource(page: Page) {
	await page.addInitScript(() => {
		type DashboardListener = (event: { data: string }) => void;
		class MockEventSource {
			onopen: (() => void) | null = null;
			onerror: (() => void) | null = null;
			closed = false;
			listeners = new Map<string, Set<DashboardListener>>();

			constructor(_url: string) {
				(window as unknown as { __dashboardEventSources: MockEventSource[] })
					.__dashboardEventSources.push(this);
				queueMicrotask(() => this.onopen?.());
			}

			addEventListener(type: string, listener: DashboardListener) {
				const listeners = this.listeners.get(type) ?? new Set<DashboardListener>();
				listeners.add(listener);
				this.listeners.set(type, listeners);
			}

			close() {
				this.closed = true;
			}
		}

		const dashboardWindow = window as unknown as {
			EventSource: typeof EventSource;
			__dashboardEventSources: MockEventSource[];
			__emitDashboardEvent: (payload: unknown) => void;
			__emitDashboardCursor: () => void;
			__emitDashboardStreamError: () => void;
			__emitDashboardTransportError: () => void;
		};
		dashboardWindow.__dashboardEventSources = [];
		dashboardWindow.EventSource = MockEventSource as unknown as typeof EventSource;
		dashboardWindow.__emitDashboardEvent = (payload) => {
			const source = [...dashboardWindow.__dashboardEventSources]
				.reverse()
				.find((candidate) => !candidate.closed);
			if (!source) throw new Error('Dashboard EventSource is not connected');
			for (const listener of source.listeners.get('dashboard') ?? []) {
				listener({ data: JSON.stringify(payload) });
			}
		};
		dashboardWindow.__emitDashboardCursor = () => {
			const source = [...dashboardWindow.__dashboardEventSources]
				.reverse()
				.find((candidate) => !candidate.closed);
			if (!source) throw new Error('Dashboard EventSource is not connected');
			for (const listener of source.listeners.get('cursor') ?? []) {
				listener({ data: '{}' });
			}
		};
		dashboardWindow.__emitDashboardStreamError = () => {
			const source = [...dashboardWindow.__dashboardEventSources]
				.reverse()
				.find((candidate) => !candidate.closed);
			if (!source) throw new Error('Dashboard EventSource is not connected');
			for (const listener of source.listeners.get('stream_error') ?? []) {
				listener({ data: '{"message":"Dashboard stream is unavailable"}' });
			}
		};
		dashboardWindow.__emitDashboardTransportError = () => {
			const source = [...dashboardWindow.__dashboardEventSources]
				.reverse()
				.find((candidate) => !candidate.closed);
			if (!source) throw new Error('Dashboard EventSource is not connected');
			source.onerror?.();
		};
	});
}

async function emitDashboardEvent(page: Page, payload: unknown) {
	await page.waitForFunction(() => {
		const sources = (window as unknown as {
			__dashboardEventSources?: Array<{ closed: boolean }>;
		}).__dashboardEventSources;
		return sources?.some((source) => !source.closed) ?? false;
	});
	await page.evaluate((next) => {
		(window as unknown as { __emitDashboardEvent: (payload: unknown) => void })
			.__emitDashboardEvent(next);
	}, payload);
}

async function emitDashboardStreamError(page: Page) {
	await page.waitForFunction(() => {
		const sources = (window as unknown as {
			__dashboardEventSources?: Array<{ closed: boolean }>;
		}).__dashboardEventSources;
		return sources?.some((source) => !source.closed) ?? false;
	});
	await page.evaluate(() => {
		(window as unknown as { __emitDashboardStreamError: () => void })
			.__emitDashboardStreamError();
	});
}

async function emitDashboardCursor(page: Page) {
	await page.waitForFunction(() => {
		const sources = (window as unknown as {
			__dashboardEventSources?: Array<{ closed: boolean }>;
		}).__dashboardEventSources;
		return sources?.some((source) => !source.closed) ?? false;
	});
	await page.evaluate(() => {
		(window as unknown as { __emitDashboardCursor: () => void })
			.__emitDashboardCursor();
	});
}

async function emitDashboardTransportError(page: Page) {
	await page.waitForFunction(() => {
		const sources = (window as unknown as {
			__dashboardEventSources?: Array<{ closed: boolean }>;
		}).__dashboardEventSources;
		return sources?.some((source) => !source.closed) ?? false;
	});
	await page.evaluate(() => {
		(window as unknown as { __emitDashboardTransportError: () => void })
			.__emitDashboardTransportError();
	});
}

function rgb(value: string): [number, number, number] {
	const channels = value.match(/[\d.]+/g)?.slice(0, 3).map(Number);
	if (!channels || channels.length !== 3) throw new Error(`Invalid RGB value: ${value}`);
	return channels as [number, number, number];
}

function contrast(foreground: string, background: string) {
	const luminance = (color: string) => {
		const channels = rgb(color).map((channel) => {
			const normalized = channel / 255;
			return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
		});
		return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
	};
	const a = luminance(foreground);
	const b = luminance(background);
	return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

test('brand opens the real-time Control center with deduplicated live navigation', async ({ page }, testInfo) => {
	await page.setViewportSize({ width: 1440, height: 1050 });
	const { scopes, attentionActions } = await mockDashboard(page);
	await page.goto('/projects');
	await page.locator('a[aria-label="Open Control center"]').first().click();
	await expect(page).toHaveURL(/\/dashboard$/);
	await expect(page.getByRole('heading', { name: 'Control center' })).toBeVisible();
	await expect(page.locator('[data-working-agents]')).toContainText('2');
	await expect(page.locator('[data-kpi="needs-attention"]')).toContainText('1');
	await expect(page.locator('[data-active-now] a[href="/tasks/task-dashboard"]')).toBeVisible();
	await expect(page.locator('a[href="/tasks/task-attention"]').first()).toBeVisible();
	if (process.env.DASHBOARD_VISUAL_QA) {
		await page.screenshot({ path: testInfo.outputPath('dashboard-desktop.png'), fullPage: true });
	}

	const stableEvent = page.locator('[data-feed-event="evt-existing"]');
	await expect(stableEvent).toHaveCount(1);
	await expect(stableEvent).toContainText(livePreview);
	await expect(stableEvent.locator('img')).toHaveCount(0);
	expect(await page.evaluate(() => (window as unknown as { dashboardPwned?: number }).dashboardPwned)).toBeUndefined();
	await expect(page.locator('[data-live-feed] [data-feed-event="evt-existing"]')).toHaveCount(1);
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Cancel task' }).click();
	await expect.poll(() => attentionActions).toEqual(['task-attention']);
	await expect(page.getByRole('heading', { name: 'Needs your attention' })).toHaveCount(0);
	await page.getByRole('button', { name: 'Load earlier activity' }).click();
	await expect(page.locator('[data-feed-event="evt-older"]')).toContainText('Earlier bounded activity');

	await page.locator('#dashboard-project').selectOption(projectId);
	await expect.poll(() => scopes.includes(projectId)).toBe(true);
	await page.getByRole('button', { name: '7d' }).click();
	await expect.poll(() => scopes.filter((scope) => scope === projectId).length).toBeGreaterThan(1);
	await page.getByRole('button', { name: 'Code' }).click();
	await expect(page.locator('[data-dashboard-chart]')).toHaveAttribute('data-chart-mode', 'code');
	await expect(page.getByText('Partial Git data')).toBeVisible();
	if (process.env.DASHBOARD_VISUAL_QA) {
		await page.setViewportSize({ width: 390, height: 844 });
		await page.locator('[data-dashboard-page]').evaluate((element) => element.scrollTo({ top: 0 }));
		await page.screenshot({ path: testInfo.outputPath('dashboard-mobile-populated.png'), fullPage: true });
	}
});

test('resources refresh while quiet and stay independent of project and period token totals', async ({ page }) => {
	await page.clock.install();
	await installMockEventSource(page);
	const { resourceRequestCount } = await mockDashboard(page, { scopedTokens: true });
	await page.goto('/dashboard');
	// A cold Vite server can take time to load the workspace's modules.
	await expect(page.getByRole('heading', { name: 'Control center', exact: true })).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('meter', { name: 'CPU usage', exact: true })).toHaveAttribute('aria-valuenow', '32');
	await expect(page.locator('[data-kpi="memory"]')).toContainText('4 GiB available');
	await expect(page.locator('[data-kpi="disk"]')).toContainText('90%');
	await expect(page.locator('[data-token-count="Total"]')).toHaveText('125,400');
	await expect(page.locator('[data-token-count="Cache write"]')).toHaveText('—');
	await page.getByLabel('Project scope').selectOption(projectId);
	await expect(page.locator('[data-token-count="Total"]')).toHaveText('12,345');
	await page.getByRole('button', { name: '7d', exact: true }).click();
	await expect(page.locator('[data-token-count="Total"]')).toHaveText('321,000');
	await expect(page.locator('[data-resource-summary]')).toContainText('all Projects');
	await page.clock.runFor(5_100);
	await expect.poll(resourceRequestCount).toBe(2);
	await expect(page.getByRole('meter', { name: 'CPU usage', exact: true })).toHaveAttribute('aria-valuenow', '48');
	await page.getByRole('button', { name: 'Tools', exact: true }).click();
	await expect(page.getByRole('img', { name: 'Tool-call volume over time' })).toBeVisible();
});

test('missing resources and missing token data never look like zero usage on a phone', async ({ page }, testInfo) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await installMockEventSource(page);
	await mockDashboard(page, { empty: true, resourceUnavailable: true });
	await page.goto('/dashboard');
	// A cold Vite server can take time to load the workspace's modules.
	await expect(page.getByRole('heading', { name: 'Control center', exact: true })).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('[data-kpi="cpu"]')).toContainText('Sampling…');
	await expect(page.locator('[data-kpi="memory"]')).toContainText('Unavailable');
	await expect(page.locator('[data-kpi="disk"]')).toContainText('Unavailable');
	await expect(page.locator('[data-token-count="Total"]')).toHaveText('—');
	await expect(page.locator('[data-token-usage]')).toContainText('No usable token counts');
	await expect(page.getByRole('meter')).toHaveCount(0);
	await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	if (process.env.DASHBOARD_VISUAL_QA) {
		await page.locator('[data-token-usage]').scrollIntoViewIfNeeded();
		await page.screenshot({ path: testInfo.outputPath('dashboard-phone-tokens.png') });
	}
});

test('resource failures retain explicitly stale readings and can be retried', async ({ page }) => {
	await page.clock.install();
	await installMockEventSource(page);
	const { resourceRequestCount } = await mockDashboard(page, { resourceFailureAfter: 1 });
	await page.goto('/dashboard');
	// A cold Vite server can take time to load the workspace's modules.
	await expect(page.getByRole('heading', { name: 'Control center', exact: true })).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('[data-kpi="cpu"]')).toContainText('32%');
	await page.clock.runFor(5_100);
	await expect(page.locator('[data-resource-status]')).toContainText('Readings stale');
	await expect(page.locator('[data-kpi="cpu"]')).toContainText('32%');
	await page.getByRole('button', { name: 'Retry resources' }).click();
	await expect.poll(resourceRequestCount).toBe(3);
	await expect(page.locator('[data-token-count="Total"]')).toHaveText('125,400');
});

test('resource polling pauses in hidden tabs and recovers from a timed-out request', async ({ page }) => {
	await page.clock.install();
	await installMockEventSource(page);
	await mockDashboard(page);
	let requests = 0;
	await page.route('**/api/dashboard/resources', async (route) => {
		requests++;
		// Leave the second request unanswered until the browser aborts it.
		if (requests === 2) return;
		await route.fallback();
	});
	await page.goto('/dashboard');
	// A cold Vite server can take time to load the workspace's modules.
	await expect(page.getByRole('heading', { name: 'Control center', exact: true })).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('[data-kpi="cpu"]')).toContainText('32%');
	await page.evaluate(() => {
		Object.defineProperty(document, 'hidden', { configurable: true, value: true });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await page.clock.runFor(20_000);
	expect(requests).toBe(1);
	await page.evaluate(() => {
		Object.defineProperty(document, 'hidden', { configurable: true, value: false });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect.poll(() => requests).toBe(2);
	await page.clock.runFor(15_100);
	await expect(page.getByRole('button', { name: 'Retry resources' })).toBeVisible();
	await expect(page.locator('[data-resource-status]')).toContainText('Readings stale');
	await page.getByRole('button', { name: 'Retry resources' }).click();
	await expect(page.locator('[data-kpi="cpu"]')).toContainText('48%');
	await expect(page.locator('[data-resource-status]')).not.toContainText('stale');
});

test('the feed accepts agent text and deletion events without tool, user, or lifecycle noise', async ({ page }) => {
	await installMockEventSource(page);
	const { snapshotRequestCount } = await mockDashboard(page);
	await page.goto('/dashboard');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toBeVisible();
	for (const [index, kind] of ['tool_call', 'task_message', 'conversation_message', 'progress', 'completion', 'waiting_for_input'].entries()) {
		await emitDashboardEvent(page, event({ cursor: 100 + index, event_id: `noise-${kind}`, event_kind: kind, preview: `Noise ${kind}` }));
	}
	await expect(page.locator('[data-feed-event^="noise-"]')).toHaveCount(0);
	await expect.poll(snapshotRequestCount).toBeGreaterThan(1);
	await emitDashboardEvent(page, event({ cursor: 200, event_id: 'visible-update', event_kind: 'agent_update', preview: 'Build passed; checking the phone layout.' }));
	await expect(page.locator('[data-feed-event="visible-update"]')).toContainText('Build passed');
	await expect(page.locator('[data-feed-event="visible-update"] .event-kind')).toHaveText('Update');
	await emitDashboardEvent(page, event({ cursor: 201, event_id: 'visible-update', event_kind: 'conversation_message_deleted', preview: '' }));
	await expect(page.locator('[data-feed-event="visible-update"]')).toHaveCount(0);
});

test('loading, empty, reconnecting, themes, reduced motion, and mobile layout stay usable', async ({ page }, testInfo) => {
	await page.emulateMedia({ reducedMotion: 'reduce' });
	await page.setViewportSize({ width: 390, height: 844 });
	await mockDashboard(page, { empty: true, delaySnapshot: true, stream: false });
	await page.goto('/dashboard');
	await expect(page.getByLabel('Loading dashboard summary')).toBeVisible();
	await expect(page.getByText('The instance is quiet')).toBeVisible();
	await expect(page.getByText('No activity in this window')).toBeVisible();
	await expect(page.locator('[data-dashboard-connection]')).toContainText(/Reconnecting|Offline/);
	await expect(page.getByRole('img', { name: 'Context usage over time' })).toBeVisible();
	await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
	const [activeBox, feedBox, chartBox] = await Promise.all([
		page.locator('[data-active-now]').boundingBox(),
		page.locator('section[aria-labelledby="live-feed-heading"]').boundingBox(),
		page.locator('section[aria-labelledby="activity-chart-heading"]').boundingBox(),
	]);
	expect(activeBox!.y).toBeLessThan(feedBox!.y);
	expect(feedBox!.y).toBeLessThan(chartBox!.y);
	await page.getByRole('button', { name: 'Code' }).click();
	await expect(page.getByText('No Git line changes in this window')).toBeVisible();
	if (process.env.DASHBOARD_VISUAL_QA) {
		await page.screenshot({ path: testInfo.outputPath('dashboard-mobile.png'), fullPage: true });
		await page.locator('[data-dashboard-page]').evaluate((element) => element.scrollTo({ top: 720 }));
		await page.screenshot({ path: testInfo.outputPath('dashboard-mobile-feed.png'), fullPage: true });
	}

	for (const dark of [false, true]) {
		await page.evaluate((enabled) => document.documentElement.classList.toggle('dark', enabled), dark);
		const styles = await page.locator('[data-kpi="cpu"]').evaluate((element) => {
			const computed = getComputedStyle(element);
			const label = getComputedStyle(element.querySelector('h2')!);
			return { foreground: label.color, background: computed.backgroundColor };
		});
		expect(contrast(styles.foreground, styles.background)).toBeGreaterThanOrEqual(4.5);
		if (dark && process.env.DASHBOARD_VISUAL_QA) {
			await page.screenshot({ path: testInfo.outputPath('dashboard-mobile-dark.png'), fullPage: true });
		}
	}
	const orbitAnimation = await page.locator('.live-orbit').evaluate((element) => getComputedStyle(element, '::before').animationName);
	expect(orbitAnimation).toBe('none');
});

test('an older page from a previous Project scope is discarded', async ({ page }) => {
	await mockDashboard(page, { delayFeed: true, stream: false });
	await page.goto('/dashboard');
	await expect(page.getByRole('heading', { name: 'Control center' })).toBeVisible();
	await page.getByRole('button', { name: 'Load earlier activity' }).click();
	await page.locator('#dashboard-project').selectOption(projectId);
	await expect(page.locator('#dashboard-project')).toHaveValue(projectId);
	await page.waitForTimeout(400);
	await expect(page.locator('[data-feed-event="evt-older"]')).toHaveCount(0);
});

test('a failed scope switch never relabels stale dashboard data', async ({ page }) => {
	await mockDashboard(page, { failScopedSnapshot: true, stream: false });
	await page.goto('/dashboard');
	await expect(page.locator('[data-working-agents]')).toContainText('2');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toBeVisible();

	await page.locator('#dashboard-project').selectOption(projectId);
	await expect(page.getByRole('alert')).toContainText('Scoped dashboard unavailable');
	await expect(page.locator('#dashboard-project')).toHaveValue(projectId);
	await expect(page.locator('[data-working-agents]')).toContainText('0');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toHaveCount(0);
});

test('live activity during a slow summary refresh queues one trailing refresh', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { outOfOrderRefresh: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-working-agents]')).toContainText('2');

	await emitDashboardEvent(page, event({ cursor: 51, event_id: 'evt-refresh-one' }));
	await expect.poll(mocked.snapshotRequestCount).toBe(2);
	await emitDashboardEvent(page, event({ cursor: 52, event_id: 'evt-refresh-two' }));
	await page.waitForTimeout(100);
	expect(mocked.snapshotRequestCount()).toBe(2);

	mocked.releaseFirstRefresh();
	await expect.poll(mocked.snapshotRequestCount).toBe(3);
	await expect(page.locator('[data-working-agents]')).toContainText('9');
});

test('a refreshed snapshot removes deleted recent activity without dropping newer live events', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { deleteRecentEventOnRefresh: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toBeVisible();

	await emitDashboardEvent(page, event({ cursor: 51, event_id: 'evt-after-delete' }));
	await expect.poll(mocked.snapshotRequestCount).toBeGreaterThan(1);
	await expect(page.locator('[data-feed-event="evt-existing"]')).toHaveCount(0);
	await expect(page.locator('[data-feed-event="evt-after-delete"]')).toBeVisible();
});

test('a durable deletion removes activity from loaded older dashboard history', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { deleteOlderEventOnRefresh: true });
	await page.goto('/dashboard');
	await page.getByRole('button', { name: 'Load earlier activity' }).click();
	await expect(page.locator('[data-feed-event="evt-older"]')).toBeVisible();

	await emitDashboardEvent(page, event({ cursor: 51, event_id: 'evt-after-older-delete' }));
	await expect.poll(mocked.snapshotRequestCount).toBeGreaterThan(1);
	await expect(page.locator('[data-feed-event="evt-older"]')).toHaveCount(0);
	await expect(page.locator('[data-feed-event="evt-after-older-delete"]')).toBeVisible();
});

test('live activity coalesced into a failed summary refresh receives one retry', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { failFirstRefresh: true, outOfOrderRefresh: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-working-agents]')).toContainText('2');

	await emitDashboardEvent(page, event({ cursor: 51, event_id: 'evt-failed-refresh-one' }));
	await expect.poll(mocked.snapshotRequestCount).toBe(2);
	await emitDashboardEvent(page, event({ cursor: 52, event_id: 'evt-failed-refresh-two' }));
	await page.waitForTimeout(100);
	expect(mocked.snapshotRequestCount()).toBe(2);

	mocked.releaseFirstRefresh();
	await expect.poll(mocked.snapshotRequestCount).toBe(3);
	await expect(page.locator('[data-working-agents]')).toContainText('9');
});

test('an open stream returns to Live after an in-stream error recovers', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { outOfOrderRefresh: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Live');

	await emitDashboardStreamError(page);
	await expect.poll(mocked.snapshotRequestCount).toBe(2);
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Reconnecting');

	await emitDashboardCursor(page);
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Live');
	await emitDashboardStreamError(page);
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Reconnecting');

	mocked.releaseFirstRefresh();
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Live');
});

test('a successful transport-error probe does not claim the stream reopened', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { outOfOrderRefresh: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Live');

	await emitDashboardTransportError(page);
	await expect.poll(mocked.snapshotRequestCount).toBe(2);
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Reconnecting');
	mocked.releaseFirstRefresh();
	await expect(page.locator('[data-working-agents]')).toContainText('4');
	await expect(page.locator('[data-dashboard-connection]')).toContainText('Reconnecting');
});

test('summary refreshes keep the Project selector current', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { refreshProjects: true });
	await page.goto('/dashboard');
	await expect(page.getByRole('option', { name: 'Docs', exact: true })).toHaveCount(1);

	await emitDashboardEvent(page, event({ cursor: 51, event_id: 'evt-project-refresh' }));
	await expect.poll(mocked.snapshotRequestCount).toBeGreaterThan(1);
	await expect(page.getByRole('option', { name: 'Platform renamed', exact: true })).toHaveCount(1);
	await expect(page.getByRole('option', { name: 'New Project', exact: true })).toHaveCount(1);
	await expect(page.getByRole('option', { name: 'Docs', exact: true })).toHaveCount(0);
});

test('deleting the selected Project returns the dashboard to All Projects', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page);
	await page.goto('/dashboard');
	await page.locator('#dashboard-project').selectOption(projectId);
	await expect(page.locator('#dashboard-project')).toHaveValue(projectId);
	await expect.poll(() => mocked.scopes.filter((scope) => scope === projectId).length).toBe(1);

	mocked.deleteSelectedProject();
	await emitDashboardStreamError(page);

	await expect(page.locator('#dashboard-project')).toHaveValue('');
	await expect(page.getByRole('option', { name: 'Platform', exact: true })).toHaveCount(0);
	await expect(page.getByRole('option', { name: 'Docs', exact: true })).toHaveCount(1);
	await expect(page.locator('[data-working-agents]')).toContainText('0');
	await expect.poll(() => mocked.scopes.at(-1)).toBe('all');
});

test('All Projects refresh removes retained activity for a deleted Project', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page);
	await page.goto('/dashboard');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toBeVisible();

	mocked.deleteSelectedProject();
	await emitDashboardStreamError(page);

	await expect(page.getByRole('option', { name: 'Platform', exact: true })).toHaveCount(0);
	await expect(page.locator('[data-feed-event="evt-existing"]')).toHaveCount(0);
	await expect(page.locator('[data-feed-event="evt-attention"]')).toHaveCount(0);
	await expect(page.locator('[data-feed-event="evt-docs-survives"]')).toContainText('Docs activity remains available');
	await expect(page.locator('[data-live-feed] a[href="/tasks/task-dashboard"]')).toHaveCount(0);
});

test('a failed stream reconnect probes and recovers a deleted selected Project', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page);
	await page.goto('/dashboard');
	await page.locator('#dashboard-project').selectOption(projectId);
	await expect(page.locator('#dashboard-project')).toHaveValue(projectId);
	await expect.poll(() => mocked.scopes.filter((scope) => scope === projectId).length).toBe(1);

	mocked.deleteSelectedProject();
	await emitDashboardTransportError(page);

	await expect(page.locator('#dashboard-project')).toHaveValue('');
	await expect(page.getByRole('option', { name: 'Platform', exact: true })).toHaveCount(0);
	await expect(page.getByRole('option', { name: 'Docs', exact: true })).toHaveCount(1);
	await expect(page.locator('[data-working-agents]')).toContainText('0');
	await expect.poll(() => mocked.scopes.at(-1)).toBe('all');
});

test('repeated stream errors cannot starve a slow deleted-Project recovery', async ({ page }) => {
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { delayDeletedSnapshot: true });
	await page.goto('/dashboard');
	await page.locator('#dashboard-project').selectOption(projectId);
	await expect(page.locator('#dashboard-project')).toHaveValue(projectId);
	await expect.poll(mocked.snapshotRequestCount).toBe(2);

	mocked.deleteSelectedProject();
	await emitDashboardStreamError(page);
	await expect.poll(mocked.snapshotRequestCount).toBe(3);
	await emitDashboardStreamError(page);
	await emitDashboardTransportError(page);
	await page.waitForTimeout(100);
	expect(mocked.snapshotRequestCount()).toBe(3);

	mocked.releaseDeletedSnapshot();
	await expect(page.locator('#dashboard-project')).toHaveValue('');
	await expect.poll(() => mocked.scopes.at(-1)).toBe('all');
});

test('a quiet dashboard advances its rolling range and prunes expired activity', async ({ page }) => {
	await page.clock.install({ time: new Date(now) });
	await installMockEventSource(page);
	const mocked = await mockDashboard(page, { rollingWindow: true });
	await page.goto('/dashboard');
	await expect(page.locator('[data-working-agents]')).toContainText('2');
	await expect(page.locator('[data-feed-event="evt-existing"]')).toBeVisible();

	await page.clock.runFor(60_100);
	await expect.poll(mocked.snapshotRequestCount).toBeGreaterThan(1);
	await expect(page.locator('[data-working-agents]')).toContainText('0');
	await expect(page.locator('[data-feed-event]')).toHaveCount(0);
});

test('bounded history disables pagination cleanly at the client cap', async ({ page }) => {
	await mockDashboard(page, { manyFeedEvents: true, stream: false });
	await page.goto('/dashboard');
	const events = page.locator('[data-feed-event]');
	for (const expected of [101, 201, 301, 400]) {
		await page.getByRole('button', { name: 'Load earlier activity' }).click();
		await expect(events).toHaveCount(expected);
	}
	await expect(page.getByRole('button', { name: 'Load earlier activity' })).toHaveCount(0);
	await expect(page.locator('[data-feed-limit]')).toContainText('latest 400 events');
});
