import { expect, test, type Page, type Route } from '@playwright/test';

// Service-worker fetches bypass Playwright's page-level API mocks.
test.use({ serviceWorkers: 'block' });

const timestamp = '2026-09-24T12:00:00Z';
const agent = { id: 'voice-agent', name: 'Voice Agent', title: 'Voice Agent', backend: 'codex', project_id: null, status: 'stopped', config: {}, created_at: timestamp };
const conversation = { id: 'voice-chat', title: 'Voice chat', project_id: null, participants: [], created_at: timestamp, updated_at: timestamp };
const task = { id: 'voice-task', title: 'Voice task', description: '', status: 'completed', activity_status: 'completed', agent_id: agent.id, created_at: timestamp, updated_at: timestamp, context: {}, depends_on: [], dependents: [], blocked_by: [], ready: true };
const defaults = { enabled: true, base_url: 'https://speech.example/v1', has_api_key: true, stt_model: 'local-stt', tts_model: 'local-tts', voice: 'test-voice' };

async function json(route: Route, body: unknown, status = 200) {
	await route.fulfill({ status, json: body });
}

async function mockApi(page: Page, options: { enabled?: boolean; reply?: string } = {}) {
	const state = { settings: { ...defaults, enabled: options.enabled ?? true }, saved: [] as Record<string, unknown>[], sent: [] as unknown[] };
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		if (path === '/api/auth/bootstrap') return json(route, { authenticated: true, authentication_enabled: true, csrf_token: 'speech-csrf' });
		if (path === '/api/health') return json(route, { status: 'ok', version: 'test', build: 'test' });
		if (path === '/api/setup/check-docker') return json(route, { available: true, installed: true });
		if (path === '/api/setup/status') return json(route, { setup_complete: true });
		if (path === '/api/setup/config') return json(route, { llm: { providers: [] }, agents: [], mcp_servers: [], system: { budget: { daily: '0' } } });
		if (path === '/api/settings/profile') return json(route, { name: 'You', avatar: null });
		if (path === '/api/settings/speech') {
			if (request.method() === 'PUT') {
				expect(request.headers()['x-xpressclaw-csrf']).toBe('speech-csrf');
				const payload = request.postDataJSON();
				state.saved.push(payload);
				const { api_key, ...fields } = payload;
				state.settings = { ...fields, has_api_key: api_key === undefined ? state.settings.has_api_key : !!api_key };
			}
			return json(route, state.settings);
		}
		if (path === '/api/agents') return json(route, [agent]);
		if (path === '/api/agents/voice-agent') return json(route, agent);
		if (path === '/api/sessions/voice-agent') return json(route, { session: { status: 'idle' }, active_attempts: [], queued_attempts: [], recent_attempts: [], recent_events: [], artifacts: [] });
		if (path === '/api/sessions/voice-agent/readiness') return json(route, { ready: true });
		if (path === '/api/conversations') return json(route, [conversation]);
		if (path === '/api/conversations/voice-chat') return json(route, conversation);
		if (path === '/api/tasks' || path === '/api/tasks/recent-by-agent' || path.endsWith('/subtasks')) return json(route, { tasks: [], counts: {} });
		if (path === '/api/tasks/counts') return json(route, {});
		if (path === '/api/tasks/voice-task') return json(route, task);
		if (path === '/api/tasks/voice-task/activity') return json(route, { attempts: [], events: [], has_more_before: false, has_more_after: false });
		if (path.endsWith('/messages')) {
			if (request.method() === 'POST') { state.sent.push(request.postDataJSON()); return json(route, {}); }
			return json(route, [1, 2].map((id) => ({
				id, conversation_id: conversation.id, role: 'assistant', sender_type: 'agent', sender_id: agent.id, sender_name: agent.title,
				content: options.reply ?? `Reply **${id}** to read.`, message_type: 'message', metadata: {}, attachments: [], created_at: timestamp, timestamp
			})));
		}
		if (path.startsWith('/api/conversations/') && path.endsWith('/events')) return route.fulfill({ contentType: 'text/event-stream', body: ': connected\n\n' });
		return json(route, []);
	});
	return state;
}

declare global {
	interface Window {
		speechTest: { stopped: number; played: number; closed: number; released: number; deny: boolean; uploaded: string[]; resolvePermission?: () => void; endPlayback?: () => void };
	}
}

