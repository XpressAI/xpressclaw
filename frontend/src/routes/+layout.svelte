<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import WorkspaceShell from '$lib/components/workspace/WorkspaceShell.svelte';
	import { initializeTheme } from '$lib/theme';

	let { children } = $props();
	let setupRoute = $derived($page.url.pathname.startsWith('/setup'));

	onMount(initializeTheme);
</script>

{#if setupRoute}
	{@render children()}
{:else}
	<WorkspaceShell>{@render children()}</WorkspaceShell>
{/if}
