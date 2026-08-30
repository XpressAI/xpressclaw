<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import WorkspaceShell from '$lib/components/workspace/WorkspaceShell.svelte';
	import { auth, type AuthBootstrap } from '$lib/api';
	import { initializeTheme } from '$lib/theme';

	let { children } = $props();
	let setupRoute = $derived($page.url.pathname.startsWith('/setup'));
	let loginRoute = $derived($page.url.pathname === '/login');
	let authenticationReady = $state(false);
	type ActiveDesktopProfile = {
		identity_status: 'unpinned' | 'matched' | 'changed';
		navigation_status?: 'ready' | 'confirmation_required' | 'profile_review_required';
		local: boolean;
	};

	async function redirectToLogin(): Promise<void> {
		const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
		try {
			// Keep the workspace gated until /login is active. If navigation itself
			// fails, the connecting screen is safer than unlocking the current page.
			await goto(`/login?return_to=${encodeURIComponent(returnTo)}`, { replaceState: true });
			authenticationReady = true;
		} catch {
			// Leave authenticationReady false and fail closed.
		}
	}

	async function bootstrapAuthentication(): Promise<void> {
		let session: AuthBootstrap;
		try {
			session = await auth.bootstrap();
		} catch {
			// Preserve the existing offline/reconnecting workspace experience when
			// the server itself cannot answer. Desktop policy failures are handled
			// separately below and never enter this fallback.
			authenticationReady = true;
			return;
		}

		if ('__TAURI_INTERNALS__' in window) {
			try {
				const { invoke } = await import('@tauri-apps/api/core');
				const active = await invoke<ActiveDesktopProfile>('get_active_instance_profile');
				if (active.identity_status === 'changed') {
					// Identity recovery belongs to the login page even when the
					// replacement has authentication disabled. Native command guards
					// also enforce this pin, so skipping /login cannot grant access.
					await redirectToLogin();
					return;
				}
				if (!active.local && active.navigation_status !== 'ready') {
					// Revalidate the selected remote on every Desktop bootstrap. This
					// catches an already-open remote that restarted with authentication
					// disabled before any workspace content is made available.
					await redirectToLogin();
					return;
				}
				if (active.identity_status === 'unpinned' && active.local && !session.authentication_enabled) {
					// Establish the automatic local profile's first-use identity before
					// exposing profile commands. Authenticated instances do this in the
					// login flow, where native Desktop installs the browser session.
					await invoke('login_active_profile');
				}
			} catch {
				// A rejected native profile/identity check is not an offline server
				// state. Route it to the recovery UI and never unlock the workspace.
				await redirectToLogin();
				return;
			}
		}

		if (session.authentication_enabled && !session.authenticated) {
			await redirectToLogin();
			return;
		}
		authenticationReady = true;
	}

	onMount(() => {
		initializeTheme();
		// The login page owns its bootstrap request so it can render the
		// credential mode and attempt Desktop keychain login exactly once.
		if (loginRoute) {
			authenticationReady = true;
			return;
		}
		void bootstrapAuthentication();
	});
</script>

{#if loginRoute}
	{@render children()}
{:else if !authenticationReady}
	<main class="grid min-h-screen place-items-center bg-background text-foreground" aria-busy="true">
		<p class="text-sm text-muted-foreground">Connecting to XpressClaw…</p>
	</main>
{:else if setupRoute}
	{@render children()}
{:else}
	<WorkspaceShell>{@render children()}</WorkspaceShell>
{/if}
