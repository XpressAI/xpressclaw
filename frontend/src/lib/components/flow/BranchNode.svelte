<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data, id } = $props();

	let outputs: string[] = $derived(data.outputs ?? []);
</script>

<div class="relative select-none" style="width: 160px; height: 100px;">
	<!-- Target handle (top) -->
	<Handle type="target" position={Position.Top} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />

	<!-- Diamond shape via rotated square -->
	<div class="absolute inset-0 flex items-center justify-center">
		<div class="w-[110px] h-[110px] rotate-45 rounded-lg border-2 border-amber-600/60 bg-amber-950/50 shadow-lg"
			style="position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%) rotate(45deg);"></div>
	</div>

	<!-- Content (not rotated) -->
	<div class="absolute inset-0 flex flex-col items-center justify-center z-10">
		<svg class="h-4 w-4 text-amber-400 mb-0.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
			<path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" />
		</svg>
		<div class="text-[10px] font-semibold text-amber-300 truncate max-w-[90px]">
			{data.label || id}
		</div>
	</div>

	<!-- Left output handle + label -->
	{#if outputs.length >= 1}
		<div class="absolute left-0 top-1/2 -translate-y-1/2 -translate-x-full pr-1 flex items-center gap-1 z-10">
			<span class="text-[9px] font-mono text-amber-300/80 whitespace-nowrap">{outputs[0]}</span>
		</div>
		<Handle
			type="source"
			position={Position.Left}
			id={outputs[0]}
			class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2"
		/>
	{/if}

	<!-- Right output handle + label -->
	{#if outputs.length >= 2}
		<div class="absolute right-0 top-1/2 -translate-y-1/2 translate-x-full pl-1 flex items-center gap-1 z-10">
			<span class="text-[9px] font-mono text-amber-300/80 whitespace-nowrap">{outputs[1]}</span>
		</div>
		<Handle
			type="source"
			position={Position.Right}
			id={outputs[1]}
			class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2"
		/>
	{/if}

	<!-- Bottom handle for 3+ outputs or default fallback -->
	{#if outputs.length >= 3}
		<div class="absolute bottom-0 left-1/2 -translate-x-1/2 translate-y-full pt-0.5 z-10">
			<span class="text-[9px] font-mono text-amber-300/80">{outputs[2]}</span>
		</div>
		<Handle
			type="source"
			position={Position.Bottom}
			id={outputs[2]}
			class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2"
		/>
	{:else if outputs.length === 0}
		<Handle type="source" position={Position.Bottom} class="!w-2.5 !h-2.5 !bg-amber-400 !border-amber-800 !border-2" />
	{/if}
</div>
