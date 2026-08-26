<script lang="ts">
	import { onMount } from 'svelte';
	import { AlertTriangle, Check, KeyRound, Laptop, LoaderCircle, Pencil, Plus, Server, ShieldCheck, Trash2, X } from '@lucide/svelte';
	import { auth, health, instanceSettings, setup, type DockerStatus, type InstanceSettings, type LiveConfig } from '$lib/api';

	type DesktopProfile = {
		id: string;
		name: string;
		url: string;
		instance_id: string | null;
		authentication: 'password' | 'startup_token' | 'none';
		local: boolean;
		active: boolean;
		health: 'healthy' | 'unreachable' | 'identity_changed' | 'authentication_required' | 'unknown';
		confirmed_unauthenticated_remote: boolean;
	};

	let serverInfo = $state<{ status: string; version: string; build: string } | null>(null);
	let config = $state<LiveConfig | null>(null);
	let runtime = $state<DockerStatus | null>(null);
	let settings = $state<InstanceSettings | null>(null);
	let clientAddress = $state('');
	let bind = $state('127.0.0.1');
	let port = $state(8935);
	let authenticationEnabled = $state(false);
	let acknowledgeRemote = $state(false);
	let password = $state('');
	let removePassword = $state(false);
	let saving = $state(false);
	let saveError = $state('');
	let saveNotice = $state('');
	let desktop = $state(false);
	let profiles = $state<DesktopProfile[]>([]);
	let profileError = $state('');
	let profileName = $state('');
	let profileUrl = $state('');
	let profileCredential = $state('');
	let profileAuthentication = $state<'password' | 'startup_token' | 'none'>('password');
	let profileTrustNoAuth = $state(false);
	let profileSaving = $state(false);
	let editingProfileId = $state<string | null>(null);
	const tunnelCommand = 'ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host';
	const listenerLabel = (address: string, listenerPort: number) => address.includes(':')
		? `[${address}]:${listenerPort}`
		: `${address}:${listenerPort}`;
	const isLoopbackBind = (value: string) => {
		const normalized = value.trim().toLowerCase();
		return normalized === '::1' || /^127(?:\.\d{1,3}){3}$/.test(normalized);
	};
	const profileHealthLabel = (value: DesktopProfile['health']) => ({
		healthy: 'Healthy',
		unreachable: 'Unreachable',
		identity_changed: 'Identity changed',
		authentication_required: 'Credentials needed',
		unknown: 'Checking',
	})[value];

	function applySettings(value: InstanceSettings) {
		settings = value;
		bind = value.saved.bind;
		port = value.saved.port;
		authenticationEnabled = value.saved.authentication_enabled;
		acknowledgeRemote = value.saved.allow_unauthenticated_remote;
		password = '';
		removePassword = false;
	}

	async function loadProfiles() {
		if (!desktop) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			profiles = await invoke<DesktopProfile[]>('list_instance_profiles');
		} catch (cause) {
			profileError = cause instanceof Error ? cause.message : String(cause);
		}
	}

	onMount(async () => {
		clientAddress = window.location.origin;
		desktop = '__TAURI_INTERNALS__' in window;
		const [nextServer, nextConfig, nextRuntime, nextSettings] = await Promise.all([
			health.check().catch(() => null),
			setup.getConfig().catch(() => null),
			setup.checkDocker().catch(() => null),
			instanceSettings.get().catch(() => null),
		]);
		serverInfo = nextServer;
		config = nextConfig;
		runtime = nextRuntime;
		if (nextSettings) applySettings(nextSettings);
		await loadProfiles();
	});

	async function saveInstance(event: SubmitEvent) {
		event.preventDefault();
		if (saving) return;
		saving = true;
		saveError = '';
		saveNotice = '';
		const submittedPassword = password;
		const submittedRemoval = removePassword;
		const submittedAuthModeChanged = settings?.saved.authentication_enabled !== authenticationEnabled;
		try {
			const value = await instanceSettings.update({
				bind,
				port,
				authentication_enabled: authenticationEnabled,
				acknowledge_unauthenticated_remote: acknowledgeRemote,
				...(password ? { password } : {}),
				...(removePassword ? { remove_password: true } : {}),
			});
			if (submittedPassword && value.effective.authentication_enabled) {
				// The password update revoked the old session. Establish a fresh
				// one before attempting optional Desktop keychain persistence.
				await auth.login(submittedPassword);
			}
			let keychainWarning = '';
			if (desktop && (submittedPassword || submittedRemoval || !authenticationEnabled)) {
				const { invoke } = await import('@tauri-apps/api/core');
				await invoke('store_active_profile_credential', {
					credential: submittedRemoval || !authenticationEnabled ? null : submittedPassword,
				}).catch((cause) => {
					keychainWarning = cause instanceof Error ? cause.message : String(cause);
				});
			}
			if (
				!submittedPassword &&
				value.effective.authentication_enabled &&
				(submittedRemoval || submittedAuthModeChanged)
			) {
				window.location.assign('/login?return_to=%2Fsettings%2Fserver');
				return;
			}
			applySettings(value);
			saveNotice = value.restart_required
				? 'Saved. Restart this XpressClaw instance to apply the pending listener or authentication change.'
				: 'Instance security settings saved.';
			if (keychainWarning) {
				saveError = `Instance settings were saved, but Desktop could not update the operating-system keychain: ${keychainWarning}`;
			}
		} catch (cause) {
			saveError = cause instanceof Error ? cause.message : 'Could not save instance settings.';
		} finally {
			password = '';
			saving = false;
		}
	}

	async function saveProfile(event: SubmitEvent) {
		event.preventDefault();
		if (profileSaving) return;
		profileSaving = true;
		profileError = '';
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('save_instance_profile', {
				input: {
					id: editingProfileId,
					name: profileName,
					url: profileUrl,
					authentication: profileAuthentication,
					credential: profileCredential || null,
					confirm_unauthenticated_remote: profileTrustNoAuth,
				},
			});
			resetProfileForm();
			await loadProfiles();
		} catch (cause) {
			profileError = cause instanceof Error ? cause.message : String(cause);
		} finally {
			profileCredential = '';
			profileSaving = false;
		}
	}

	function editProfile(profile: DesktopProfile) {
		editingProfileId = profile.id;
		profileName = profile.name;
		profileUrl = profile.url;
		profileAuthentication = profile.authentication;
		profileCredential = '';
		profileTrustNoAuth = profile.confirmed_unauthenticated_remote;
		profileError = '';
	}

	function resetProfileForm() {
		editingProfileId = null;
		profileName = '';
		profileUrl = '';
		profileCredential = '';
		profileAuthentication = 'password';
		profileTrustNoAuth = false;
	}

	async function selectProfile(id: string) {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('select_instance_profile', { id });
		} catch (cause) {
			profileError = cause instanceof Error ? cause.message : String(cause);
		}
	}

	async function deleteProfile(id: string) {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('delete_instance_profile', { id });
			if (editingProfileId === id) resetProfileForm();
			await loadProfiles();
		} catch (cause) {
			profileError = cause instanceof Error ? cause.message : String(cause);
		}
	}
