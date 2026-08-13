<script lang="ts">
	import type { WorkspaceTabKind } from '$lib/workspace';
	import SettingsPage from '../../../routes/settings/+page.svelte';
	import McpSettingsPage from '../../../routes/settings/mcp/+page.svelte';
	import ServerSettingsPage from '../../../routes/settings/server/+page.svelte';
	import SyncSettingsPage from '../../../routes/settings/sync/+page.svelte';

	let { kind }: { kind: WorkspaceTabKind } = $props();
</script>

<div class="h-full min-h-0 overflow-y-auto">
	{#if kind === 'settings-server'}
		<ServerSettingsPage />
	{:else if kind === 'settings-mcp'}
		<McpSettingsPage />
	{:else if kind === 'settings-sync'}
		<SyncSettingsPage />
	{:else}
		<SettingsPage />
	{/if}
</div>
