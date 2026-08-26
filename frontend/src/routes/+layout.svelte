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

	onMount(() => {
		initializeTheme();
		// The login page owns its bootstrap request so it can render the
		// credential mode and attempt Desktop keychain login exactly once.
		if (loginRoute) {
			authenticationReady = true;
			return;
		}
		void auth.bootstrap().then((session) => {
			if (session.authentication_enabled && !session.authenticated) {
				const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
				// The root layout persists across client-side navigation. Mark its
				// bootstrap complete before visiting /login so a successful login can
				// return here without leaving the workspace behind this loading state.
				authenticationReady = true;
				void goto(`/login?return_to=${encodeURIComponent(returnTo)}`, { replaceState: true });
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