async function mockMedia(page: Page, mime = 'audio/webm;codecs=opus', deferredPermission = false) {
	await page.addInitScript(({ mime, deferredPermission }) => {
		window.speechTest = { stopped: 0, played: 0, closed: 0, released: 0, deny: false, uploaded: [] };
		const fetch = window.fetch.bind(window);
		window.fetch = async (input, init) => {
			if (String(input).endsWith('/api/speech/transcriptions') && init?.body instanceof FormData) {
				// WebKit's interception protocol omits file bytes from postData().
				// Inspect the real FormData without replacing the browser upload.
				window.speechTest.uploaded.push(await (init.body.get('file') as Blob).text());
			}
			return fetch(input, init);
		};
		Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: {
			async getUserMedia() {
				if (window.speechTest.deny) throw new DOMException('Denied', 'NotAllowedError');
				if (deferredPermission) await new Promise<void>((resolve) => { window.speechTest.resolvePermission = resolve; });
				return { getTracks: () => [{ stop: () => { window.speechTest.stopped += 1; } }] };
			}
		} });
		class FakeRecorder {
			state = 'inactive';
			mimeType = mime;
			ondataavailable?: (event: { data: Blob }) => void;
			onstop?: () => void;
			static isTypeSupported(type: string) { return type === mime; }
			start() { this.state = 'recording'; }
			stop() {
				this.state = 'inactive';
				queueMicrotask(() => {
					this.ondataavailable?.({ data: new Blob(['final-recorded-audio'], { type: mime }) });
					this.onstop?.();
				});
			}
		}
		Object.defineProperty(window, 'MediaRecorder', { configurable: true, value: FakeRecorder });
		let inClick = false;
		document.addEventListener('click', () => {
			inClick = true;
			setTimeout(() => { inClick = false; }, 0);
		}, true);
		class FakeAudioContext {
			destination = {};
			async resume() {
				if (!inClick) throw new DOMException('Playback must start during a click', 'NotAllowedError');
			}
			async close() { window.speechTest.closed += 1; }
			async decodeAudioData() { return {}; }
			createBufferSource() {
				const source = {
					onended: null as (() => void) | null,
					connect() {},
					disconnect() { window.speechTest.released += 1; },
					stop() {},
					start() {
						window.speechTest.played += 1;
						window.speechTest.endPlayback = () => source.onended?.();
					}
				};
				return source;
			}
		}
		Object.defineProperty(window, 'AudioContext', { configurable: true, value: FakeAudioContext });
	}, { mime, deferredPermission });
}

test('speech settings retain, replace, and explicitly clear the server key', async ({ page }) => {
	const state = await mockApi(page, { enabled: false });
	await page.goto('/settings/speech');
	await expect(page.getByRole('heading', { name: 'Speech', exact: true })).toBeVisible();
	await expect(page.getByLabel('API key', { exact: true })).toHaveValue('');
	await page.getByLabel('Enable speech').check();
	await page.getByLabel('Voice', { exact: true }).fill('another-voice');
	await page.getByRole('button', { name: 'Save speech settings' }).click();
	await expect(page.getByRole('status').filter({ hasText: 'Saved' })).toBeVisible();
	expect(state.saved[0]).toMatchObject({ enabled: true, voice: 'another-voice' });
	expect(state.saved[0]).not.toHaveProperty('api_key');
	await page.getByLabel('API key', { exact: true }).fill('new-private-key');
	await page.getByRole('button', { name: 'Save speech settings' }).click();
	await expect(page.getByLabel('API key', { exact: true })).toHaveValue('');
	expect(state.saved[1].api_key).toBe('new-private-key');
	expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain('new-private-key');
	await page.getByRole('button', { name: 'Remove saved key' }).click();
	await page.getByRole('button', { name: 'Save speech settings' }).click();
	await expect(page.getByText('No API key saved.')).toBeVisible();
	expect(state.saved[2].api_key).toBe('');
	await page.getByRole('link', { name: 'P Profile', exact: true }).click();
	await page.getByRole('link', { name: 'S Speech', exact: true }).click();
	await expect(page.getByLabel('Voice', { exact: true })).toHaveValue('another-voice');
});

for (const [path, mime, extension] of [
	['/conversations/voice-chat', 'audio/webm;codecs=opus', 'webm'],
	['/tasks/voice-task', 'audio/mp4', 'mp4'],
	['/', 'audio/webm;codecs=opus', 'webm'],
	['/agents/voice-agent', 'audio/mp4', 'mp4'],
] as const) {
	test(`dictation fills an editable draft without sending: ${path} (${extension})`, async ({ page }) => {
		const state = await mockApi(page);
		await mockMedia(page, mime);
		let uploads = 0;
		await page.route('**/api/speech/transcriptions', async (route) => {
			const request = route.request();
			expect(request.headers()['x-xpressclaw-csrf']).toBe('speech-csrf');
			expect(request.headers()['content-type']).toContain('multipart/form-data; boundary=');
			expect(request.postData()).toContain(`filename="recording.${extension}"`);
			uploads += 1;
			await json(route, { text: ' dictated words ' });
		});
		await page.goto(path);
		const draft = page.locator('textarea').first();
		await draft.fill('Typed words');
		await page.getByRole('button', { name: 'Dictate message' }).click();
		await expect(page.getByRole('status').filter({ hasText: 'Recording' })).toBeVisible();
		await page.getByRole('button', { name: 'Finish dictation' }).click();
		await expect(draft).toHaveValue('Typed words dictated words');
		expect(uploads).toBe(1);
		expect(await page.evaluate(() => window.speechTest.uploaded)).toEqual(['final-recorded-audio']);
		expect(state.sent).toEqual([]);
		expect(await page.evaluate(() => window.speechTest.stopped)).toBe(1);
	});
}

