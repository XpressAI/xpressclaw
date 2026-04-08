<script lang="ts">
	import type { Connector } from '$lib/api';

	let {
		label = '',
		sinks = [],
		expanded = false, compact = false,
		connectorList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string;
		sinks?: { connector: string; channel: string; template?: string }[];
		expanded?: boolean; compact?: boolean;
		connectorList?: Connector[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();
</script>

<div class="group rounded-lg border border-border/60 bg-purple-950/20 border-l-[3px] border-l-purple-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-purple-400">NOTIFY</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Send notification'}</span>
		{#if sinks.length > 0 && sinks[0].connector}
			<span class="text-[10px] text-muted-foreground bg-muted rounded px-1.5 py-0.5">{sinks[0].connector}</span>
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
		<div class="border-t border-border/40 px-3 py-3 space-y-3">
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
				<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
					class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
			</div>
			{#each sinks as sink, si}
				<div class="rounded border border-border/40 p-2 space-y-1.5 relative">
					{#if sinks.length > 1}
						<button onclick={() => onupdate({ sinks: sinks.filter((_, i) => i !== si) })}
							class="absolute top-1 right-1 text-muted-foreground/40 hover:text-destructive text-xs">x</button>
					{/if}
					<div class="grid grid-cols-2 gap-2">
						<div>
							<label class="block text-[10px] text-muted-foreground mb-0.5">CHANNEL</label>
							<input type="text" value={sink.channel}
								oninput={(e) => { const s = [...sinks]; s[si] = { ...s[si], channel: e.currentTarget.value }; onupdate({ sinks: s }); }}
								class="w-full rounded border border-input bg-background px-2 py-1 text-xs" placeholder="#channel" />
						</div>
						<div>
							<label class="block text-[10px] text-muted-foreground mb-0.5">TEMPLATE</label>
							<input type="text" value={sink.template || ''}
								oninput={(e) => { const s = [...sinks]; s[si] = { ...s[si], template: e.currentTarget.value }; onupdate({ sinks: s }); }}
								class="w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono" placeholder="@step.output" />
						</div>
					</div>
				</div>
			{/each}
			<button onclick={() => onupdate({ sinks: [...sinks, { connector: '', channel: '', template: '' }] })}
				class="text-[10px] text-primary hover:underline">+ Add sink</button>
		</div>
	{/if}
</div>
