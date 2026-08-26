<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import WorkspaceShell from '$lib/components/workspace/WorkspaceShell.svelte';
	import { auth } from '$lib/api';
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

	onMount(() => {
		initializeTheme();
		// The login page owns its bootstrap request so it can render the
		// credential mode and attempt Desktop keychain login exactly once.
		if (loginRoute) {
			authenticationReady = true;
			return;
		}
		void auth.bootstrap().then(async (session) => {
			if ('__TAURI_INTERNALS__' in window) {
				const { invoke } = await import('@tauri-apps/api/core');
				const active = await invoke<ActiveDesktopProfile>('get_active_instance_profile');
				if (active.identity_status === 'changed') {
					const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
					// Identity recovery belongs to the login page even when the
					// replacement has authentication disabled. Native command guards
					// also enforce this pin, so skipping /login cannot grant access.
					await goto(`/login?return_to=${encodeURIComponent(returnTo)}`, { replaceState: true });
					authenticationReady = true;
					return;
				}
				if (!active.local && active.navigation_status !== 'ready') {
					const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
					// Revalidate the selected remote on every Desktop bootstrap. This
					// catches an already-open remote that restarted with authentication
					// disabled before any workspace content is made available.
					await goto(`/login?return_to=${encodeURIComponent(returnTo)}`, { replaceState: true });
					authenticationReady = true;
					return;
				}
				if (active.identity_status === 'unpinned' && active.local && !session.authentication_enabled) {
					// Establish the automatic local profile's first-use identity before
					// exposing profile commands. Authenticated instances do this in the
					// login flow, where native Desktop installs the browser session.
					await invoke('login_active_profile');
				}
			}
			if (session.authentication_enabled && !session.authenticated) {
				const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
				// The root layout persists across client-side navigation. Complete its
				// bootstrap after /login is active so no protected workspace can flash,
				// while a successful login can still return through this layout.
				await goto(`/login?return_to=${encodeURIComponent(returnTo)}`, { replaceState: true });
				authenticationReady = true;
				return;
			}
			authenticationReady = true;
		}).catch(() => {
			// Preserve the existing offline/reconnecting workspace experience.
			authenticationReady = true;
		});
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
