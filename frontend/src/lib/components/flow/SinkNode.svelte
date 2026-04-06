<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data } = $props();

	let sinks: { connector: string; channel: string; template?: string }[] = $derived(
		data.sinks ?? []
	);

	function connectorIcon(connector: string): string {
		switch (connector.toLowerCase()) {
			case 'telegram': return 'T';
			case 'email': return '@';
			case 'slack': return 'S';
			case 'webhook': return 'W';
			default: return connector[0]?.toUpperCase() ?? '?';
		}
	}
</script>

<div
	class="w-[220px] rounded-lg border border-blue-800/60 bg-blue-950/40 shadow-lg select-none"
>
	<!-- Target handle (top) -->
	<Handle type="target" position={Position.Top} class="!w-2.5 !h-2.5 !bg-blue-400 !border-blue-800 !border-2" />

	<div class="flex items-center gap-2.5 px-3 py-2.5">
		<div
			class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg bg-blue-600/20"
		>
			<svg class="h-3.5 w-3.5 text-blue-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
				<path stroke-linecap="round" stroke-linejoin="round" d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5" />
			</svg>
		</div>
		<div class="min-w-0 flex-1">
			<div class="text-[10px] font-semibold uppercase tracking-wider text-blue-400">
				Sink
			</div>
			<div class="truncate text-xs font-medium text-[hsl(220,20%,92%)]">
				{data.label || 'Send Notification'}
			</div>
		</div>
	</div>

	{#if sinks.length > 0}
		<div class="border-t border-blue-800/40 px-3 py-2 space-y-1.5">
			{#each sinks as sink}
				<div class="flex items-center gap-2">
					<span
						class="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded bg-blue-600/20 text-[9px] font-bold text-blue-300"
					>
						{connectorIcon(sink.connector)}
					</span>
					<div class="min-w-0 flex-1">
						<div class="truncate text-[10px] text-[hsl(220,20%,92%)]">{sink.connector}</div>
						<div class="truncate text-[9px] text-[hsl(225,15%,55%)]">#{sink.channel}</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
