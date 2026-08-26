import { expect, test, type Page, type Route } from '@playwright/test';

const baseInstance = {
	instance_id: 'instance-browser-test',
	effective: {
		bind: '127.0.0.1',
		port: 8935,
		authentication_enabled: false,
		allow_unauthenticated_remote: false,
	},
	saved: {
		bind: '127.0.0.1',
		port: 8935,
		authentication_enabled: false,
		allow_unauthenticated_remote: false,
	},
	restart_required: false,
	credential_kind: 'disabled',
	password_configured: false,
	config_path: '/tmp/instance/xpressclaw.yaml',
	data_dir: '/tmp/instance',
	workspace_dir: '/tmp/instance/workspaces',
	transport_encryption: 'operator_managed',
};

function genericResponse(path: string): unknown {
	if (path === '/api/health') return { status: 'ok', version: '0.2.0', build: 'test', name: 'xpressclaw' };
	if (path === '/api/setup/check-docker') return { available: true, installed: true, can_start: false, runtime: 'docker' };
	if (path === '/api/setup/config') return {
		instance: { config_path: baseInstance.config_path, data_dir: baseInstance.data_dir, workspace_dir: baseInstance.workspace_dir },
		llm: { providers: [] }, agents: [], mcp_servers: [],
		system: { budget: { daily: '0', monthly: null, on_exceeded: 'warn' } },
	};
	if (path === '/api/tasks/counts') return {};
	return [];
}

async function fulfill(route: Route, body: unknown, status = 200) {
	await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
}

test('protected-route login returns through the persistent layout without exposing credentials', async ({ page }) => {
	let authenticated = false;
	let submittedCredential = '';
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, {
				instance_id: 'instance-browser-test',
				authentication_enabled: true,
				credential_kind: 'password',
				authenticated,
				csrf_token: authenticated ? 'csrf-browser-test' : null,
			});
			return;
		}
		if (path === '/api/auth/login') {
			submittedCredential = (request.postDataJSON() as { credential: string }).credential;
			authenticated = true;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				headers: { 'Set-Cookie': 'xpressclaw_session=opaque-session; Path=/; HttpOnly; SameSite=Strict' },
				body: JSON.stringify({ authenticated: true, csrf_token: 'csrf-browser-test' }),
			});
			return;
		}
		if (path === '/api/settings/instance/') {
			await fulfill(route, { ...baseInstance, effective: { ...baseInstance.effective, authentication_enabled: true }, saved: { ...baseInstance.saved, authentication_enabled: true }, credential_kind: 'password', password_configured: true });
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/settings/server');
	await expect(page).toHaveURL(/\/login\?return_to=%2Fsettings%2Fserver$/);
	await expect(page.getByRole('heading', { name: 'Sign in to this instance' })).toBeVisible();
	await page.getByLabel('Password').fill('not-persisted-password');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page).toHaveURL(/\/settings\/server$/);
	await expect(page.getByRole('heading', { name: 'Instance', exact: true })).toBeVisible();
	expect(submittedCredential).toBe('not-persisted-password');
	expect(page.url()).not.toContain('not-persisted-password');
	expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain('not-persisted-password');
});

test('Desktop auto-login returns only status after native session installation', async ({ page }) => {
	let bootstrapCalls = 0;
	let bearerExchangeCalls = 0;
	await page.addInitScript(() => {
		const target = window as unknown as {
			__desktopLoginResults: unknown[];
			__TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> };
		};
		target.__desktopLoginResults = [];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string) => {
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'matched', local: false };
				}
				if (command === 'login_active_profile') {
					const result = true;
					target.__desktopLoginResults.push(result);
					return result;
				}
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/auth/bootstrap') {
			bootstrapCalls += 1;
			await fulfill(route, {
				instance_id: 'desktop-native-session',
				authentication_enabled: true,
				credential_kind: 'password',
				authenticated: bootstrapCalls > 1,
				csrf_token: bootstrapCalls > 1 ? 'csrf-from-native-session' : null,
			});
			return;
		}
		if (path === '/api/auth/exchange') {
			bearerExchangeCalls += 1;
			await fulfill(route, { error: 'legacy bearer exchange must not be used' }, 404);
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/login?return_to=%2Fdashboard');
	await expect(page).toHaveURL(/\/dashboard$/);
	expect(bootstrapCalls).toBeGreaterThanOrEqual(2);
	expect(bearerExchangeCalls).toBe(0);
	expect(await page.evaluate(() => (
		window as unknown as { __desktopLoginResults: unknown[] }
	).__desktopLoginResults)).toEqual([true]);
});