test('cancel stops recording and navigation releases a pending microphone permission', async ({ page }) => {
	await mockApi(page);
	await mockMedia(page, 'audio/webm;codecs=opus', true);
	let uploads = 0;
	await page.route('**/api/speech/transcriptions', async (route) => { uploads += 1; await json(route, { text: 'Must not be inserted' }); });
	await page.goto('/conversations/voice-chat');
	await page.getByRole('button', { name: 'Dictate message' }).click();
	await expect(page.getByRole('button', { name: 'Cancel dictation' })).toBeVisible();
	await page.getByRole('button', { name: 'Cancel dictation' }).click();
	await page.evaluate(() => window.speechTest.resolvePermission?.());
	await expect.poll(() => page.evaluate(() => window.speechTest.stopped)).toBe(1);
	await page.getByRole('button', { name: 'Dictate message' }).click();
	await page.evaluate(() => window.speechTest.resolvePermission?.());
	await expect(page.getByRole('button', { name: 'Finish dictation' })).toBeVisible();
	await page.getByRole('button', { name: 'Cancel dictation' }).click();
	await expect.poll(() => page.evaluate(() => window.speechTest.stopped)).toBe(2);
	await expect(page.locator('textarea').first()).toHaveValue('');
	await page.getByRole('button', { name: 'Dictate message' }).click();
	// Client-side navigation unmounts the composer without destroying the mock window.
	await page.getByRole('link', { name: 'Settings', exact: true }).click();
	await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible();
	await page.evaluate(() => window.speechTest.resolvePermission?.());
	await expect.poll(() => page.evaluate(() => window.speechTest.stopped)).toBe(3);
	expect(uploads).toBe(0);
});

test('permission and provider failures preserve the typed draft', async ({ page }) => {
	await mockApi(page);
	await mockMedia(page);
	await page.route('**/api/speech/transcriptions', (route) => json(route, { error: 'Speech provider returned HTTP 401.' }, 502));
	await page.goto('/conversations/voice-chat');
	await page.locator('textarea').first().fill('Keep my draft');
	await page.evaluate(() => { window.speechTest.deny = true; });
	await page.getByRole('button', { name: 'Dictate message' }).click();
	await expect(page.getByRole('alert')).toContainText('Microphone access was denied');
	await page.evaluate(() => { window.speechTest.deny = false; });
	await page.getByRole('button', { name: 'Dictate message' }).click();
	await page.getByRole('button', { name: 'Finish dictation' }).click();
	await expect(page.getByRole('alert')).toContainText('HTTP 401');
	await expect(page.locator('textarea').first()).toHaveValue('Keep my draft');
});

test('read aloud retries CSRF, reads long prose in chunks, and stops competing playback', async ({ page }) => {
	const prose = 'A sentence to read. '.repeat(350).trim();
	await mockApi(page, { reply: `**${prose}**\n\n\`\`\`ts\nsecretCodeBlock();\n\`\`\`` });
	await mockMedia(page);
	let rejected = false;
	const inputs: string[] = [];
	await page.route('**/api/speech/synthesize', async (route) => {
		expect(route.request().headers()['x-xpressclaw-csrf']).toBe('speech-csrf');
		if (!rejected) { rejected = true; return json(route, { error: 'CSRF token is invalid or expired' }, 403); }
		inputs.push(route.request().postDataJSON().input);
		await route.fulfill({ contentType: 'audio/mpeg', body: Buffer.from('test-audio') });
	});
	await page.goto('/conversations/voice-chat');
	const replies = page.locator('[data-message-role="assistant"]');
	await expect(replies).toHaveCount(2);
	expect(inputs).toEqual([]);
	await replies.first().getByRole('button', { name: 'Read aloud', exact: true }).click();
	await expect.poll(() => page.evaluate(() => window.speechTest.played)).toBe(1);
	await page.evaluate(() => window.speechTest.endPlayback?.());
	await expect.poll(() => page.evaluate(() => window.speechTest.played)).toBe(2);
	expect(inputs.join(' ')).toBe(prose);
	expect(inputs.every((input) => input.length <= 4000)).toBe(true);
	await replies.last().getByRole('button', { name: 'Read aloud', exact: true }).click();
	await expect.poll(() => page.evaluate(() => window.speechTest.played)).toBe(3);
	await expect(replies.first().getByRole('button', { name: 'Read aloud', exact: true })).toBeVisible();
	await replies.last().getByRole('button', { name: 'Stop reading aloud' }).click();
	await expect.poll(() => page.evaluate(() => window.speechTest.released)).toBe(3);
	expect(await page.evaluate(() => window.speechTest.closed)).toBe(2);
});

