<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';

	let { data, id } = $props();

	const statusColors: Record<string, string> = {
		completed: 'border-t-emerald-500',
		running: 'border-t-blue-500',
		pending: 'border-t-amber-500',
		failed: 'border-t-red-500',
		error: 'border-t-red-500',
		default: 'border-t-[hsl(225,18%,25%)]'
	};

	let borderClass = $derived(
		data.status ? (statusColors[data.status] ?? statusColors.default) : statusColors.default
	);

	function initials(name?: string): string {
		if (!name) return '?';
		return name
			.split(/[-_\s]+/)
			.slice(0, 2)
			.map((w: string) => w[0]?.toUpperCase() ?? '')
			.join('');
	}

	let outputs: string[] = $derived(data.outputs ?? []);
</script>

<div
	class="w-[220px] rounded-lg border border-[hsl(225,18%,18%)] bg-[hsl(228,22%,11%)] shadow-lg border-t-[3px] {borderClass} select-none"
>
	<!-- Target handle (top) -->
	<Handle type="target" position={Position.Top} class="!w-2.5 !h-2.5 !bg-[hsl(225,15%,40%)] !border-[hsl(225,18%,25%)] !border-2" />

	<!-- Header -->
	<div class="flex items-center gap-2 px-3 pt-2.5 pb-1.5">
		<div
			class="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-full bg-[hsl(225,50%,25%)] text-[10px] font-bold text-[hsl(220,20%,92%)]"
		>
			{initials(data.agent)}
		</div>
		<div class="min-w-0 flex-1">
			<div class="truncate text-xs font-semibold text-[hsl(220,20%,92%)]">
				{data.label || id}
			</div>
			{#if data.agent}
				<div class="truncate text-[10px] text-[hsl(225,15%,55%)]">{data.agent}</div>
			{/if}
		</div>
	</div>

	<!-- Prompt preview -->
	{#if data.prompt}
		<div class="px-3 pb-2">
			<div class="line-clamp-2 text-[10px] leading-relaxed text-[hsl(225,15%,55%)]">
				{data.prompt}
			</div>
		</div>
	{/if}

	<!-- Procedure badge -->
	{#if data.procedure}
		<div class="px-3 pb-2">
			<span
				class="inline-flex items-center gap-1 rounded bg-[hsl(225,50%,25%)/0.3] px-1.5 py-0.5 text-[10px] text-[hsl(225,65%,55%)]"
			>
				<svg class="h-2.5 w-2.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
					<path stroke-linecap="round" stroke-linejoin="round" d="M3.75 12h16.5m-16.5 3.75h16.5M3.75 19.5h16.5M5.625 4.5h12.75a1.875 1.875 0 010 3.75H5.625a1.875 1.875 0 010-3.75z" />
				</svg>
				{data.procedure}
			</span>
		</div>
	{/if}

	<!-- Source handle(s) (bottom) -->
	{#if outputs.length > 1}
		{#each outputs as output, i}
			<Handle
				type="source"
				position={Position.Bottom}
				id={output}
				style="left: {20 + (i * 60) / (outputs.length)}%"
				class="!w-2.5 !h-2.5 !bg-[hsl(225,65%,55%)] !border-[hsl(225,18%,25%)] !border-2"
			/>
		{/each}
	{:else}
		<Handle type="source" position={Position.Bottom} class="!w-2.5 !h-2.5 !bg-[hsl(225,65%,55%)] !border-[hsl(225,18%,25%)] !border-2" />
	{/if}
</div>
