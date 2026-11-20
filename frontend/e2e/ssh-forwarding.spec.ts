import { expect, test, type Page } from '@playwright/test';

const PODMAN_MACOS_REASON =
	'Podman on macOS cannot forward a host SSH agent into a container, because virtiofs does not proxy Unix socket connections into the Podman machine VM';

async function mockSetup(page: Page, unsupportedReason: string | null = PODMAN_MACOS_REASON) {
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		const responses: Record<string, unknown> = {
			'/api/setup/system-info': { os: 'macos', working_directory: '/workspace/project' },
			'/api/setup/check-docker': {
				available: true,
				runtime: 'podman',
				installed: true,
				can_start: false,
				ssh_agent_forwarding_unsupported_reason: unsupportedReason
			},
			'/api/setup/agent-catalog': { agents: [] },
			'/api/setup/project-environment': {
				path: '/workspace/project',
				git_uses_ssh: true,
				suggestions: []
			},
			'/api/setup/complete': { success: true },
			'/api/setup/add-session': { success: true, session_id: 'new-agent' }
		};
		await route.fulfill({ json: responses[path] ?? {} });
	});
}

for (const addAgent of [false, true]) {
	test(`an unsupported host SSH agent is explained without blocking opt-in during ${addAgent ? 'add-agent' : 'setup'}`, async ({
		page
	}, testInfo) => {
		await mockSetup(page);
		if (addAgent) await page.setViewportSize({ width: 390, height: 844 });
		await page.emulateMedia({ colorScheme: addAgent ? 'dark' : 'light' });
		await page.goto(addAgent ? '/setup?mode=add-session' : '/setup');

		const checkbox = page.getByLabel('Share my host SSH access', { exact: true });
		// The checkbox is never gated on a runtime probe.
		await expect(checkbox).toBeEnabled();
		await expect(checkbox).not.toBeChecked();

		// The limitation is stated up front, where it can inform the choice,
		// rather than interrupting after the click.
		const notice = page.getByText(PODMAN_MACOS_REASON, { exact: false });
		await expect(notice).toBeVisible();
		await expect(notice).toContainText('Key files in ~/.ssh are still shared');
		await expect(page.getByRole('dialog')).toHaveCount(0);
		await expect(page.getByRole('link', { name: 'Podman issue #23785' })).toHaveAttribute(
			'href',
			'https://github.com/containers/podman/issues/23785'
		);
		await page.screenshot({ path: testInfo.outputPath('podman-ssh-notice.png') });

		await checkbox.check();
		await expect(checkbox).toBeChecked();
		// The trust warning is still shown once access is actually granted.
		await expect(page.getByText('Enable this only for harnesses and tasks you trust')).toBeVisible();

		const endpoint = addAgent ? '/api/setup/add-session' : '/api/setup/complete';
		const saved = page.waitForRequest(
			(request) => new URL(request.url()).pathname === endpoint && request.method() === 'POST'
		);
		await page.getByRole('button', { name: addAgent ? 'Create agent' : 'Finish setup' }).click();
		const payload = (await saved).postDataJSON();
		expect(addAgent ? payload.ssh_agent_forwarding : payload.agents[0].ssh_agent_forwarding).toBe(
			true
		);
	});
}

test('a supported host shows no SSH forwarding limitation', async ({ page }) => {
	await mockSetup(page, null);
	await page.goto('/setup');
	const checkbox = page.getByLabel('Share my host SSH access', { exact: true });
	await checkbox.check();
	await expect(checkbox).toBeChecked();
	await expect(page.getByText('cannot forward a host SSH agent', { exact: false })).toHaveCount(0);
});

test('an unreachable container runtime does not block configuring SSH access', async ({ page }) => {
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/setup/check-docker') {
			await route.fulfill({ status: 503, json: { error: 'Runtime check unavailable' } });
			return;
		}
		const responses: Record<string, unknown> = {
			'/api/setup/system-info': { os: 'macos', working_directory: '/workspace/project' },
			'/api/setup/agent-catalog': { agents: [] },
			'/api/setup/project-environment': {
				path: '/workspace/project',
				git_uses_ssh: true,
				suggestions: []
			}
		};
		await route.fulfill({ json: responses[path] ?? {} });
	});
	await page.goto('/setup');
	const checkbox = page.getByLabel('Share my host SSH access', { exact: true });
	await expect(checkbox).toBeEnabled();
	await checkbox.check();
	await expect(checkbox).toBeChecked();
});
