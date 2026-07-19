<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';
	import WorkspaceShell from '$lib/components/workspace/WorkspaceShell.svelte';

	let { children } = $props();
	let setupRoute = $derived($page.url.pathname.startsWith('/setup'));
</script>

{#if setupRoute}
	{@render children()}
{:else}
	<WorkspaceShell>{@render children()}</WorkspaceShell>
{/if}
