import { expect, test, type Page } from '@playwright/test';
import type { ProxySettings } from '../src/lib/api';

function fixture(): ProxySettings {
	return {
		config: { enabled: true, web_ports: [80, 443], network_rules: [{ name: 'SSH', enabled: true, protocol: 'tcp', destination: '*', ports: '22' }], domain_policy: 'allow_except_blocked', allowed_domains: [], blocked_domains: [], inspection_bypass: [], header_rules: [
			{ header: 'User-Agent', matcher: 'email', value: '', action: 'replace', replacement: 'XpressCLAW/1.0', domains: [] },
			{ header: 'From', matcher: 'any', value: '', action: 'remove', replacement: '', domains: [] }
		] },
		status: { state: 'running', image: 'mitmproxy', events: [{ time: 1793900000, action: 'header_replace', host: 'example.com', rule: 1, client: '10.0.0.2' }] },
		direct_agents: ['Compatibility agent'], host_engine_agents: []
	};
}

async function mock(page: Page, fail = false) {
	const data = fixture();
	await page.route('**/api/**', async route => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/settings/proxy') {
			if (route.request().method() === 'PUT') {
				if (fail) return route.fulfill({ status: 400, json: { error: 'Invalid domain: https://example.com' } });
				data.config = route.request().postDataJSON();
			}
			return route.fulfill({ json: data });
		}
		if (path.startsWith('/api/settings/proxy')) {
			return route.fulfill({ status: 404, json: { error: 'API route not found' } });
		}
		const responses: Record<string, unknown> = {
			'/api/auth/bootstrap': { authentication_enabled: false, authenticated: true },
			'/api/setup/status': { setup_complete: true },
			'/api/setup/check-docker': { available: true },
			'/api/health': { status: 'ok', version: 'test' },
			'/api/agents': [], '/api/projects': [], '/api/conversations': [], '/api/tasks': [], '/api/workflows': [], '/api/schedules': []
		};
		await route.fulfill({ json: responses[path] ?? {} });
	});
	return data;
}

test('proxy settings save domain and scoped header rules and survive reload', async ({ page }, info) => {
	const data = await mock(page);
	await page.goto('/settings/proxy');
	const view = page.getByTestId('proxy-settings');
	await expect(view.getByRole('heading', { name: 'Proxy', exact: true })).toBeVisible();
	await expect(view.getByText('Compatibility agent', { exact: false })).toBeVisible();
	await view.getByLabel('Default policy').selectOption('allow_listed_only');
	await view.getByLabel('Allowed domains', { exact: true }).fill('example.com');
	await view.getByLabel('Allowed domains', { exact: true }).press('End');
	await view.getByLabel('Allowed domains', { exact: true }).press('Enter');
	await view.getByLabel('Allowed domains', { exact: true }).pressSequentially('*.example.com');
	await view.getByLabel('Blocked domains', { exact: true }).fill('blocked.example.com');
	await view.getByLabel('Skip HTTPS inspection', { exact: true }).fill('pinned.example.com');
	await view.getByRole('button', { name: 'Add rule' }).click();
	const rule = view.getByTestId('header-rule').nth(2);
	await rule.getByLabel('Header name').fill('X-Identity');
	await rule.getByLabel('Match', { exact: true }).selectOption('contains');
	await rule.getByLabel('Match value').fill('private');
	await rule.getByLabel('Action').selectOption('block');
	await rule.getByLabel('Domains (optional)').fill('example.com');
	await view.getByRole('button', { name: 'Save proxy settings' }).click();
	await expect(view.getByRole('status')).toContainText('Proxy settings saved');
	expect(data.config.allowed_domains).toEqual(['example.com', '*.example.com']);
	expect(data.config.inspection_bypass).toEqual(['pinned.example.com']);
	expect(data.config.header_rules[2]).toMatchObject({ header: 'X-Identity', matcher: 'contains', value: 'private', action: 'block', domains: ['example.com'] });
	await page.reload();
	await expect(view.getByLabel('Allowed domains', { exact: true })).toHaveValue('example.com\n*.example.com');
	await expect(view.getByTestId('header-rule')).toHaveCount(3);
	await page.screenshot({ path: info.outputPath('proxy-settings.png'), fullPage: true });
});

test('invalid proxy settings keep unsaved edits and show the server error', async ({ page }) => {
	await mock(page, true);
	await page.goto('/settings/proxy');
	await page.getByLabel('Blocked domains', { exact: true }).fill('https://example.com');
	await page.getByRole('button', { name: 'Save proxy settings' }).click();
	await expect(page.getByRole('alert')).toContainText('Invalid domain');
	await expect(page.getByLabel('Blocked domains', { exact: true })).toHaveValue('https://example.com');
	await expect(page.getByRole('button', { name: 'Save proxy settings' })).toBeEnabled();
});

test('proxy settings fit a narrow viewport', async ({ page }, info) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mock(page);
	await page.goto('/settings/proxy');
	await expect(page.getByTestId('header-rule')).toHaveCount(2);
	expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
	await page.screenshot({ path: info.outputPath('proxy-mobile.png'), fullPage: true });
});

test('network permissions toggle SSH and save TCP, UDP, and web port controls', async ({ page }, info) => {
	const data = await mock(page);
	await page.goto('/settings/proxy');
	const ssh = page.getByTestId('network-rule').first();
	await expect(ssh.getByLabel('Enable SSH', { exact: true })).toBeChecked();
	await expect(ssh.getByLabel('Ports', { exact: true })).toHaveValue('22');
	await page.getByTestId('network-access-settings').screenshot({ path: info.outputPath('network-access.png') });
	await ssh.getByLabel('Enable SSH', { exact: true }).uncheck();
	await page.getByRole('button', { name: 'Add PostgreSQL' }).click();
	const database = page.getByTestId('network-rule').nth(1);
	await database.getByLabel('Destination', { exact: true }).fill('db.example.com');
	await expect(database.getByLabel('Ports', { exact: true })).toHaveValue('5432');
	await page.getByRole('button', { name: 'Add DNS' }).click();
	const dns = page.getByTestId('network-rule').nth(2);
	await dns.getByLabel('Destination', { exact: true }).fill('192.0.2.53/32');
	await expect(dns.getByLabel('Protocol', { exact: true })).toHaveValue('udp');
	await page.getByLabel('Web proxy ports', { exact: true }).fill('80,443,8443');
	await page.getByRole('button', { name: 'Save proxy settings' }).click();
	await expect(page.getByTestId('proxy-settings').getByRole('status')).toContainText('Proxy settings saved');
	expect(data.config.network_rules).toEqual([
		{ name: 'SSH', enabled: false, protocol: 'tcp', destination: '*', ports: '22' },
		{ name: 'PostgreSQL', enabled: true, protocol: 'tcp', destination: 'db.example.com', ports: '5432' },
		{ name: 'DNS', enabled: true, protocol: 'udp', destination: '192.0.2.53/32', ports: '53' }
	]);
	expect(data.config.web_ports).toEqual([80, 443, 8443]);
	await page.reload();
	await expect(page.getByLabel('Enable SSH', { exact: true })).not.toBeChecked();
	await expect(page.getByTestId('network-rule')).toHaveCount(3);
});