test('speech controls stay hidden while disabled', async ({ page }) => {
	await mockApi(page, { enabled: false });
	await page.goto('/conversations/voice-chat');
	await expect(page.locator('textarea').first()).toBeVisible();
	await expect(page.getByRole('button', { name: 'Dictate message' })).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Read aloud', exact: true })).toHaveCount(0);
});

test('cancelled transcription cannot append a late result to the draft', async ({ page }) => {
	await mockApi(page);
	await mockMedia(page);
	let release!: () => void;
	const pending = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/api/speech/transcriptions', async (route) => {
		await pending;
		await json(route, { text: 'Late transcript' }).catch(() => {});
	});
	await page.goto('/conversations/voice-chat');
	await page.locator('textarea').first().fill('Keep this');
	await page.getByRole('button', { name: 'Dictate message' }).click();
	const upload = page.waitForRequest('**/api/speech/transcriptions');
	await page.getByRole('button', { name: 'Finish dictation' }).click();
	await upload;
	await page.getByRole('button', { name: 'Cancel dictation' }).click();
	release();
	await expect(page.getByRole('button', { name: 'Dictate message' })).toBeEnabled();
	await expect(page.locator('textarea').first()).toHaveValue('Keep this');
});

test('read aloud can cancel generation and recover from a provider error', async ({ page }) => {
	await mockApi(page);
	await mockMedia(page);
	let release!: () => void;
	const pending = new Promise<void>((resolve) => { release = resolve; });
	let requests = 0;
	await page.route('**/api/speech/synthesize', async (route) => {
		requests += 1;
		if (requests === 1) await pending;
		await json(route, { error: 'Speech provider unavailable' }, 502).catch(() => {});
	});
	await page.goto('/conversations/voice-chat');
	const reply = page.locator('[data-message-role="assistant"]').first();
	const generation = page.waitForRequest('**/api/speech/synthesize');
	await reply.getByRole('button', { name: 'Read aloud', exact: true }).click();
	await generation;
	await reply.getByRole('button', { name: 'Stop reading aloud' }).click();
	release();
	await reply.getByRole('button', { name: 'Read aloud', exact: true }).click();
	await expect(reply.getByRole('alert')).toHaveText('Speech provider unavailable');
	expect(await page.evaluate(() => window.speechTest.played)).toBe(0);
	await expect(reply.getByRole('button', { name: 'Read aloud', exact: true })).toBeEnabled();
});

test('read aloud decodes and plays provider audio through the native browser audio context', async ({ page }) => {
	await mockApi(page);
	await page.addInitScript(() => {
		window.speechTest = { stopped: 0, played: 0, closed: 0, released: 0, deny: false, uploaded: [] };
		const NativeAudioContext = window.AudioContext;
		window.AudioContext = class extends NativeAudioContext {
			createBufferSource() {
				const source = super.createBufferSource();
				const start = source.start.bind(source);
				source.start = () => { window.speechTest.played += 1; start(); };
				return source;
			}
		};
	});
	// A short silent PCM clip exercises the real decoder and playback lifecycle.
	const wav = Buffer.alloc(1644);
	wav.write('RIFF', 0); wav.writeUInt32LE(wav.length - 8, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28); wav.writeUInt16LE(2, 32);
	wav.writeUInt16LE(16, 34); wav.write('data', 36); wav.writeUInt32LE(wav.length - 44, 40);
	await page.route('**/api/speech/synthesize', (route) => route.fulfill({ contentType: 'audio/wav', body: wav }));
	await page.goto('/conversations/voice-chat');
	const reply = page.locator('[data-message-role="assistant"]').first();
	await reply.getByRole('button', { name: 'Read aloud', exact: true }).click();
	await expect.poll(() => page.evaluate(() => window.speechTest.played)).toBe(1);
	await expect(reply.getByRole('button', { name: 'Read aloud', exact: true })).toBeVisible();
	await expect(reply.getByRole('alert')).toHaveCount(0);
});