test('Desktop blocks credentials when a remote address changes instance identity', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__selectedProfile: string | null;
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__selectedProfile = null;
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'changed', local: false };
				}
				if (command === 'select_instance_profile') {
					target.__selectedProfile = (args as { id: string }).id;
					return null;
				}
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/auth/bootstrap', (route) => fulfill(route, {
		instance_id: 'replacement-instance',
		authentication_enabled: true,
		credential_kind: 'password',
		authenticated: false,
		csrf_token: null,
	}));

	await page.goto('/login');
	await expect(page.getByText(/address now identifies a different XpressClaw instance/i)).toBeVisible();
	await expect(page.getByLabel('Password')).toHaveCount(0);
	await page.getByRole('button', { name: 'Return to local instance' }).click();
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __selectedProfile: string | null }
	).__selectedProfile)).toBe('local');
});

test('Desktop routes a no-auth replacement through identity recovery', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__selectedProfile: string | null;
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__selectedProfile = null;
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'changed', local: false };
				}
				if (command === 'select_instance_profile') {
					target.__selectedProfile = (args as { id: string }).id;
					return null;
				}
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/auth/bootstrap', (route) => fulfill(route, {
		instance_id: 'replacement-instance',
		authentication_enabled: false,
		credential_kind: 'disabled',
		authenticated: true,
		csrf_token: null,
	}));

	await page.goto('/dashboard');
	await expect(page).toHaveURL(/\/login\?return_to=%2Fdashboard$/);
	await expect(page.getByText(/address now identifies a different XpressClaw instance/i)).toBeVisible();
	await page.getByRole('button', { name: 'Return to local instance' }).click();
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __selectedProfile: string | null }
	).__selectedProfile)).toBe('local');
});

test('Desktop pins a first-use no-auth local instance before showing the workspace', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__tauriCalls: string[];
			__TAURI_INTERNALS__: { invoke: (command: string) => Promise<unknown> };
		};
		target.__tauriCalls = [];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string) => {
				target.__tauriCalls.push(command);
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'unpinned', local: true };
				}
				if (command === 'login_active_profile') return null;
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, {
				instance_id: 'first-local-instance',
				authentication_enabled: false,
				credential_kind: 'disabled',
				authenticated: true,
				csrf_token: null,
			});
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/dashboard');
	await expect(page).toHaveURL(/\/dashboard$/);
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __tauriCalls: string[] }
	).__tauriCalls)).toEqual(['get_active_instance_profile', 'login_active_profile']);
});

test('Desktop requires explicit recovery before trusting a replacement local identity', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__tauriCalls: string[];
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__tauriCalls = [];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				target.__tauriCalls.push(`${command}:${JSON.stringify(args ?? null)}`);
				if (command === 'get_active_instance_profile') {
					return { identity_status: 'changed', local: true };
				}
				if (command === 'trust_local_instance_replacement') return null;
				if (command === 'login_active_profile') return null;
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/auth/bootstrap', (route) => fulfill(route, {
		instance_id: 'replacement-local-instance',
		authentication_enabled: true,
		credential_kind: 'password',
		authenticated: false,
		csrf_token: null,
	}));

	await page.goto('/login');
	await expect(page.getByText(/different XpressClaw instance is answering on the saved local address/i)).toBeVisible();
	await expect(page.getByText(/keeps the previous instance identity in native storage/i)).toBeVisible();
	await expect(page.getByText('trusted-local-instance', { exact: true })).toHaveCount(0);
	await expect(page.getByLabel('Password')).toHaveCount(0);
	expect(await page.evaluate(() => (
		window as unknown as { __tauriCalls: string[] }
	).__tauriCalls.some((call) => call.startsWith('login_active_profile:')))).toBe(false);

	await page.getByRole('button', { name: 'Trust replacement local instance' }).click();
	await expect(page.getByLabel('Password')).toBeVisible();
	const calls = await page.evaluate(() => (
		window as unknown as { __tauriCalls: string[] }
	).__tauriCalls);
	expect(calls).toContain('trust_local_instance_replacement:{"instanceId":"replacement-local-instance"}');
	expect(calls.filter((call) => call.startsWith('login_active_profile:'))).toHaveLength(1);
	expect(calls.some((call) => call.includes('credential'))).toBe(false);
});

