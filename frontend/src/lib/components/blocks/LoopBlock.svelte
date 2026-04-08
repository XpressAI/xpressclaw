<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		label = '', overVar = '', asVar = 'item',
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {},
		children
	}: {
		label?: string; overVar?: string; asVar?: string;
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
		children?: Snippet;
	} = $props();
</script>

<div class="group rounded-lg border-2 border-dashed border-amber-600/40 bg-amber-950/10 loop-border">
	<!-- Header -->
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-red-400">LOOP</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'For each'}</span>
		{#if overVar}
			<span class="text-[10px] text-muted-foreground font-mono">{overVar} as {asVar}</span>
		{/if}
		{#if !compact}
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if expanded && !compact}
		<div class="border-t border-dashed border-amber-600/30 px-3 py-2 space-y-2">
			<div class="grid grid-cols-2 gap-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">ITERATING</label>
					<input type="text" value={overVar} oninput={(e) => onupdate({ overVar: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="@step.entities" />
				</div>
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">AS</label>
					<input type="text" value={asVar} oninput={(e) => onupdate({ asVar: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="entity" />
				</div>
			</div>
		</div>
	{/if}

	<!-- Nested content (child blocks) -->
	{#if !compact}
		<div class="px-4 py-2">
			{#if children}
				{@render children()}
			{:else}
				<div class="rounded border border-dashed border-border/30 p-3 text-center text-[10px] text-muted-foreground/50">
					Nested steps will appear here
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.loop-border {
		background-image: repeating-linear-gradient(
			90deg,
			transparent,
			transparent 4px,
			hsl(25 95% 53% / 0.15) 4px,
			hsl(25 95% 53% / 0.15) 8px
		);
		background-size: 100% 2px;
		background-position: 0 0, 0 100%;
		background-repeat: no-repeat;
	}
</style>
