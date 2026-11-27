import { expect, test, type Page } from '@playwright/test';

const USAGE_FIXTURE = {
	fetched_at: '2026-11-26T12:00:00Z',
	accounts: [
		{
			id: 'claude:acct-1',
			provider: 'claude',
			provider_label: 'Claude',
			harness: 'Claude',
			account_label: 'dev@example.com',
			status: 'available',
			plan_label: 'Max 5x',
			observed_at: '2026-11-26T12:00:00Z',
			fetched_at: '2026-11-26T12:00:00Z',
			windows: [
				{
					id: 'five_hour',
					label: 'Session',
					short_label: '5h',
					summary: true,
					used_pct: 42,
					remaining_pct: 58,
					resets_at: '2099-11-26T16:00:00Z',
					duration_seconds: 18000,
					tone: 'ok',
				},
				{
					id: 'weekly',
					label: 'Weekly',
					short_label: 'wk',
					summary: true,
					used_pct: 82,
					remaining_pct: 18,
					resets_at: '2099-12-01T00:00:00Z',
					duration_seconds: 604800,
					tone: 'warning',
				},
			],
			balances: [],
			details: [{ id: 'extra_usage', label: 'Extra usage', value: 'Enabled' }],
			problem: null,
			error: null,
			last_successful: null,
		},
		{
			id: 'codex:acct-2',
			provider: 'codex',
			provider_label: 'Codex',
			harness: 'Codex',
			account_label: null,
			status: 'available',
			plan_label: 'Plus',
			observed_at: '2026-11-26T12:00:00Z',
			fetched_at: '2026-11-26T12:00:00Z',
			windows: [
				{
					id: 'five_hour',
					label: '5-hour',
					short_label: '5h',
					summary: true,
					used_pct: 15,
					remaining_pct: 85,
					resets_at: '2099-11-26T15:00:00Z',
					duration_seconds: 18000,
					tone: 'ok',
				},
			],
			balances: [{ id: 'credits', label: 'Credits', remaining: 12.5, unit: 'credits', tone: 'ok' }],
			details: [],
			problem: null,
			error: null,
			last_successful: null,
		},
	],
	deferred_automations: [
		{
			kind: 'workflow',
			id: 'wf-nightly',
			name: 'Nightly audit',
			provider: 'claude',
			resume_at: '2099-11-26T16:01:00Z',
			reason: 'claude Session has 8% remaining (needs ≥20%)',
		},
	],
};

async function mockAppApi(page: Page) {
	let usageRefreshCalls = 0;
	let createdScheduleBody: Record<string, unknown> | null = null;

	await page.route('**/api/**', async (route) => {
		const url = new URL(route.request().url());
		const path = url.pathname;
		const method = route.request().method();

		if (path === '/api/auth/bootstrap') {
			return route.fulfill({ json: { authentication_enabled: false, authenticated: true } });
		}
		if (path === '/api/health') {
			return route.fulfill({ json: { status: 'ok', docker: { available: true, installed: true } } });
		}
		if (path === '/api/setup/check-docker') {
			return route.fulfill({ json: { available: true, installed: true, can_start: false } });
		}
		if (path === '/api/projects') {
			return route.fulfill({ json: [{ id: 'proj-1', name: 'Core', description: null, icon: null }] });
		}
		if (path === '/api/agents') {
			return route.fulfill({
				json: [
					{ id: 'agent-1', name: 'atlas', title: 'Atlas', backend: 'claude', status: 'working', project_id: 'proj-1', config: { runner: { kind: 'claude' } } },
					{ id: 'agent-2', name: 'scout', title: 'Scout', backend: 'codex', status: 'idle', project_id: 'proj-1', config: { runner: { kind: 'codex' } } },
				],
			});
		}
		if (path === '/api/conversations') {
			return route.fulfill({ json: [] });
		}
		if (path === '/api/tasks' || path === '/api/tasks/recent-by-agent') {
			return route.fulfill({
				json: {
					tasks: [
						{ id: 'task-1', title: 'Queued task', status: 'pending', activity_status: 'queued', agent_id: 'agent-1', project_id: 'proj-1', created_at: '2026-11-26T12:00:00Z', updated_at: '2026-11-26T12:00:00Z' },
					],
				},
			});
		}
		if (path === '/api/tasks/attention') {
			return route.fulfill({ json: { tasks: [] } });
		}
		if (path === '/api/workflows') {
			return route.fulfill({
				json: [
					{
						id: 'wf-nightly',
						name: 'Nightly audit',
						description: 'Quota-gated nightly check',
						yaml_content: 'name: Nightly audit\nschedule:\n  cron: "0 2 * * *"\n  quota_gate:\n    min_remaining_pct: 20\nflows:\n  main:\n    steps:\n      - id: s1\n        agent: agent-1\n        prompt: Audit\n',
						enabled: true,
						default_for_tasks: false,
						version: 1,
						created_at: '2026-11-26T10:00:00Z',
						updated_at: '2026-11-26T10:00:00Z',
						last_triggered_at: null,
						trigger_count: 0,
						trigger_error: null,
						quota_deferred_until: '2099-11-26T16:01:00Z',
						quota_deferred_reason: 'claude Session has 8% remaining (needs ≥20%)',
					},
				],
			});
		}
		if (path === '/api/schedules' && method === 'GET') {
			return route.fulfill({ json: [] });
		}
		if (path === '/api/schedules' && method === 'POST') {
			createdScheduleBody = route.request().postDataJSON() as Record<string, unknown>;
			return route.fulfill({
				status: 201,
				json: {
					id: 'sched-1',
					...createdScheduleBody,
					enabled: true,
					last_run: null,
					run_count: 0,
					created_at: '2026-11-26T12:00:00Z',
					schedule_type: 'cron',
					run_at: null,
					continuation_task_id: null,
					conversation_id: null,
				},
			});
		}
		if (path === '/api/dashboard/resources') {
			return route.fulfill({
				json: {
					sampled_at: '2026-11-26T12:00:00Z',
					cpu_percent: 37,
					cpu_count: 8,
					memory: {
						total_bytes: 16 * 1024 ** 3,
						available_bytes: 6 * 1024 ** 3,
						used_bytes: 10 * 1024 ** 3,
						used_percent: 62.5,
					},
					disks: [
						{
							locations: ['Data', 'Workspaces'],
							mount_point: '/',
							capacity: {
								total_bytes: 500 * 1024 ** 3,
								available_bytes: 260 * 1024 ** 3,
								used_bytes: 240 * 1024 ** 3,
								used_percent: 48,
							},
						},
					],
				},
			});
		}
		if (path === '/api/usage') {
			if (url.searchParams.get('refresh') === 'true') usageRefreshCalls += 1;
			return route.fulfill({ json: USAGE_FIXTURE });
		}
		if (path.startsWith('/api/ui/')) {
			return route.fulfill({ json: {} });
		}
		return route.fulfill({ json: {} });
	});

	return {
		getUsageRefreshCalls: () => usageRefreshCalls,
		getCreatedScheduleBody: () => createdScheduleBody,
	};
}

