<script lang="ts">
	import type { Connector } from '$lib/api';

	let {
		connector = '', channel = '', event = '',
		expanded = false, compact = false,
		connectorList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
	}: {
		connector?: string; channel?: string; event?: string;
		expanded?: boolean; compact?: boolean; connectorList?: Connector[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
	} = $props();
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-red-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">TRIGGER</span>
		<span class="text-sm text-foreground flex-1">{channel ? `On ${channel}` : 'Configure trigger'}</span>
		<span class="text-xs text-muted-foreground font-mono truncate max-w-[40%] text-right">
			{[connector, channel, event].filter(Boolean).join(' · ')}
		</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-red-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">TRIGGER</span>
			<span class="text-sm font-medium text-foreground flex-1">{channel ? `On ${channel}` : 'Configure trigger'}</span>
			<span class="text-xs text-muted-foreground font-mono">{event}</span>
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
		</div>

		{#if expanded}
			<div class="px-1 pb-3 pt-1">
				<div class="grid grid-cols-2 gap-2">
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">CHANNEL</label>
						<div class="rounded bg-muted px-2.5 py-1.5">
							<input type="text" value={channel} oninput={(e) => onupdate({ channel: e.currentTarget.value })}
								class="w-full bg-transparent text-xs text-foreground focus:outline-none" placeholder="#channel-name" />
						</div>
					</div>
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">EVENT</label>
						<div class="rounded bg-muted px-2.5 py-1.5">
							<input type="text" value={event} oninput={(e) => onupdate({ event: e.currentTarget.value })}
								class="w-full bg-transparent text-xs text-foreground focus:outline-none" placeholder="message_received" />
						</div>
					</div>
				</div>
			</div>
		{/if}
	</div>
{/if}
