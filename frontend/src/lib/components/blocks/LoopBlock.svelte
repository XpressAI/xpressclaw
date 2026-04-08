<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		label = '', overVar = '', asVar = 'item',
		childCount = 0,
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {},
		children
	}: {
		label?: string; overVar?: string; asVar?: string;
		childCount?: number;
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void; onremove?: () => void;
		children?: Snippet;
	} = $props();

	let detailText = $derived(
		[overVar ? `${asVar}` : '', overVar, childCount > 0 ? `${childCount} steps` : ''].filter(Boolean).join(' · ')
	);
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-red-500 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">LOOP</span>
		<span class="text-sm text-foreground flex-1 truncate">{label}</span>
		<span class="text-xs text-muted-foreground font-mono truncate max-w-[40%] text-right">{detailText}</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-red-500 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">LOOP</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">{label}</span>
			<span class="text-xs text-muted-foreground font-mono">{overVar} as {asVar}</span>
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		</div>

		{#if expanded}
			<div class="pb-2 pt-1">
				<!-- Loop config -->
				<div class="px-1 grid grid-cols-2 gap-2 mb-2">
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">ITERATING</label>
						<div class="rounded bg-muted px-2.5 py-1.5">
							<input type="text" value={overVar} oninput={(e) => onupdate({ overVar: e.currentTarget.value })}
								class="w-full bg-transparent text-xs font-mono text-foreground focus:outline-none" placeholder="@step.entities" />
						</div>
					</div>
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">AS</label>
						<div class="rounded bg-muted px-2.5 py-1.5">
							<input type="text" value={asVar} oninput={(e) => onupdate({ asVar: e.currentTarget.value })}
								class="w-full bg-transparent text-xs font-mono text-foreground focus:outline-none" placeholder="entity" />
						</div>
					</div>
				</div>

				<!-- Nested content with animated border -->
				<div class="loop-animated-border rounded-lg mx-1 p-3">
					{#if children}
						{@render children()}
					{:else}
						<div class="text-center text-[10px] text-muted-foreground/50 py-2">
							No steps inside loop
						</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>
{/if}

<style>
	.loop-animated-border {
		--border-color: hsl(25 95% 53% / 0.4);
		background-image:
			repeating-linear-gradient(90deg, var(--border-color) 0, var(--border-color) 6px, transparent 6px, transparent 12px),
			repeating-linear-gradient(90deg, var(--border-color) 0, var(--border-color) 6px, transparent 6px, transparent 12px),
			repeating-linear-gradient(0deg, var(--border-color) 0, var(--border-color) 6px, transparent 6px, transparent 12px),
			repeating-linear-gradient(0deg, var(--border-color) 0, var(--border-color) 6px, transparent 6px, transparent 12px);
		background-size: 12px 2px, 12px 2px, 2px 12px, 2px 12px;
		background-position: 0 0, 0 100%, 0 0, 100% 0;
		background-repeat: repeat-x, repeat-x, repeat-y, repeat-y;
		animation: loop-march 0.8s linear infinite;
	}
	@keyframes loop-march {
		to { background-position: 12px 0, -12px 100%, 0 -12px, 100% 12px; }
	}
</style>