test('bottom status bar displays agents, resources, quotas popover, and automation quota gates', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	const state = await mockAppApi(page);

	await page.goto('/automations');
	const bar = page.locator('[data-workspace-statusbar]');
	await expect(bar).toBeVisible({ timeout: 15_000 });

	// Left zone: agents, queued tasks, and quota-waiting count.
	await expect(bar.locator('[data-statusbar-agents]')).toContainText('1/2 agents');
	await expect(bar.locator('[data-statusbar-queued]')).toContainText('1 queued');
	await expect(bar.locator('[data-statusbar-quota-waits]')).toContainText('1 waiting on quota');

	// Right zone: CPU, Memory, Disk.
	await expect(bar.locator('[data-statusbar-cpu]')).toContainText('CPU 37%');
	await expect(bar.locator('[data-statusbar-memory]')).toContainText('MEM 63%');
	await expect(bar.locator('[data-statusbar-disk]')).toContainText('DISK 48%');

	// Center zone: pinned quota pills for Claude and Codex.
	await expect(bar.locator('[data-statusbar-quota-pill="claude"]')).toContainText('42%');
	await expect(bar.locator('[data-statusbar-quota-pill="codex"]')).toContainText('15%');

	// Open Harness Quotas popover, switch to Remaining %, and trigger Refresh.
	await bar.locator('[data-statusbar-quotas]').click();
	const popover = bar.locator('[data-statusbar-quotas-popover]');
	await expect(popover).toBeVisible();
	await expect(popover).toContainText('Waiting for quota reset (1)');
	await expect(popover).toContainText('Nightly audit');
	await expect(popover).toContainText('Max 5x');

	await popover.getByRole('button', { name: 'Left' }).click();
	await expect(bar.locator('[data-statusbar-quota-pill="claude"]')).toContainText('58%');

	await popover.getByRole('button', { name: 'Refresh' }).click();
	await expect.poll(() => state.getUsageRefreshCalls()).toBe(1);

	// Close popover with Escape and verify Automations page shows deferred workflow badge & quota gate form.
	await page.keyboard.press('Escape');
	await expect(popover).toBeHidden();
	await expect(page.locator('[data-workflow-quota-deferred]')).toContainText('Waiting for quota reset');

	await page.getByRole('button', { name: 'New Schedule' }).first().click();
	const form = page.locator('[data-schedule-form]');
	await form.getByLabel('Name').fill('Nightly cleanup');
	await form.getByRole('textbox', { name: 'Cron' }).fill('0 3 * * *');
	await form.getByLabel('Task title').fill('Run cleanup');
	await form.locator('[data-schedule-quota-gate-toggle]').check();
	await form.getByRole('button', { name: 'Create schedule' }).click();

	await expect.poll(() => state.getCreatedScheduleBody()).not.toBeNull();
	expect(state.getCreatedScheduleBody()?.quota_gate).toEqual({
		min_remaining_pct: 20,
		window: 'any',
		on_limit: 'wait_for_reset',
	});
});