test('Instance settings requires the explicit no-auth remote warning and shows restart-pending values', async ({ page }) => {
	let saved = structuredClone(baseInstance.saved);
	let updatePayload: Record<string, unknown> | null = null;
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, { instance_id: baseInstance.instance_id, authentication_enabled: false, credential_kind: 'disabled', authenticated: true, csrf_token: null });
			return;
		}
		if (path === '/api/settings/instance/') {
			if (request.method() === 'PUT') {
				updatePayload = request.postDataJSON() as Record<string, unknown>;
				saved = {
					bind: String(updatePayload.bind),
					port: Number(updatePayload.port),
					authentication_enabled: Boolean(updatePayload.authentication_enabled),
					allow_unauthenticated_remote: Boolean(updatePayload.acknowledge_unauthenticated_remote),
				};
			}
			await fulfill(route, { ...baseInstance, saved, restart_required: saved.bind !== baseInstance.effective.bind || saved.port !== baseInstance.effective.port });
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/settings/server');
	await page.getByLabel('Bind address or interface').fill('0.0.0.0');
	const save = page.getByRole('button', { name: 'Save instance settings' });
	await expect(save).toBeDisabled();
	await page.getByLabel(/I understand that anyone who can reach this port/).check();
	await expect(save).toBeEnabled();
	await save.click();

	await expect(page.getByText('Restart pending')).toBeVisible();
	await expect(page.getByText(/Restart this XpressClaw instance/)).toBeVisible();
	expect(updatePayload).toMatchObject({
		bind: '0.0.0.0',
		authentication_enabled: false,
		acknowledge_unauthenticated_remote: true,
	});
	await expect(page.getByText('127.0.0.1:8935', { exact: true })).toBeVisible();
});

test('Desktop persists a newly configured password before authentication restarts', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__credentialCalls: unknown[];
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__credentialCalls = [];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				if (command === 'list_instance_profiles') return [];
				if (command === 'store_active_profile_credential') {
					target.__credentialCalls.push(structuredClone(args));
					return null;
				}
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	let saved = structuredClone(baseInstance.saved);
	let passwordConfigured = false;
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, { instance_id: baseInstance.instance_id, authentication_enabled: false, credential_kind: 'disabled', authenticated: true, csrf_token: null });
			return;
		}
		if (path === '/api/settings/instance/') {
			if (request.method() === 'PUT') {
				const update = request.postDataJSON() as { authentication_enabled: boolean; password?: string };
				saved = { ...saved, authentication_enabled: update.authentication_enabled };
				passwordConfigured = Boolean(update.password);
			}
			await fulfill(route, {
				...baseInstance,
				saved,
				password_configured: passwordConfigured,
				restart_required: saved.authentication_enabled !== baseInstance.effective.authentication_enabled,
			});
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/settings/server');
	await page.getByLabel('Require XpressClaw login').check();
	await page.getByPlaceholder('Optional password (12+ characters)').fill('pending-password-123');
	await page.getByRole('button', { name: 'Save instance settings' }).click();

	await expect(page.getByText('Restart pending')).toBeVisible();
	await expect(page.getByText(/Desktop could not update/)).toHaveCount(0);
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __credentialCalls: unknown[] }
	).__credentialCalls)).toEqual([{ credential: 'pending-password-123' }]);
});

