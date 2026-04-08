<script lang="ts">
	let {
		variables = [],
		filter = '',
		x = 0, y = 0,
		onselect = (_: string) => {},
		onclose = () => {}
	}: {
		variables?: { name: string; type?: string; source?: string }[];
		filter?: string;
		x?: number; y?: number;
		onselect?: (name: string) => void;
		onclose?: () => void;
	} = $props();

	let selectedIdx = $state(0);

	let filtered = $derived(
		variables.filter(v => !filter || v.name.toLowerCase().includes(filter.toLowerCase()))
	);

	// Reset selection when filter changes
	$effect(() => { filter; selectedIdx = 0; });

	export function handleKey(e: KeyboardEvent): boolean {
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			selectedIdx = Math.min(selectedIdx + 1, filtered.length - 1);
			return true;
		}
		if (e.key === 'ArrowUp') {
			e.preventDefault();
			selectedIdx = Math.max(selectedIdx - 1, 0);
			return true;
		}
		if (e.key === 'Enter' || e.key === 'Tab') {
			e.preventDefault();
			if (filtered.length > 0) onselect(filtered[selectedIdx].name);
			return true;
		}
		if (e.key === 'Escape') {
			e.preventDefault();
			onclose();
			return true;
		}
		return false;
	}
</script>

<svelte:window onclick={onclose} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="fixed z-50 rounded-lg border border-border bg-card shadow-xl py-1 min-w-[220px] max-h-[200px] overflow-y-auto"
	style="left: {x}px; top: {y}px" onclick={(e) => e.stopPropagation()}>
	{#if filtered.length === 0}
		<div class="px-3 py-2 text-xs text-muted-foreground">No matching variables</div>
	{:else}
		{#each filtered as v, i}
			<button onclick={() => onselect(v.name)}
				class="flex w-full items-center gap-2 px-3 py-1.5 text-xs transition-colors text-left
					{i === selectedIdx ? 'bg-accent text-foreground' : 'hover:bg-accent/50'}">
				<span class="text-amber-400 font-mono">@</span>
				<span class="font-mono text-foreground flex-1">{v.name}</span>
				{#if v.type}
					<span class="text-[9px] text-muted-foreground bg-muted rounded px-1">{v.type}</span>
				{/if}
			</button>
		{/each}
	{/if}
</div>
