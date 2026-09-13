<script lang="ts">
	import { createSortable } from '@dnd-kit/svelte/sortable';
	import type { WorkspaceTab } from '$lib/workspace';

	// One sortable tab. dnd-kit needs each sortable in its own component so the
	// item keeps a stable identity across reorders, so both the pane strip and
	// the compact strip render this rather than inlining the markup.
	let {
		tab,
		index,
		group,
		isActive,
		widthClass = 'max-w-56',
		closeClass = 'text-muted-foreground/50 opacity-0 group-hover:opacity-100',
		onactivate,
		onclose,
		oncontext,
	}: {
		tab: WorkspaceTab;
		index: number;
		group: string;
		isActive: boolean;
		widthClass?: string;
		closeClass?: string;
		onactivate: () => void;
		onclose: () => void;
		oncontext: (event: MouseEvent) => void;
	} = $props();

	const sortable = createSortable({
		get id() {
			return tab.id;
		},
		get index() {
			return index;
		},
		get group() {
			return group;
		},
		type: 'workspace-tab',
		accept: 'workspace-tab',
	});

	function statusClass(status: string | null): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'bg-blue-500 animate-pulse';
		if (status === 'queued' || status === 'pending') return 'bg-amber-400';
		if (status === 'completed') return 'bg-emerald-500';
		return 'bg-muted-foreground/45';
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	{@attach sortable.attach}
	data-workspace-tab
	data-workspace-tab-id={tab.id}
	data-workspace-tab-title={tab.title}
	data-workspace-tab-active={isActive}
	data-workspace-tab-dragging={sortable.isDragging}
	oncontextmenu={oncontext}
	class="group relative flex min-w-0 {widthClass} shrink-0 cursor-grab items-center border-r border-border/70 transition-colors active:cursor-grabbing {sortable.isDragging ? 'opacity-40' : ''} {isActive ? 'bg-card font-semibold text-primary shadow-[inset_0_0_0_1px_hsl(var(--border-strong))]' : 'text-muted-foreground hover:bg-[hsl(var(--hover))] hover:text-foreground'}"
>
	<button type="button" onclick={onactivate} aria-current={isActive ? 'page' : undefined} class="flex min-w-0 flex-1 items-center gap-2 py-2 pl-3 text-left text-xs" title={tab.title}>
		{#if tab.status}<span class="h-1.5 w-1.5 shrink-0 rounded-full {statusClass(tab.status)}"></span>{/if}
		<span class="truncate">{tab.title}</span>
	</button>
	<!-- data-no-drag keeps the pointer sensor from claiming a press on the close
	     control, so a click never has to out-race a drag activation. -->
	<button
		type="button"
		data-no-drag
		onclick={(event) => { event.stopPropagation(); onclose(); }}
		class="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded text-sm hover:bg-accent hover:text-foreground {closeClass} {isActive ? 'opacity-80' : ''}"
		aria-label="Close {tab.title}"
	>×</button>
	{#if isActive}<span data-active-tab-indicator class="pointer-events-none absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-primary" aria-hidden="true"></span>{/if}
</div>
