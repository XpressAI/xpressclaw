<script lang="ts">
	import type { Connector } from '$lib/api';

	let {
		connector = '', channel = '', event = '',
		expanded = false,
		connectorList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		connector?: string; channel?: string; event?: string;
		expanded?: boolean; connectorList?: Connector[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();
</script>

<div class="group rounded-lg border border-border/60 bg-emerald-950/20 border-l-[3px] border-l-emerald-500">
	<!-- Header -->
	<div class="flex items-center gap-2 px-3 py-2">
		<button class="flex items-center gap-2 flex-1 text-left min-w-0" onclick={ontoggle}>
			<span class="text-[10px] font-bold tracking-wider text-emerald-400">TRIGGER</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">
				{connector || 'Configure trigger'}{event ? ` — ${event}` : ''}
			</span>
			<svg class="h-3.5 w-3.5 text-muted-foreground transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
		</button>
		<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
			<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
		</button>
	</div>

	{#if expanded}
		<div class="border-t border-border/40 px-3 py-3 space-y-3">
			<div class="grid grid-cols-2 gap-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">CHANNEL</label>
					<input type="text" value={channel} oninput={(e) => onupdate({ channel: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="#channel-name" />
				</div>
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">EVENT</label>
					<input type="text" value={event} oninput={(e) => onupdate({ event: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="message_received" />
				</div>
			</div>
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">CONNECTOR</label>
				<select value={connector} onchange={(e) => onupdate({ connector: e.currentTarget.value })}
					class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
					<option value="">Select connector...</option>
					{#each connectorList as c}<option value={c.name}>{c.name} ({c.connector_type})</option>{/each}
					<option value="webhook">webhook</option>
					<option value="telegram">telegram</option>
					<option value="file_watcher">file_watcher</option>
				</select>
			</div>
		</div>
	{/if}
</div>
