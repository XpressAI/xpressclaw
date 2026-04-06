<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data } = $props();
</script>

<div
	class="w-[200px] rounded-lg border border-emerald-800/60 bg-emerald-950/40 shadow-lg select-none"
>
	<div class="flex items-center gap-2.5 px-3 py-2.5">
		<!-- Bolt icon -->
		<div
			class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg bg-emerald-600/20"
		>
			<svg class="h-3.5 w-3.5 text-emerald-400" fill="currentColor" viewBox="0 0 24 24">
				<path d="M13 2L3 14h9l-1 10 10-12h-9l1-10z" />
			</svg>
		</div>
		<div class="min-w-0 flex-1">
			<div class="text-[10px] font-semibold uppercase tracking-wider text-emerald-400">
				Trigger
			</div>
			<div class="truncate text-xs font-medium text-[hsl(220,20%,92%)]">
				{data.connector || 'webhook'}
			</div>
		</div>
	</div>

	{#if data.channel || data.event}
		<div class="border-t border-emerald-800/40 px-3 py-2 space-y-0.5">
			{#if data.channel}
				<div class="flex items-center gap-1.5 text-[10px] text-[hsl(225,15%,55%)]">
					<span class="text-emerald-500/70">#</span>
					<span class="truncate">{data.channel}</span>
				</div>
			{/if}
			{#if data.event}
				<div class="flex items-center gap-1.5 text-[10px] text-[hsl(225,15%,55%)]">
					<svg class="h-2.5 w-2.5 flex-shrink-0 text-emerald-500/70" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
						<path stroke-linecap="round" stroke-linejoin="round" d="M3.75 13.5l10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75z" />
					</svg>
					<span class="truncate">{data.event}</span>
				</div>
			{/if}
		</div>
	{/if}

	<!-- Source handle (right) -->
	<Handle type="source" position={Position.Right} class="!w-2.5 !h-2.5 !bg-emerald-400 !border-emerald-800 !border-2" />
</div>