test('Desktop retains its keychain password while authentication is temporarily disabled', async ({ page }) => {
	await page.addInitScript(() => {
		const target = window as unknown as {
			__credentialCalls: unknown[];
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__credentialCalls = [];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				if (command === 'get_active_instance_profile') return { identity_status: 'matched', local: true };
				if (command === 'list_instance_profiles') return [];
				if (command === 'store_active_profile_credential') {
					target.__credentialCalls.push(structuredClone(args));
					return null;
				}
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	const effective = { ...baseInstance.effective, authentication_enabled: true };
	let saved = { ...baseInstance.saved, authentication_enabled: true };
	let updateCompleted = false;
	await page.route('**/api/**', async (route) => {
		const request = route.request();
		const path = new URL(request.url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, { instance_id: baseInstance.instance_id, authentication_enabled: true, credential_kind: 'password', authenticated: true, csrf_token: 'csrf-test' });
			return;
		}
		if (path === '/api/settings/instance/') {
			if (request.method() === 'PUT') {
				const update = request.postDataJSON() as { authentication_enabled: boolean };
				saved = { ...saved, authentication_enabled: update.authentication_enabled };
				updateCompleted = true;
			}
			await fulfill(route, {
				...baseInstance,
				effective,
				saved,
				credential_kind: 'password',
				password_configured: true,
				restart_required: saved.authentication_enabled !== effective.authentication_enabled,
			});
			return;
		}
		await fulfill(route, genericResponse(path));
	});

	await page.goto('/settings/server');
	await page.getByLabel('Require XpressClaw login').uncheck();
	await page.getByRole('button', { name: 'Save instance settings' }).click();
	await expect.poll(() => updateCompleted).toBe(true);
	await expect.poll(() => page.evaluate(() => (
		window as unknown as { __credentialCalls: unknown[] }
	).__credentialCalls)).toEqual([]);
});

test('Desktop profiles can be edited without exposing their saved keychain credential', async ({ page }) => {
	await page.addInitScript(() => {
		type Profile = {
			id: string; name: string; url: string; instance_id: string | null;
			authentication: string; local: boolean; active: boolean; health: string;
			confirmed_unauthenticated_remote: boolean;
		};
		const target = window as unknown as {
			__desktopProfileCalls: { command: string; args: unknown }[];
			__desktopProfiles: Profile[];
			__TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> };
		};
		target.__desktopProfileCalls = [];
		target.__desktopProfiles = [
			{ id: 'local', name: 'Local XpressClaw', url: 'http://localhost:8935', instance_id: 'local-id', authentication: 'none', local: true, active: true, health: 'healthy', confirmed_unauthenticated_remote: true },
			{ id: 'remote-id', name: 'Tailnet server', url: 'https://server.tailnet.example', instance_id: 'remote-instance', authentication: 'password', local: false, active: false, health: 'reachable', confirmed_unauthenticated_remote: false },
		];
		target.__TAURI_INTERNALS__ = {
			invoke: async (command: string, args: unknown) => {
				target.__desktopProfileCalls.push({ command, args });
				if (command === 'list_instance_profiles') return structuredClone(target.__desktopProfiles);
				if (command === 'save_instance_profile') {
					const input = (args as { input: { id: string; name: string; url: string; authentication: string; confirm_unauthenticated_remote: boolean } }).input;
					const profile = target.__desktopProfiles.find((value) => value.id === input.id)!;
					Object.assign(profile, input);
					return structuredClone(profile);
				}
				if (command === 'select_instance_profile' || command === 'delete_instance_profile') return null;
				throw new Error(`Unexpected Tauri command: ${command}`);
			},
		};
	});
	await page.route('**/api/**', async (route) => {
		const path = new URL(route.request().url()).pathname;
		if (path === '/api/auth/bootstrap') {
			await fulfill(route, { instance_id: baseInstance.instance_id, authentication_enabled: false, credential_kind: 'disabled', authenticated: true, csrf_token: null });
		} else if (path === '/api/settings/instance/') {
			await fulfill(route, baseInstance);
		} else {
			await fulfill(route, genericResponse(path));
		}
	});

	await page.goto('/settings/server');
	await expect(page.getByText('Tailnet server', { exact: true })).toBeVisible();
	await expect(page.getByText('Reachable · password')).toBeVisible();
	await page.getByRole('button', { name: 'Edit Tailnet server' }).click();
	await expect(page.getByText('Edit remote profile')).toBeVisible();
	await expect(page.getByPlaceholder('Blank keeps it only when address and mode are unchanged')).toHaveValue('');
	await page.getByPlaceholder('Profile name').fill('Primary tailnet');
	await page.getByRole('button', { name: 'Update profile' }).click();
	await expect(page.getByText('Primary tailnet', { exact: true })).toBeVisible();

	const saveCall = await page.evaluate(() => (
		window as unknown as { __desktopProfileCalls: { command: string; args: { input?: Record<string, unknown> } }[] }
	).__desktopProfileCalls.find((call) => call.command === 'save_instance_profile'));
	expect(saveCall?.args.input).toMatchObject({
		id: 'remote-id',
		name: 'Primary tailnet',
		credential: null,
	});
	expect(JSON.stringify(saveCall)).not.toContain('saved-password');
});
