import { expect, test, type Page } from '@playwright/test';

async function mockSetup(page: Page, os = 'macos', runtime = 'podman', failCheck = false) {
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/setup/check-docker' && failCheck) {
			await route.fulfill({ status: 503, json: { error: 'Runtime check unavailable' } });
			return;
		}
		const responses: Record<string, unknown> = {
			'/api/setup/system-info': { os, working_directory: '/workspace/project' },
			'/api/setup/check-docker': { available: true, runtime, installed: true, can_start: false },
			'/api/setup/agent-catalog': { agents: [] },
			'/api/setup/project-environment': { path: '/workspace/project', git_uses_ssh: true, suggestions: [] },
			'/api/setup/complete': { success: true },
			'/api/setup/add-session': { success: true, session_id: 'new-agent' },
		};
		await route.fulfill({ json: responses[path] ?? {} });
	});
}

for (const addAgent of [false, true]) {
	test(`macOS Podman SSH warning allows cancellation and opting in during ${addAgent ? 'add-agent' : 'setup'}`, async ({ page }, testInfo) => {
		await mockSetup(page);
		if (addAgent) await page.setViewportSize({ width: 390, height: 844 });
		await page.emulateMedia({ colorScheme: addAgent ? 'dark' : 'light' });
		await page.goto(addAgent ? '/setup?mode=add-session' : '/setup');
		const checkbox = page.getByLabel('Share my host SSH access', { exact: true });
		const dialog = page.getByRole('dialog', { name: 'SSH forwarding with Podman on macOS' });
		await expect(checkbox).toBeEnabled();
		await expect(checkbox).not.toBeChecked();
		await checkbox.click();
		await expect(dialog).toBeVisible();
		await expect(dialog).toContainText('known Podman limitation');
		await expect(dialog).toContainText('Switch to Docker Desktop');
		await expect(dialog.getByRole('link', { name: 'Podman issue #23785' })).toHaveAttribute('href', 'https://github.com/podman-container-tools/podman/issues/23785');
		await expect(dialog.getByRole('button', { name: 'Keep disabled' })).toBeFocused();
		await expect(checkbox).not.toBeChecked();
		await page.screenshot({ path: testInfo.outputPath('podman-ssh-warning.png') });
		await page.keyboard.press('Escape');
		await expect(dialog).not.toBeVisible();
		await expect(checkbox).not.toBeChecked();
		await expect(checkbox).toBeFocused();

		await checkbox.click();
		await dialog.getByRole('button', { name: 'Keep disabled' }).click();
		await expect(checkbox).not.toBeChecked();
		await checkbox.click();
		await dialog.getByRole('button', { name: 'Enable anyway' }).click();
		await expect(dialog).not.toBeVisible();
		await expect(checkbox).toBeChecked();

		// Each new opt-in warns again, allowing users to retry future releases.
		await checkbox.uncheck();
		await checkbox.click();
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Enable anyway' }).click();
		const endpoint = addAgent ? '/api/setup/add-session' : '/api/setup/complete';
		const saved = page.waitForRequest((request) => new URL(request.url()).pathname === endpoint && request.method() === 'POST');
		await page.getByRole('button', { name: addAgent ? 'Create agent' : 'Finish setup' }).click();
		const payload = (await saved).postDataJSON();
		expect(addAgent ? payload.ssh_agent_forwarding : payload.agents[0].ssh_agent_forwarding).toBe(true);
	});
}

for (const [os, runtime] of [['linux', 'podman'], ['macos', 'docker'], ['windows', 'podman']]) {
	test(`SSH access stays available without the macOS warning on ${os}/${runtime}`, async ({ page }) => {
		await mockSetup(page, os, runtime);
		await page.goto('/setup');
		const checkbox = page.getByLabel('Share my host SSH access', { exact: true });
		await checkbox.check();
		await expect(checkbox).toBeChecked();
		await expect(page.getByRole('dialog')).not.toBeVisible();
	});
}

test('a failed compatibility check does not block configuring SSH access', async ({ page }) => {
	await mockSetup(page, 'macos', 'podman', true);
	await page.goto('/setup');
	await expect(page.getByText('Could not check the container runtime.', { exact: false })).toBeVisible();
	await page.getByLabel('Share my host SSH access', { exact: true }).check();
	await expect(page.getByLabel('Share my host SSH access', { exact: true })).toBeChecked();
});
