import { expect, test } from '@playwright/test';

test.use({ serviceWorkers: 'block' });

test('pairing requires platform approval before publishing a selected local Agent', async ({ page }) => {
	let settings: any = { base_url: '', name: '', enabled: false, paired: false, bindings: [], pairing: null };
	let published: unknown;
	await page.route('**/api/**', async route => {
		const req = route.request(); const path = new URL(req.url()).pathname;
		const json = (body: unknown) => route.fulfill({ json: body });
		if (path === '/api/auth/bootstrap') return json({ authenticated: true, authentication_enabled: true, csrf_token: 'connect-csrf' });
		if (path === '/api/setup/status') return json({ setup_complete: true });
		if (path === '/api/setup/check-docker') return json({ available: true, installed: true });
		if (path === '/api/setup/config') return json({ llm: { providers: [] }, agents: [], mcp_servers: [], system: { budget: { daily: '0' } } });
		if (path === '/api/health') return json({ status: 'ok', version: 'test', build: 'test' });
		if (path === '/api/settings/profile') return json({ name: 'You', avatar: null });
		if (path === '/api/settings/connect/') return json(settings);
		if (path === '/api/settings/connect/pair') {
			expect(req.headers()['x-xpressclaw-csrf']).toBe('connect-csrf');
			settings = { ...settings, ...req.postDataJSON(), pairing: { user_code: 'verify-this-code', verification_url: 'https://platform.example/settings/connect?code=verify-this-code', expires_at: '2030-01-01', interval: 5 } };
			return json(settings);
		}
		if (path === '/api/settings/connect/pair/poll') { settings = { ...settings, paired: true, enabled: true, pairing: null }; return json(settings); }
		if (path === '/api/settings/connect/projects') return json([{ id: 'platform-project', name: 'Platform project' }]);
		if (path === '/api/projects') return json([{ id: 'local-project', name: 'Local project' }]);
		if (path === '/api/agents') return json([{ id: 'atlas', name: 'Atlas', project_id: 'local-project' }, { id: 'other', name: 'Other Agent', project_id: 'elsewhere' }]);
		if (path === '/api/settings/connect/bindings') {
			published = req.postDataJSON(); settings.bindings = [{ ...(published as object), id: 'binding', active: true }]; return json(settings);
		}
		return json([]);
	});
	await page.goto('/settings/connect');
	await page.getByLabel('Platform URL').fill('https://platform.example');
	await page.getByRole('button', { name: 'Connect account', exact: true }).click();
	await expect(page.getByText('verify-this-code', { exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Publish Agent', exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: 'I’ve approved the connection' }).click();
	await page.getByRole('combobox', { name: 'Local Project', exact: true }).selectOption('local-project');
	await expect(page.getByRole('combobox', { name: 'Local Agent', exact: true }).locator('option[value="other"]')).toHaveCount(0);
	await page.getByRole('combobox', { name: 'Local Agent', exact: true }).selectOption('atlas');
	await page.getByRole('combobox', { name: 'Xpress AI project', exact: true }).selectOption('platform-project');
	await page.getByLabel('Agent name in Xpress AI').fill('my-atlas');
	await page.getByRole('button', { name: 'Publish Agent', exact: true }).click();
	await expect(page.getByText('Agent published to Xpress AI.')).toBeVisible();
	expect(published).toEqual({ localProjectId: 'local-project', localAgentId: 'atlas', projectId: 'platform-project', agentName: 'my-atlas' });
});
