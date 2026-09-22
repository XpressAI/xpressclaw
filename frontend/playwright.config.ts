import { defineConfig, devices } from '@playwright/test';

// The executable override is Chromium-specific. It is spread into the Chromium
// projects only; putting it in the top-level `use` would hand the Chromium
// binary to the WebKit browser type, which fails at launch.
const chromiumExecutablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH;
const chromiumLaunch = chromiumExecutablePath ? { launchOptions: { executablePath: chromiumExecutablePath } } : {};

export default defineConfig({
	testDir: './e2e',
	fullyParallel: true,
	forbidOnly: Boolean(process.env.CI),
	retries: process.env.CI ? 2 : 0,
	reporter: process.env.CI ? 'github' : 'list',
	use: {
		baseURL: 'http://127.0.0.1:4173',
		trace: 'retain-on-failure',
	},
	projects: [
		{
			name: 'chromium',
			use: { ...devices['Desktop Chrome'], ...chromiumLaunch },
		},
		// Safari and the desktop app's WKWebView are supported engines, and
		// gesture handling is where engines differ most, so the tab-strip suite
		// runs on WebKit as well as Chromium.
		{
			name: 'webkit',
			use: { ...devices['Desktop Safari'] },
			testMatch: /workspace\.spec\.ts/,
			// Only the tab-strip suite is written to run on WebKit; other tests carry
			// Chromium-only assumptions and would need their own porting work.
			grep: /dragging a tab|abandons the drag|marks where it will land|compact tab strip|compact strip|never starts a drag|freshly split pane|drag in flight|deleted mid-drag|touch swipe/,
		},
		// Chromium can inject real touch input through CDP, including native
		// panning, which WebKit's driver cannot; this profile reproduces phone
		// scrolling faithfully enough to catch a drag sensor that blocks it.
		{
			name: 'touch-chromium',
			use: { ...devices['iPhone 14'], browserName: 'chromium', ...chromiumLaunch },
			testMatch: /workspace\.spec\.ts/,
			grep: /touch swipe/,
		},
	],
	webServer: {
		command: 'npm run dev -- --host 127.0.0.1 --port 4173',
		url: 'http://127.0.0.1:4173',
		reuseExistingServer: !process.env.CI,
	},
});
