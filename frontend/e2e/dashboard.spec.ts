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
	manyFeedEvents?: boolean;
	stream?: boolean;
} = {}) {
	const scopes: string[] = [];
	let snapshotRequests = 0;
	let feedRequests = 0;
	await page.route('**/api/**', async (route) => {
		const url = new URL(route.request().url());
		if (url.pathname === '/api/dashboard/snapshot') {
			snapshotRequests += 1;
			scopes.push(url.searchParams.get('project_id') ?? 'all');
			if (options.delaySnapshot && snapshotRequests === 1) await new Promise((resolve) => setTimeout(resolve, 800));
			return route.fulfill({ json: snapshot(Boolean(options.empty), options.manyFeedEvents ? 1_000 : 40) });
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
		if (url.pathname === '/api/setup/check-docker') return route.fulfill({ json: { available: true, installed: true, can_start: false } });
		if (url.pathname === '/api/projects') return route.fulfill({ json: [] });
		if (url.pathname === '/api/conversations') return route.fulfill({ json: [] });
		if (url.pathname === '/api/agents') return route.fulfill({ json: [] });
		if (url.pathname === '/api/tasks/recent-by-agent') return route.fulfill({ json: { tasks: [] } });
		if (url.pathname === '/api/tasks') return route.fulfill({ json: { tasks: [], counts: {} } });
		if (url.pathname === '/api/workflows' || url.pathname === '/api/schedules') return route.fulfill({ json: [] });
		return route.fulfill({ json: {} });
	});
	return scopes;
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
	const scopes = await mockDashboard(page);
	await page.goto('/projects');
	await page.locator('a[aria-label="Open Control center"]').first().click();
	await expect(page).toHaveURL(/\/dashboard$/);
	await expect(page.getByRole('heading', { name: 'Control center' })).toBeVisible();
	await expect(page.locator('[data-kpi="working-agents"]')).toContainText('2');
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
		const styles = await page.locator('[data-kpi="working-agents"]').evaluate((element) => {
			const computed = getComputedStyle(element);
			const label = getComputedStyle(element.querySelector('.kpi-top')!);
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

test('bounded history disables pagination cleanly at the client cap', async ({ page }) => {
	await mockDashboard(page, { manyFeedEvents: true, stream: false });
	await page.goto('/dashboard');
	const events = page.locator('[data-feed-event]');
	for (const expected of [102, 202, 302, 400]) {
		await page.getByRole('button', { name: 'Load earlier activity' }).click();
		await expect(events).toHaveCount(expected);
	}
	await expect(page.getByRole('button', { name: 'Load earlier activity' })).toHaveCount(0);
	await expect(page.locator('[data-feed-limit]')).toContainText('latest 400 events');
});
