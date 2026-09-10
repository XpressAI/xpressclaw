<script lang="ts">
	import type { DashboardRange, DashboardTokenUsage } from '$lib/api';
	let { usage, range }: { usage: DashboardTokenUsage | undefined; range: DashboardRange } = $props();
	function count(value: number | null | undefined): string {
		return value == null ? '—' : value.toLocaleString();
	}
</script>

<div class="border-b border-border/70 px-4 py-3" data-token-usage>
	<div class="flex flex-wrap items-baseline justify-between gap-2">
		<h3 class="text-xs font-semibold">Reported tokens · {range}</h3>
		<span class="text-[10px] text-muted-foreground">{usage?.reported_responses ?? 0} responses with counts</span>
	</div>
	<dl class="mt-2 grid grid-cols-3 gap-x-3 gap-y-2 text-xs">
		{#each [{ label: 'Total', value: usage?.total_tokens }, { label: 'Input', value: usage?.input_tokens }, { label: 'Output', value: usage?.output_tokens }, { label: 'Cache read', value: usage?.cached_read_tokens }, { label: 'Cache write', value: usage?.cached_write_tokens }, { label: 'Reasoning', value: usage?.thought_tokens }] as item}
			<div class="min-w-0"><dt class="text-[10px] text-muted-foreground">{item.label}</dt><dd class="mt-0.5 break-words font-mono font-medium" data-token-count={item.label}>{count(item.value)}</dd></div>
		{/each}
	</dl>
	<p class="mt-2 text-[10px] leading-relaxed text-muted-foreground">
		{#if !usage?.reported_responses}No usable token counts reported in this window. {/if}
		{#if usage?.unreported_responses}{usage.unreported_responses} responses without counts. {/if}
		{#if usage?.unclassified_responses}{usage.unclassified_responses} reports excluded because their accounting is unsupported. {/if}
		Counts arrive when responses finish. Optional categories include reported values only and may overlap; the reported total is used directly.
	</p>
	{#if usage?.recording_started_at}
		<p class="mt-1 text-[10px] text-muted-foreground">Recording since {new Date(usage.recording_started_at).toLocaleString()} · earlier usage is unavailable.</p>
	{/if}
</div>
