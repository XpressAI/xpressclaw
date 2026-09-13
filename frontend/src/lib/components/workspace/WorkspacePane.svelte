<script lang="ts">
	import type { WorkspacePaneState, WorkspaceTab } from '$lib/workspace';
	import SortableTab from './SortableTab.svelte';
	import TabStripDropZone from './TabStripDropZone.svelte';
	import WorkspaceContent from './WorkspaceContent.svelte';

	let {
		pane,
		focused,
		compact,
		canSplit,
		onfocus,
		onactivate,
		onclose,
		oncontext,
		onsplit,
	}: {
		pane: WorkspacePaneState;
		focused: boolean;
		compact: boolean;
		canSplit: boolean;
		onfocus: () => void;
		onactivate: (tab: WorkspaceTab) => void;
		onclose: (tab: WorkspaceTab) => void;
		oncontext: (event: MouseEvent, tab: WorkspaceTab) => void;
		onsplit: () => void;
	} = $props();

	let tabStrip = $state<HTMLDivElement>();
	let activeTab = $derived(pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0]);

	$effect(() => {
		pane.activeTabId;
		if (!tabStrip) return;
		const frame = window.requestAnimationFrame(() => {
			tabStrip
				?.querySelector<HTMLElement>('[data-workspace-tab-active="true"]')
				?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		});
		return () => window.cancelAnimationFrame(frame);
	});
</script>

<section
	class="workspace-pane flex min-w-0 flex-1 flex-col overflow-hidden bg-background {focused ? 'ring-1 ring-inset ring-primary/25' : ''}"
	onpointerdown={onfocus}
	onfocusin={onfocus}
	role="group"
	aria-label="Workspace pane"
>
	<div class="hidden h-9 shrink-0 items-stretch border-b border-border bg-[hsl(var(--field))] lg:flex">
		<TabStripDropZone
			paneId={pane.id}
			bind:element={tabStrip}
			data-workspace-tab-strip
			data-workspace-pane-id={pane.id}
			role="group"
			aria-label="Pane tabs"
			class="flex min-w-0 flex-1 overflow-x-auto scrollbar-hide"
		>
			{#each pane.tabs as tab, index (tab.id)}
				<SortableTab
					{tab}
					{index}
					group={pane.id}
					isActive={tab.id === pane.activeTabId}
					onactivate={() => onactivate(tab)}
					onclose={() => onclose(tab)}
					oncontext={(event) => oncontext(event, tab)}
				/>
			{/each}
		</TabStripDropZone>
		<button type="button" onclick={onsplit} disabled={!canSplit} class="flex h-9 w-9 shrink-0 items-center justify-center border-l border-border/70 text-muted-foreground hover:bg-accent hover:text-foreground disabled:cursor-not-allowed disabled:opacity-25" title={canSplit ? 'Split active tab right' : 'No room for another pane'} aria-label="Split active tab right">
			<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><rect x="3.5" y="4" width="17" height="16" rx="2"/><path d="M12 4v16"/></svg>
		</button>
	</div>

	<div class="min-h-0 flex-1 overflow-hidden">
		{#if activeTab}
			{#key activeTab.id}
				<WorkspaceContent tab={activeTab} {compact} />
			{/key}
		{/if}
	</div>
</section>