</script>

<div class="mx-auto max-w-5xl space-y-6 p-4 sm:p-6">
	<div>
		<h1 class="text-2xl font-bold">Instance</h1>
		<p class="mt-1 text-sm text-muted-foreground">Connection, security, and Desktop profiles for this control plane</p>
	</div>

	<div class="grid gap-4 lg:grid-cols-2">
		<section class="rounded-xl border border-border bg-card p-4 shadow-sm">
			<div class="mb-3 flex items-center gap-2"><Server size={17} /><h2 class="text-sm font-semibold">Running instance</h2></div>
			<dl class="space-y-2.5 text-sm">
				<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Health</dt><dd class="flex items-center gap-1.5"><span class="size-2 rounded-full {serverInfo?.status === 'ok' ? 'bg-emerald-500' : 'bg-red-500'}"></span>{serverInfo?.status ?? 'Unknown'}</dd></div>
				<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Version</dt><dd>{serverInfo?.version ?? '—'}</dd></div>
				<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Client address</dt><dd class="break-all text-right font-mono text-xs">{clientAddress || '—'}</dd></div>
				{#if settings}
					<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Effective listener</dt><dd class="font-mono text-xs">{listenerLabel(settings.effective.bind, settings.effective.port)}</dd></div>
					<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Effective authentication</dt><dd>{settings.effective.authentication_enabled ? settings.credential_kind.replace('_', ' ') : 'Off'}</dd></div>
					<div class="flex items-start justify-between gap-4"><dt class="text-muted-foreground">Instance ID</dt><dd class="max-w-[60%] break-all text-right font-mono text-[11px]">{settings.instance_id}</dd></div>
				{/if}
				<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Container runtime</dt><dd class="capitalize">{runtime?.runtime ?? (runtime?.available ? 'Available' : 'Unavailable')}</dd></div>
			</dl>
		</section>

		<section class="rounded-xl border border-border bg-card p-4 shadow-sm">
			<div class="mb-3 flex items-center gap-2"><ShieldCheck size={17} /><h2 class="text-sm font-semibold">Connection model</h2></div>
			<p class="text-sm leading-relaxed text-muted-foreground">The browser is only a client. Agents continue on the control-plane machine when this page closes or reconnects.</p>
			<div class="mt-3 rounded-lg border border-amber-500/25 bg-amber-500/8 p-3 text-xs leading-relaxed text-amber-800 dark:text-amber-200">
				XpressClaw authentication does not encrypt traffic. Direct HTTP is appropriate only on an operator-trusted LAN or tailnet. Use an HTTPS reverse proxy for untrusted networks.
			</div>
			<div class="mt-3 rounded-md bg-muted px-3 py-2 font-mono text-[11px] text-foreground break-all">{tunnelCommand}</div>
		</section>
	</div>

	{#if settings}
		<form class="rounded-xl border border-border bg-card p-4 shadow-sm" onsubmit={saveInstance}>
			<div class="flex items-start justify-between gap-4">
				<div><h2 class="text-sm font-semibold">Saved listener and authentication</h2><p class="mt-1 text-xs text-muted-foreground">Listener and mode changes apply after restart. Password changes revoke existing sessions immediately.</p></div>
				{#if settings.restart_required}<span class="shrink-0 rounded-full bg-amber-500/15 px-2.5 py-1 text-[11px] font-medium text-amber-700 dark:text-amber-300">Restart pending</span>{/if}
			</div>

			<div class="mt-5 grid gap-4 sm:grid-cols-[1fr_140px]">
				<label class="space-y-1.5 text-sm"><span class="font-medium">Bind address or interface</span><input bind:value={bind} required class="w-full rounded-lg border border-input bg-background px-3 py-2 font-mono text-sm" aria-describedby="bind-help" /><span id="bind-help" class="block text-xs text-muted-foreground">Use 127.0.0.1 for local-only, 0.0.0.0 for all IPv4 interfaces, or :: for all IPv6 interfaces.</span></label>
				<label class="space-y-1.5 text-sm"><span class="font-medium">Port</span><input bind:value={port} type="number" min="1" max="65535" required class="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm" /></label>
			</div>

			<label class="mt-5 flex cursor-pointer items-start gap-3 rounded-lg border border-border p-3">
				<input type="checkbox" bind:checked={authenticationEnabled} class="mt-1" />
				<span><span class="block text-sm font-medium">Require XpressClaw login</span><span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">Use a password, or a strong token printed once each time the server starts when no password is set.</span></span>
			</label>

			{#if authenticationEnabled || settings.password_configured}
				<div class="mt-4 rounded-lg bg-muted/60 p-3">
					<div class="flex items-center gap-2 text-sm font-medium"><KeyRound size={15} />Password</div>
					<p class="mt-1 text-xs text-muted-foreground">{settings.password_configured ? authenticationEnabled ? 'A password is configured. Leave blank to keep it.' : 'A password is retained for the next time authentication is enabled.' : 'No password is configured; restart will generate a new startup token.'}</p>
					{#if authenticationEnabled}<input bind:value={password} type="password" autocomplete="off" minlength="12" maxlength="1024" placeholder={settings.password_configured ? 'Set a new password' : 'Optional password (12+ characters)'} class="mt-3 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm" disabled={removePassword} />{/if}
					{#if settings.password_configured}<label class="mt-3 flex items-center gap-2 text-xs"><input type="checkbox" bind:checked={removePassword} /> Remove the saved password and require a fresh token after restart</label>{/if}
				</div>
			{/if}

			{#if !authenticationEnabled && !isLoopbackBind(bind)}
				<label class="mt-4 flex cursor-pointer items-start gap-3 rounded-lg border border-amber-500/30 bg-amber-500/8 p-3">
					<input type="checkbox" bind:checked={acknowledgeRemote} class="mt-1" />
					<span><span class="flex items-center gap-1.5 text-sm font-medium text-amber-800 dark:text-amber-200"><AlertTriangle size={15} />Allow unprotected non-loopback access</span><span class="mt-1 block text-xs leading-relaxed text-muted-foreground">I understand that anyone who can reach this port can control Agents and read Project data, and that a trusted LAN or tailnet—not XpressClaw—provides the access boundary.</span></span>
				</label>
			{/if}

			{#if saveError}<p class="mt-4 text-sm text-destructive" role="alert">{saveError}</p>{/if}
			{#if saveNotice}<p class="mt-4 flex items-center gap-2 text-sm text-emerald-600 dark:text-emerald-400" role="status"><Check size={15} />{saveNotice}</p>{/if}
			<div class="mt-5 flex justify-end"><button type="submit" disabled={saving || (!authenticationEnabled && !isLoopbackBind(bind) && !acknowledgeRemote)} class="inline-flex items-center gap-2 rounded-lg bg-primary px-4 py-2 text-sm font-semibold text-primary-foreground disabled:opacity-50">{#if saving}<LoaderCircle size={15} class="animate-spin" />{/if}{saving ? 'Saving…' : 'Save instance settings'}</button></div>
		</form>
	{/if}

	{#if desktop}
		<section class="rounded-xl border border-border bg-card p-4 shadow-sm">
			<div class="flex items-center gap-2"><Laptop size={17} /><h2 class="text-sm font-semibold">Desktop instance profiles</h2></div>
			<p class="mt-1 text-xs text-muted-foreground">This Desktop window uses one selected profile at a time. The automatic local sidecar continues running when you connect remotely. Credentials stay in the operating-system keychain.</p>
			<div class="mt-4 grid gap-2">
				{#each profiles as profile}
					<div class="flex items-center gap-3 rounded-lg border {profile.active ? 'border-primary/50 bg-primary/5' : 'border-border'} p-3">
						<span aria-hidden="true" class="size-2.5 rounded-full {profile.health === 'healthy' ? 'bg-emerald-500' : profile.health === 'unreachable' ? 'bg-red-500' : 'bg-amber-500'}"></span>
						<div class="min-w-0 flex-1"><p class="truncate text-sm font-medium">{profile.name}{profile.local ? ' · Local' : ''}</p><p class="truncate text-xs text-muted-foreground">{profile.url}</p><p class="mt-0.5 text-[11px] text-muted-foreground">{profileHealthLabel(profile.health)} · {profile.authentication.replace('_', ' ')}</p></div>
						{#if !profile.active}<button type="button" onclick={() => selectProfile(profile.id)} class="rounded-md border border-border px-2.5 py-1.5 text-xs">Connect</button>{:else}<span class="text-xs font-medium text-primary">Connected</span>{/if}
						{#if !profile.local}<button type="button" aria-label="Edit {profile.name}" onclick={() => editProfile(profile)} class="rounded-md p-1.5 text-muted-foreground hover:text-foreground"><Pencil size={15} /></button><button type="button" aria-label="Delete {profile.name}" onclick={() => deleteProfile(profile.id)} class="rounded-md p-1.5 text-muted-foreground hover:text-destructive"><Trash2 size={15} /></button>{/if}
					</div>
				{/each}
			</div>

			<form class="mt-5 border-t border-border pt-4" onsubmit={saveProfile}>
				<div class="mb-3 flex items-center justify-between gap-3"><div class="flex items-center gap-2 text-sm font-medium">{#if editingProfileId}<Pencil size={15} />Edit remote profile{:else}<Plus size={15} />Add remote profile{/if}</div>{#if editingProfileId}<button type="button" onclick={resetProfileForm} class="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"><X size={14} />Cancel edit</button>{/if}</div>
				<div class="grid gap-3 sm:grid-cols-2"><input bind:value={profileName} required maxlength="80" placeholder="Profile name" class="rounded-lg border border-input bg-background px-3 py-2 text-sm" /><input bind:value={profileUrl} required maxlength="2048" type="url" placeholder="http://machine.tailnet:8935" class="rounded-lg border border-input bg-background px-3 py-2 text-sm" /></div>
				<div class="mt-3 grid gap-3 sm:grid-cols-2"><select bind:value={profileAuthentication} class="rounded-lg border border-input bg-background px-3 py-2 text-sm"><option value="password">Password</option><option value="startup_token">Startup token</option><option value="none">No authentication</option></select>{#if profileAuthentication !== 'none'}<input bind:value={profileCredential} required={!editingProfileId} maxlength="4096" type="password" autocomplete="off" placeholder={editingProfileId ? 'Blank keeps it only when address and mode are unchanged' : profileAuthentication === 'password' ? 'Instance password' : 'Current startup token'} class="rounded-lg border border-input bg-background px-3 py-2 text-sm" />{/if}</div>
				{#if profileAuthentication === 'none'}<label class="mt-3 flex items-start gap-2 text-xs text-muted-foreground"><input type="checkbox" bind:checked={profileTrustNoAuth} class="mt-0.5" /> I confirm this remote instance is reachable only through an operator-trusted LAN or tailnet.</label>{/if}
				{#if profileError}<p class="mt-3 text-sm text-destructive" role="alert">{profileError}</p>{/if}
				<div class="mt-3 flex justify-end"><button type="submit" disabled={profileSaving || (profileAuthentication === 'none' && !profileTrustNoAuth)} class="rounded-lg border border-border px-3 py-2 text-sm font-medium disabled:opacity-50">{profileSaving ? 'Saving…' : editingProfileId ? 'Update profile' : 'Save profile'}</button></div>
			</form>
		</section>
	{/if}

	{#if config}
		<section class="rounded-xl border border-border bg-card p-4 shadow-sm">
			<h2 class="text-sm font-semibold">Local paths and defaults</h2>
			<dl class="mt-3 space-y-2 text-sm">
				<div class="flex items-start justify-between gap-4"><dt class="text-muted-foreground">Configuration</dt><dd class="break-all text-right font-mono text-xs">{config.instance.config_path}</dd></div>
				<div class="flex items-start justify-between gap-4"><dt class="text-muted-foreground">Local data</dt><dd class="break-all text-right font-mono text-xs">{config.instance.data_dir}</dd></div>
				<div class="flex items-start justify-between gap-4"><dt class="text-muted-foreground">Managed workspaces</dt><dd class="break-all text-right font-mono text-xs">{config.instance.workspace_dir}</dd></div>
				<div class="flex justify-between"><dt class="text-muted-foreground">Daily budget</dt><dd>{config.system.budget.daily ?? 'none'}</dd></div>
			</dl>
		</section>
	{/if}
</div>
