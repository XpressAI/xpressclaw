<script lang="ts">
	import type { WorkspacePaneState, WorkspaceTab } from '$lib/workspace';
	import { revealActiveTab } from '$lib/tabStrip';
	import { measurePane } from '$lib/workspaceLayout';
	import SortableTab from './SortableTab.svelte';
	import TabStripDropZone from './TabStripDropZone.svelte';
	import WorkspaceContent from './WorkspaceContent.svelte';

	let {
		pane,
		focused,
		compact,
		canSplit,
		showTabs = true,
		splitDirection = 'right',
		onbrowse,
		onfocus,
		onactivate,
		onclose,
		oncontext,
		onsplit,
		dropIndex = null,
	}: {
		pane: WorkspacePaneState;
		focused: boolean;
		compact: boolean;
		canSplit: boolean;
		showTabs?: boolean;
		splitDirection?: 'right' | 'below';
		onbrowse?: () => void;
		onfocus: () => void;
		onactivate: (tab: WorkspaceTab) => void;
		onclose: (tab: WorkspaceTab) => void;
		oncontext: (event: MouseEvent, tab: WorkspaceTab) => void;
		onsplit: () => void;
		dropIndex?: number | null;
	} = $props();

	let tabStrip = $state<HTMLDivElement>();
	let activeTab = $derived(pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0]);
	let activeTabId = $derived(activeTab?.id);
	let splitLabel = $derived(`Split active tab ${splitDirection}`);

	$effect(() => {
		activeTabId;
		if (!tabStrip) return;
		const frame = window.requestAnimationFrame(() => {
			if (tabStrip) revealActiveTab(tabStrip);
		});
		return () => window.cancelAnimationFrame(frame);
	});
</script>

<section
	use:measurePane
	class="workspace-pane flex min-w-0 flex-1 flex-col overflow-hidden bg-background {focused ? 'ring-1 ring-inset ring-primary/25' : ''}"
	onpointerdown={onfocus}
	onfocusin={onfocus}
	role="group"
	aria-label="Workspace pane"
>
	<div style:display={showTabs ? 'flex' : 'none'} class="h-9 shrink-0 items-stretch border-b border-border bg-[hsl(var(--field))]">
		{#if onbrowse}<button type="button" onclick={onbrowse} aria-label="Browse workspace" class="flex h-9 w-9 shrink-0 items-center justify-center text-muted-foreground hover:bg-accent"><svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" d="M4 6h16M4 12h16M4 18h16" /></svg></button>{/if}
		<TabStripDropZone
			paneId={pane.id}
			bind:element={tabStrip}
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
					dropBefore={dropIndex === index}
					onactivate={() => onactivate(tab)}
					onclose={() => onclose(tab)}
					oncontext={(event) => oncontext(event, tab)}
				/>
			{/each}
			{#if dropIndex === pane.tabs.length}<span data-tab-drop-indicator class="w-0.5 shrink-0 self-stretch bg-primary" aria-hidden="true"></span>{/if}
		</TabStripDropZone>
		<button type="button" onclick={onsplit} disabled={!canSplit} class="flex h-9 w-9 shrink-0 items-center justify-center border-l border-border/70 text-muted-foreground hover:bg-accent hover:text-foreground disabled:cursor-not-allowed disabled:opacity-25" title={canSplit ? splitLabel : 'No room for another pane'} aria-label={splitLabel}>
			<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><rect x="3.5" y="4" width="17" height="16" rx="2"/><path d={splitDirection === 'below' ? 'M4 12h16' : 'M12 4v16'}/></svg>
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
