<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data, id } = $props();

	let outputs: string[] = $derived(data.outputs ?? []);
</script>

<div
	class="w-[220px] rounded-lg border border-amber-700/50 bg-amber-950/30 shadow-lg select-none"
>
	<!-- Target handle (top) -->
	<Handle type="target" position={Position.Top} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />

	<div class="flex items-center gap-2.5 px-3 py-2.5">
		<div class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg bg-amber-600/20">
			<svg class="h-4 w-4 text-amber-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
				<path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" />
			</svg>
		</div>
		<div class="min-w-0 flex-1">
			<div class="text-[10px] font-semibold uppercase tracking-wider text-amber-400">Branch</div>
			<div class="truncate text-xs font-medium text-[hsl(220,20%,92%)]">
				{data.label || id}
			</div>
		</div>
	</div>

	{#if outputs.length > 0}
		<div class="border-t border-amber-800/30 px-3 py-2 space-y-0.5">
			{#each outputs as output}
				<div class="flex items-center gap-1.5 text-[10px] text-amber-300/70">
					<svg class="h-2.5 w-2.5 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
						<path stroke-linecap="round" stroke-linejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" />
					</svg>
					<span class="truncate font-mono">{output}</span>
				</div>
			{/each}
		</div>
	{:else}
		<div class="border-t border-amber-800/30 px-3 py-2">
			<div class="text-[10px] text-amber-300/50 italic">Connect edges to define branches</div>
		</div>
	{/if}

	<!-- Source handles (bottom) — one per output, or one default -->
	{#if outputs.length > 1}
		{#each outputs as output, i}
			<Handle
				type="source"
				position={Position.Bottom}
				id={output}
				style="left: {20 + (i * 60) / (outputs.length)}%"
				class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2"
			/>
		{/each}
	{:else}
		<Handle type="source" position={Position.Bottom} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />
	{/if}
</div>
