<script lang="ts">
	import type { WorkspaceTabKind } from '$lib/workspace';
	import SettingsPage from '../../../routes/settings/+page.svelte';
	import ServerSettingsPage from '../../../routes/settings/server/+page.svelte';

	let { kind }: { kind: WorkspaceTabKind } = $props();

	const sections = [
		{ kind: 'settings', label: 'Profile', href: '/settings' },
		{ kind: 'settings-server', label: 'Server', href: '/settings/server' },
	] as const;
</script>

<div class="flex h-full min-h-0 flex-col">
	<nav class="flex h-10 shrink-0 items-end gap-1 overflow-x-auto border-b border-border px-4 scrollbar-hide" aria-label="Settings sections">
		{#each sections as section}
			<a href={section.href} class="border-b-2 px-3 py-2 text-xs transition-colors {kind === section.kind ? 'border-primary font-medium text-foreground' : 'border-transparent text-muted-foreground hover:text-foreground'}">{section.label}</a>
		{/each}
	</nav>
	<div class="min-h-0 flex-1 overflow-y-auto">
		{#if kind === 'settings-server'}
			<ServerSettingsPage />
		{:else}
			<SettingsPage />
		{/if}
	</div>
</div>
