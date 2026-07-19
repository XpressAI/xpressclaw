import { defineConfig, devices } from '@playwright/test';

const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH;

export default defineConfig({
	testDir: './e2e',
	fullyParallel: true,
	forbidOnly: Boolean(process.env.CI),
	retries: process.env.CI ? 2 : 0,
	reporter: process.env.CI ? 'github' : 'list',
	use: {
		baseURL: 'http://127.0.0.1:4173',
		trace: 'retain-on-failure',
		launchOptions: executablePath ? { executablePath } : {},
	},
	projects: [
		{
			name: 'chromium',
			use: { ...devices['Desktop Chrome'] },
		},
	],
	webServer: {
		command: 'npm run dev -- --host 127.0.0.1 --port 4173',
		url: 'http://127.0.0.1:4173',
		reuseExistingServer: !process.env.CI,
	},
});
