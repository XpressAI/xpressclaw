<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data, id } = $props();

	let outputs: string[] = $derived(data.outputs ?? []);
</script>

<div class="w-[220px] rounded-lg border border-amber-700/50 bg-amber-950/30 shadow-lg select-none">
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

	<!-- Output handles with labels -->
	{#if outputs.length > 1}
		<div class="border-t border-amber-800/30 px-2 py-1.5">
			<div class="flex justify-between">
				{#each outputs as output, i}
					<div class="flex flex-col items-center relative" style="width: {100 / outputs.length}%">
						<span class="text-[9px] font-mono text-amber-300/80 mb-1 truncate max-w-full px-0.5">{output}</span>
						<Handle
							type="source"
							position={Position.Bottom}
							id={output}
							style="left: {((i + 0.5) / outputs.length) * 100}%; position: absolute; bottom: -8px;"
							class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2"
						/>
					</div>
				{/each}
			</div>
		</div>
	{:else if outputs.length === 1}
		<div class="border-t border-amber-800/30 px-3 py-1.5 text-center">
			<span class="text-[9px] font-mono text-amber-300/80">{outputs[0]}</span>
		</div>
		<Handle type="source" position={Position.Bottom} id={outputs[0]} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />
	{:else}
		<div class="border-t border-amber-800/30 px-3 py-2">
			<div class="text-[10px] text-amber-300/50 italic">Connect edges to define branches</div>
		</div>
		<Handle type="source" position={Position.Bottom} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />
	{/if}
</div>
