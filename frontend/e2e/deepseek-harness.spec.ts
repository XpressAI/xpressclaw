import { expect, test } from '@playwright/test';

const catalog = {
	agents: [
		{
			kind: 'codex', name: 'Codex', mark: 'C', description: 'Codex over ACP.',
			command: ['codex-acp'], login_command: 'codex login', install_url: 'https://developers.openai.com/codex/cli/',
			image: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest', host_image: 'ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest',
			installed: true, configured: true, status: 'ready', executable: 'codex',
		},
		{
			kind: 'deepseek-harness', name: 'DeepSeek Harness', mark: 'DS',
			description: "DeepSeek Harness through openma-ai's maintained ACP adapter.",
			command: ['dsh-acp'], login_command: 'dsh-acp login',
			install_url: 'https://github.com/openma-ai/deepseek-harness-acp',
			image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest',
			host_image: 'ghcr.io/xpressai/xpressclaw-runner-deepseek-harness-docker:latest',
			installed: true, configured: true, status: 'ready', executable: 'dsh-acp',
		},
	],
};

test('DeepSeek Harness is shown during setup and can be selected when adding an Agent', async ({ page }) => {
	let submitted: Record<string, unknown> | null = null;
	let sshAgentAvailable = false;
	let gitUsesSsh = true;
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		let body: unknown;
		if (path === '/api/setup/system-info') {
			body = {
				os: 'linux', arch: 'x86_64', working_directory: '/srv/repos/platform',
				ssh_agent_available: sshAgentAvailable, ssh_agent_socket: sshAgentAvailable ? '/run/user/1000/ssh-agent.socket' : null,
			};
		} else if (path === '/api/setup/agent-catalog') {
			body = catalog;
		} else if (path === '/api/setup/check-docker') {
			body = {
				available: true, installed: true, can_start: false, runtime: 'docker',
				version: '29.6.1', socket: '/var/run/docker.sock', rootless: false, error: null,
			};
		} else if (path === '/api/setup/project-environment') {
			body = { path: '/srv/repos/platform', git_remote: gitUsesSsh ? 'git@github.com:XpressAI/platform.git' : 'https://github.com/XpressAI/platform.git', git_uses_ssh: gitUsesSsh, suggestions: [] };
		} else if (path === '/api/setup/add-session') {
			submitted = request.postDataJSON() as Record<string, unknown>;
			body = { success: true, session: 'platform-dsh', session_id: 'platform-dsh', title: 'Platform DSH', project_id: 'platform' };
		} else {
			body = {};
		}
		await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
	});

	await page.goto('/setup');
	await expect(page.getByText("This repository has an SSH remote. GitHub repositories can use XpressClaw's scoped GitHub credential; use an HTTPS remote for other hosts.")).toBeVisible();
	await expect(page.getByText('Use my host SSH agent')).toHaveCount(0);
	await expect(page.getByText(/Start ssh-agent/)).toHaveCount(0);
	sshAgentAvailable = true;
	await page.reload();
	await expect(page.getByText('/run/user/1000/ssh-agent.socket')).toBeVisible();
	await page.getByLabel('Use my host SSH agent').check();
	gitUsesSsh = false;
	await page.getByRole('button', { name: /Inspect|Rescan/ }).click();
	await expect(page.getByLabel('Use my host SSH agent')).toBeChecked();
	await page.getByLabel('Use my host SSH agent').uncheck();
	await expect(page.getByLabel('Use my host SSH agent')).toBeVisible();
	await expect(page.getByRole('button', { name: /DeepSeek Harness/ })).toContainText('DS');
	await expect(page.getByRole('button', { name: /DeepSeek Harness/ })).toContainText("openma-ai's maintained ACP adapter");

	await page.goto('/setup?mode=add-session&project_id=platform');
	const dsh = page.getByRole('button', { name: /DeepSeek Harness/ });
	await expect(dsh).toContainText('DS');
	await expect(dsh).toContainText("openma-ai's maintained ACP adapter");
	await dsh.click();
	await expect(page.getByText('Use my existing DeepSeek Harness login')).toBeVisible();
	await page.getByRole('button', { name: 'Create agent' }).click();
	await expect.poll(() => submitted).not.toBeNull();
	const payload = submitted as Record<string, unknown>;
	expect(payload.runner_kind).toBe('deepseek-harness');
	expect(payload.backend).toBe('deepseek-harness');
	expect(payload.runner_image).toBe('ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest');
	expect(payload.subscription_auth).toBe(true);
});
