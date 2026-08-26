<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { LockKeyhole, Radio, Server } from '@lucide/svelte';
	import { ApiError, auth, type AuthBootstrap } from '$lib/api';

	let session = $state<AuthBootstrap | null>(null);
	let credential = $state('');
	let error = $state('');
	let submitting = $state(false);
	let desktopAttempted = $state(false);
	let desktopIdentityBlocked = $state(false);
	let blockedLocalProfile = $state(false);
	let pinnedInstanceId = $state<string | null>(null);
	let returningLocal = $state(false);
	let trustingReplacement = $state(false);

	function safeReturnTo(): string {
		const candidate = $page.url.searchParams.get('return_to') ?? '/dashboard';
		try {
			const target = new URL(candidate, window.location.origin);
			if (target.origin !== window.location.origin) return '/dashboard';
			return `${target.pathname}${target.search}${target.hash}`;
		} catch {
			return '/dashboard';
		}
	}

	async function finishDesktopLogin(): Promise<boolean> {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const result = await invoke<{ ticket: string; instance_id: string } | null>('login_active_profile');
			if (!result) return false;
			if (session && result.instance_id !== session.instance_id) {
				throw new Error('The remote server identity changed. Review the saved profile before reconnecting.');
			}
			await auth.exchangeDesktopTicket(result.ticket);
			await goto(safeReturnTo(), { replaceState: true });
			return true;
		} catch (cause) {
			// A normal browser has no Tauri IPC. Only surface genuine Desktop
			// profile errors after the command was available.
			if (cause instanceof Error && !cause.message.includes('login_active_profile')) {
				error = cause.message;
			}
			return false;
		}
	}

	onMount(async () => {
		try {
			session = await auth.bootstrap();
			if ('__TAURI_INTERNALS__' in window) {
				desktopAttempted = true;
				const { invoke } = await import('@tauri-apps/api/core');
				const active = await invoke<{ instance_id: string | null; local: boolean }>('get_active_instance_profile');
				if (active.instance_id && active.instance_id !== session.instance_id) {
					desktopIdentityBlocked = true;
					blockedLocalProfile = active.local;
					pinnedInstanceId = active.instance_id;
					error = active.local
						? 'A different XpressClaw instance is answering on the saved local address. Desktop will not send it a saved credential.'
						: 'This address now identifies a different XpressClaw instance. Return to the local instance, then review or replace the remote profile only if you trust it.';
					return;
				}
				if (session.authentication_enabled && !session.authenticated && await finishDesktopLogin()) return;
			}
			if (!session.authentication_enabled || session.authenticated) {
				await goto(safeReturnTo(), { replaceState: true });
			}
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not reach this XpressClaw instance.';
		}
	});

	async function returnToLocalInstance() {
		if (returningLocal) return;
		returningLocal = true;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('select_instance_profile', { id: 'local' });
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not return to the local instance.';
			returningLocal = false;
		}
	}

	async function trustLocalReplacement() {
		if (!session || trustingReplacement) return;
		trustingReplacement = true;
		error = '';
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('trust_local_instance_replacement', { instanceId: session.instance_id });
			desktopIdentityBlocked = false;
			blockedLocalProfile = false;
			pinnedInstanceId = null;
			if (session.authentication_enabled && !session.authenticated) {
				await finishDesktopLogin();
			} else {
				await goto(safeReturnTo(), { replaceState: true });
			}
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not trust the replacement local instance.';
		} finally {
			trustingReplacement = false;
		}
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!credential || submitting) return;
		submitting = true;
		error = '';
		try {
			const submittedCredential = credential;
			await auth.login(submittedCredential);
			if ('__TAURI_INTERNALS__' in window) {
				const { invoke } = await import('@tauri-apps/api/core');
				// A successful manual Desktop login repairs a missing or rotated
				// keychain entry. Keychain failure must not invalidate the browser
				// session that was just established.
				await invoke('store_active_profile_credential', {
					credential: submittedCredential,
				}).catch(() => undefined);
			}
			credential = '';
			await goto(safeReturnTo(), { replaceState: true });
		} catch (cause) {
			credential = '';
			if (cause instanceof ApiError && cause.status === 429) {
				error = 'Too many attempts. Wait a minute, then try again.';
			} else {
				error = cause instanceof Error ? cause.message : 'Login failed.';
			}
		} finally {
			submitting = false;
		}
	}
</script>

