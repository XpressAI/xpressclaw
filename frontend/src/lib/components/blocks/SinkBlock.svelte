<script lang="ts">
	import type { Connector } from '$lib/api';

	let {
		label = '', sinks = [],
		expanded = false, compact = false,
		connectorList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; sinks?: { connector: string; channel: string; template?: string }[];
		expanded?: boolean; compact?: boolean; connectorList?: Connector[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void; onremove?: () => void;
	} = $props();

	let detailText = $derived(
		sinks.length === 1
			? [sinks[0].connector, sinks[0].channel, sinks[0].template].filter(Boolean).join(' · ')
			: `${sinks.length} sinks`
	);
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-purple-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">NOTIFY</span>
		<span class="text-sm text-foreground flex-1 truncate">{label}</span>
		<span class="text-xs text-muted-foreground font-mono truncate max-w-[40%] text-right">{detailText}</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-purple-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">NOTIFY</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">{label}</span>
			<span class="text-xs text-muted-foreground font-mono">{detailText}</span>
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		</div>

		{#if expanded}
			<div class="px-1 pb-3 pt-1 space-y-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1 text-xs" />
				</div>
				{#each sinks as sink, si}
					<div class="rounded bg-muted p-2 space-y-1.5 relative">
						{#if sinks.length > 1}
							<button onclick={() => onupdate({ sinks: sinks.filter((_, i) => i !== si) })}
								class="absolute top-1 right-1 text-muted-foreground/40 hover:text-destructive text-xs">x</button>
						{/if}
						<div class="grid grid-cols-2 gap-2">
							<div>
								<label class="block text-[10px] text-muted-foreground mb-0.5">CHANNEL</label>
								<input type="text" value={sink.channel}
									oninput={(e) => { const s = [...sinks]; s[si] = { ...s[si], channel: e.currentTarget.value }; onupdate({ sinks: s }); }}
									class="w-full bg-transparent rounded border border-input/30 px-1.5 py-0.5 text-xs" placeholder="#channel" />
							</div>
							<div>
								<label class="block text-[10px] text-muted-foreground mb-0.5">TEMPLATE</label>
								<input type="text" value={sink.template || ''}
									oninput={(e) => { const s = [...sinks]; s[si] = { ...s[si], template: e.currentTarget.value }; onupdate({ sinks: s }); }}
									class="w-full bg-transparent rounded border border-input/30 px-1.5 py-0.5 text-xs font-mono" placeholder="@step.output" />
							</div>
						</div>
					</div>
				{/each}
				<button onclick={() => onupdate({ sinks: [...sinks, { connector: '', channel: '', template: '' }] })}
					class="text-[10px] text-primary hover:underline">+ Add sink</button>
			</div>
		{/if}
	</div>
{/if}
