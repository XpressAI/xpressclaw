<script lang="ts">
	import { tabDropIndex, type WorkspacePaneState, type WorkspaceTab, type WorkspaceTabDrag } from '$lib/workspace';
	import WorkspaceContent from './WorkspaceContent.svelte';

	let {
		pane,
		focused,
		compact,
		canSplit,
		drag,
		onfocus,
		onactivate,
		onclose,
		oncontext,
		onsplit,
		ontabdragstart,
		ontabdragover,
		ontabdragleave,
		ontabdrop,
		ontabdragend,
	}: {
		pane: WorkspacePaneState;
		focused: boolean;
		compact: boolean;
		canSplit: boolean;
		drag: WorkspaceTabDrag | null;
		onfocus: () => void;
		onactivate: (tab: WorkspaceTab) => void;
		onclose: (tab: WorkspaceTab) => void;
		oncontext: (event: MouseEvent, tab: WorkspaceTab) => void;
		onsplit: () => void;
		ontabdragstart: (event: DragEvent, tab: WorkspaceTab) => void;
		ontabdragover: (event: DragEvent, index: number) => void;
		ontabdragleave: () => void;
		ontabdrop: (event: DragEvent, index: number) => void;
		ontabdragend: () => void;
	} = $props();

	let tabStrip = $state<HTMLDivElement>();
	let activeTab = $derived(pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0]);
	let dropIndex = $derived(drag && drag.target && drag.target.paneId === pane.id ? drag.target.index : null);

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

	function overTab(event: DragEvent): boolean {
		return Boolean((event.target as HTMLElement | null)?.closest('[data-workspace-tab]'));
	}

	// Empty strip space past the last tab appends; the tab handlers own everything else.
	function stripDragOver(event: DragEvent) {
		if (!drag || overTab(event)) return;
		ontabdragover(event, pane.tabs.length);
	}

	function stripDragLeave(event: DragEvent) {
		if (!drag) return;
		const strip = event.currentTarget as HTMLElement | null;
		if (strip && event.relatedTarget instanceof Node && strip.contains(event.relatedTarget)) return;
		ontabdragleave();
	}

	function stripDrop(event: DragEvent) {
		if (!drag || overTab(event)) return;
		ontabdrop(event, pane.tabs.length);
	}

	function statusClass(status: string | null): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'bg-blue-500 animate-pulse';
		if (status === 'queued' || status === 'pending') return 'bg-amber-400';
		if (status === 'completed') return 'bg-emerald-500';
		return 'bg-muted-foreground/45';
	}
</script>

<section
	class="workspace-pane flex min-w-0 flex-1 flex-col overflow-hidden bg-background {focused ? 'ring-1 ring-inset ring-primary/25' : ''}"
	onpointerdown={onfocus}
	onfocusin={onfocus}
	role="group"
	aria-label="Workspace pane"
>
	<div class="hidden h-9 shrink-0 items-stretch border-b border-border bg-[hsl(var(--field))] lg:flex">
		<div
			bind:this={tabStrip}
			data-workspace-tab-strip
			data-workspace-drop-index={dropIndex}
			ondragover={stripDragOver}
			ondragleave={stripDragLeave}
			ondrop={stripDrop}
			role="group"
			aria-label="Pane tabs"
			class="flex min-w-0 flex-1 overflow-x-auto scrollbar-hide"
		>
			{#each pane.tabs as tab, index (tab.id)}
				{@const isActive = tab.id === pane.activeTabId}
				{@const isDragged = Boolean(drag) && drag?.source.paneId === pane.id && drag?.source.tabId === tab.id}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					data-workspace-tab
					data-workspace-tab-title={tab.title}
					data-workspace-tab-active={isActive}
					data-workspace-tab-dragging={isDragged}
					draggable="true"
					ondragstart={(event) => ontabdragstart(event, tab)}
					ondragover={(event) => ontabdragover(event, tabDropIndex(event, index))}
					ondrop={(event) => ontabdrop(event, tabDropIndex(event, index))}
					ondragend={ontabdragend}
					oncontextmenu={(event) => oncontext(event, tab)}
					class="group relative flex min-w-0 max-w-56 shrink-0 cursor-grab items-center border-r border-border/70 transition-colors active:cursor-grabbing {isDragged ? 'opacity-40' : ''} {isActive ? 'bg-card font-semibold text-primary shadow-[inset_0_0_0_1px_hsl(var(--border-strong))]' : 'text-muted-foreground hover:bg-[hsl(var(--hover))] hover:text-foreground'}"
				>
					<button type="button" onclick={() => onactivate(tab)} aria-current={isActive ? 'page' : undefined} class="flex min-w-0 flex-1 items-center gap-2 py-2 pl-3 text-left text-xs" title={tab.title}>
						{#if tab.status}<span class="h-1.5 w-1.5 shrink-0 rounded-full {statusClass(tab.status)}"></span>{/if}
						<span class="truncate">{tab.title}</span>
					</button>
					<button type="button" onclick={(event) => { event.stopPropagation(); onclose(tab); }} class="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded text-sm text-muted-foreground/50 opacity-0 hover:bg-accent hover:text-foreground group-hover:opacity-100 {isActive ? 'opacity-80' : ''}" aria-label="Close {tab.title}">×</button>
					{#if dropIndex === index}<span data-tab-drop-indicator class="pointer-events-none absolute inset-y-0 left-0 z-10 w-0.5 bg-primary" aria-hidden="true"></span>{/if}
					{#if isActive}<span data-active-tab-indicator class="pointer-events-none absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-primary" aria-hidden="true"></span>{/if}
				</div>
			{/each}
			{#if dropIndex === pane.tabs.length}<span data-tab-drop-indicator class="w-0.5 shrink-0 self-stretch bg-primary" aria-hidden="true"></span>{/if}
		</div>
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