<svelte:head><title>Sign in · XpressClaw</title></svelte:head>

<main class="min-h-screen bg-background text-foreground grid place-items-center p-5">
	<div class="w-full max-w-md">
		<div class="mb-7 flex items-center justify-center gap-3" aria-label="XpressClaw">
			<div class="grid size-10 place-items-center rounded-xl bg-primary text-primary-foreground shadow-lg shadow-primary/20">
				<Radio size={20} />
			</div>
			<div>
				<p class="text-lg font-semibold tracking-tight">XpressClaw</p>
				<p class="text-xs text-muted-foreground">Remote control plane</p>
			</div>
		</div>

		<section class="rounded-2xl border border-border bg-card p-6 shadow-xl shadow-black/5">
			<div class="mb-5 flex items-start gap-3">
				<div class="grid size-9 shrink-0 place-items-center rounded-lg bg-muted"><LockKeyhole size={18} /></div>
				<div>
					<h1 class="text-xl font-semibold">{desktopIdentityBlocked ? 'Verify this instance' : 'Sign in to this instance'}</h1>
					<p class="mt-1 text-sm leading-relaxed text-muted-foreground">
						{desktopIdentityBlocked
							? 'Desktop detected that the instance identity changed before using a saved credential.'
							: session?.credential_kind === 'startup_token'
							? 'Enter the token printed when this instance started.'
							: 'Enter the instance password set by its operator.'}
					</p>
				</div>
			</div>

			{#if desktopIdentityBlocked}
				<div class="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive" role="alert">
					<p>{error}</p>
					{#if blockedLocalProfile}
						<div class="mt-3 space-y-1 text-xs text-muted-foreground">
							<p>Saved identity: <code class="break-all">{pinnedInstanceId}</code></p>
							<p>Current identity: <code class="break-all">{session?.instance_id}</code></p>
						</div>
						<p class="mt-3 text-xs text-foreground">Only trust the replacement if you intentionally reset or replaced this local instance. Desktop will discard the previous credential; if it launched this replacement, it can retain only a startup token the new instance verifies.</p>
						<button type="button" onclick={trustLocalReplacement} disabled={trustingReplacement} class="mt-3 rounded-md bg-destructive px-3 py-1.5 text-xs font-medium text-destructive-foreground disabled:opacity-50">
							{trustingReplacement ? 'Waiting for confirmation…' : 'Trust replacement local instance'}
						</button>
					{:else}
						<button type="button" onclick={returnToLocalInstance} disabled={returningLocal} class="mt-3 rounded-md border border-destructive/40 px-3 py-1.5 text-xs font-medium disabled:opacity-50">
							{returningLocal ? 'Returning…' : 'Return to local instance'}
						</button>
					{/if}
				</div>
			{:else if session?.credential_kind === 'restart_required'}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-sm text-amber-700 dark:text-amber-300">
					Authentication changed. Restart the control plane to create a new login token.
				</div>
			{:else}
				<form class="space-y-4" onsubmit={submit}>
					<label class="block text-sm font-medium" for="credential">
						{session?.credential_kind === 'startup_token' ? 'Startup token' : 'Password'}
					</label>
					<input
						id="credential"
						type="password"
						bind:value={credential}
						autocomplete="off"
						maxlength="4096"
						class="w-full rounded-lg border border-input bg-background px-3 py-2.5 text-sm outline-none ring-ring focus:ring-2"
					/>
					{#if error}<p class="text-sm text-destructive" role="alert">{error}</p>{/if}
					<button
						type="submit"
						disabled={!credential || submitting}
						class="w-full rounded-lg bg-primary px-4 py-2.5 text-sm font-semibold text-primary-foreground disabled:opacity-50"
					>
						{submitting ? 'Signing in…' : 'Sign in'}
					</button>
				</form>
			{/if}

			<div class="mt-5 flex items-start gap-2 border-t border-border pt-4 text-xs leading-relaxed text-muted-foreground">
				<Server size={14} class="mt-0.5 shrink-0" />
				<span>App authentication protects XpressClaw data, but does not encrypt the connection. Use a trusted tailnet or terminate TLS in front of remote instances.</span>
			</div>
		</section>
		{#if desktopAttempted}
			<p class="mt-3 text-center text-[11px] text-muted-foreground">Desktop credentials are read from your operating-system keychain and are never copied into this page.</p>
		{/if}
	</div>
</main>
