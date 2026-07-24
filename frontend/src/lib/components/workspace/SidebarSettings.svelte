<script lang="ts">
	import { SETTINGS_SECTIONS } from '$lib/settingsSections';
	import type { WorkspaceTabKind } from '$lib/workspace';

	let {
		activeKind,
		compact = false,
		showHeading = true,
		onnavigate,
	}: {
		activeKind: WorkspaceTabKind;
		compact?: boolean;
		showHeading?: boolean;
		onnavigate?: () => void;
	} = $props();
</script>

{#if compact}
	<div data-sidebar-mode="settings" class="flex flex-col items-center gap-1">
		{#each SETTINGS_SECTIONS as section (section.kind)}
			<a
				href={section.href}
				onclick={onnavigate}
				aria-current={activeKind === section.kind ? 'page' : undefined}
				class="flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {activeKind === section.kind ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}"
				title={section.label}
			>
				{section.shortLabel}
			</a>
		{/each}
	</div>
{:else}
	<div data-sidebar-mode="settings">
		{#if showHeading}
			<div class="mb-1.5 px-2 text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Settings</div>
		{/if}
		<div class="space-y-0.5">
			{#each SETTINGS_SECTIONS as section (section.kind)}
				<a
					data-sidebar-setting={section.kind}
					href={section.href}
					onclick={onnavigate}
					aria-current={activeKind === section.kind ? 'page' : undefined}
					class="flex items-center gap-2 rounded-lg px-2 py-2 text-xs transition-colors {activeKind === section.kind ? 'bg-[hsl(var(--sidebar-active))] font-medium text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}"
				>
					<span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-[10px] font-semibold">{section.shortLabel}</span>
					<span class="truncate">{section.label}</span>
				</a>
			{/each}
		</div>
	</div>
{/if}
