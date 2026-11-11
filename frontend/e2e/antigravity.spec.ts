import { expect, test } from '@playwright/test';

test('Antigravity setup explains login and creates the built-in runner', async ({ page }) => {
	let submitted: Record<string, unknown> | null = null;
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		let body: unknown = {};
		if (path === '/api/setup/agent-catalog') body = { agents: [{
			kind: 'antigravity', name: 'Google Antigravity', mark: 'AG',
			description: "Google's Antigravity CLI with its native ACP server.",
			command: ['agy', 'acp', '--process-per-session'], login_command: 'agy acp --login',
			install_url: 'https://antigravity.google/docs/cli/acp/',
			image: 'ghcr.io/xpressai/xpressclaw-runner-antigravity:latest',
			host_image: 'ghcr.io/xpressai/xpressclaw-runner-antigravity-docker:latest',
			installed: true, configured: false, status: 'sign_in', executable: 'agy',
		}] };
		else if (path === '/api/setup/system-info') body = { os: 'linux', arch: 'x86_64', working_directory: '/srv/project', ssh_agent_available: false };
		else if (path === '/api/setup/check-docker') body = { available: true, installed: true, runtime: 'docker', version: '29.6.1' };
		else if (path === '/api/setup/project-environment') body = { path: '/srv/project', suggestions: [] };
		else if (path === '/api/setup/add-session') {
			submitted = route.request().postDataJSON();
			body = { success: true, session: 'google-agent', session_id: 'google-agent', title: 'Google Agent', project_id: 'project' };
		}
		await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
	});
	await page.goto('/setup?mode=add-session&project_id=project');
	await page.getByRole('button', { name: /Google Antigravity/ }).click();
	await expect(page.getByText('agy acp --login').first()).toBeVisible();
	await expect(page.getByText('GEMINI_API_KEY', { exact: true })).toBeVisible();
	await expect(page.getByText('"modelProvider": "gemini"', { exact: true })).toBeVisible();
	await page.getByRole('checkbox', { name: /Use my existing Google Antigravity login/ }).uncheck();
	await page.getByRole('button', { name: 'Create agent' }).click();
	await expect.poll(() => submitted).not.toBeNull();
	expect(submitted).toMatchObject({
		runner_kind: 'antigravity', backend: 'antigravity', subscription_auth: false,
		runner_image: 'ghcr.io/xpressai/xpressclaw-runner-antigravity:latest',
	});
});
