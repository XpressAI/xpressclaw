<script lang="ts">
	import { onMount } from 'svelte';
	import type { ResourceCapacity } from '$lib/api';
	import { resourceStore } from '$lib/stores/resources.svelte';
	import { timeAgo } from '$lib/utils';

	let { needsAttention }: { needsAttention: number } = $props();
	let resources = $derived(resourceStore.resources);
	let loading = $derived(resourceStore.loading);
	let error = $derived(resourceStore.error);
	let stale = $derived(resourceStore.stale);

	onMount(() => resourceStore.subscribe());

	function refresh() {
		resourceStore.retry();
	}

	function bytes(value: number): string {
		const unit = value >= 1024 ** 3 ? 'GiB' : 'MiB';
		return `${(value / (unit === 'GiB' ? 1024 ** 3 : 1024 ** 2)).toLocaleString(undefined, { maximumFractionDigits: 1 })} ${unit}`;
	}
	function percent(value: number | null | undefined): string {
		return value == null ? (loading ? 'Loading…' : 'Unavailable') : `${value.toFixed(0)}%`;
	}
</script>

{#snippet meter(label: string, value: number | null | undefined)}
	{#if value != null}
		<div class="meter" class:high={value >= 90} role="meter" aria-label={label} aria-valuenow={value} aria-valuemin="0" aria-valuemax="100" aria-valuetext={`${value.toFixed(0)}% used`}>
			<span style:width={`${Math.max(0, Math.min(100, value))}%`}></span>
		</div>
	{/if}
{/snippet}

{#snippet capacityDetails(capacity: ResourceCapacity | null | undefined)}
	{#if capacity}
		<p class="note">{bytes(capacity.used_bytes)} / {bytes(capacity.total_bytes)} used</p>
		<p class="note">{bytes(capacity.available_bytes)} available</p>
	{/if}
{/snippet}

<section aria-label="Live summary" data-resource-summary>
	<div class="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
		<span title="The machine running XpressClaw. A container or VM installation reports the resources visible to that environment.">Server resources · live · all Projects</span>
		<span role="status" data-resource-status class:text-amber-600={stale}>
			{#if stale} {resources ? `Readings stale · last sample ${timeAgo(resources.sampled_at)}` : 'Resource monitoring unavailable'}
			{:else} Refreshes every 5 seconds {/if}
		</span>
		{#if error}<button type="button" onclick={refresh} class="underline underline-offset-2">Retry resources</button>{/if}
	</div>
	<div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
		<div class="resource-card" data-kpi="cpu">
			<h2>CPU</h2>
			<div class="value">{resources && resources.cpu_count > 0 && resources.cpu_percent == null ? 'Sampling…' : percent(resources?.cpu_percent)}</div>
			{@render meter('CPU usage', resources?.cpu_percent)}
			<p class="note">{resources?.cpu_count ? `${resources.cpu_count} logical CPUs · overall load` : 'Server CPU load'}</p>
		</div>
		<div class="resource-card" data-kpi="memory">
			<h2>Memory</h2>
			<div class="value">{percent(resources?.memory?.used_percent)}</div>
			{@render meter('Memory usage', resources?.memory?.used_percent)}
			{@render capacityDetails(resources?.memory)}
		</div>
		<div class="resource-card" data-kpi="disk">
			<h2>Disk</h2>
			{#if resources?.disks?.length}
				{#each resources.disks as disk}
					<div class="disk-volume" title={disk.mount_point ?? 'Containing filesystem unavailable'}>
						<div class="value">{percent(disk.capacity?.used_percent)}</div>
						{@render meter(`${disk.locations.join(' and ')} disk usage`, disk.capacity?.used_percent)}
						<p class="note">{disk.locations.join(' + ')}</p>
						{@render capacityDetails(disk.capacity)}
					</div>
				{/each}
			{:else}<div class="value">{loading ? 'Loading…' : 'Unavailable'}</div>{/if}
		</div>
		<div class="resource-card" class:attention={needsAttention > 0} data-kpi="needs-attention">
			<h2>Needs you</h2>
			<div class="value">{needsAttention}</div>
			<p class="note">Waiting or blocked</p>
			<p class="note">Selected Project scope</p>
		</div>
	</div>
</section>

<style>
	.resource-card { min-width: 0; border: 1px solid hsl(var(--border)); border-radius: .875rem; background: hsl(var(--card) / .94); padding: .9rem 1rem; box-shadow: 0 1px 2px rgb(0 0 0 / .04); }
	h2 { font-size: .75rem; font-weight: 600; color: hsl(var(--muted-foreground)); }
	.value { margin: .35rem 0; font-size: clamp(1.15rem, 2.5vw, 1.75rem); font-weight: 650; letter-spacing: -.035em; font-variant-numeric: tabular-nums; }
	.note { margin-top: .2rem; font-size: .65rem; color: hsl(var(--muted-foreground)); overflow-wrap: anywhere; }
	.meter { height: .3rem; overflow: hidden; margin: .4rem 0; background: hsl(var(--muted)); border-radius: 1rem; }
	.meter span { display: block; height: 100%; background: hsl(var(--primary)); }
	.meter.high span { background: hsl(var(--warning)); }
	.attention { border-color: hsl(var(--warning) / .3); background: linear-gradient(135deg, hsl(var(--card)), hsl(var(--warning-tint))); }
	.disk-volume + .disk-volume { margin-top: .6rem; border-top: 1px solid hsl(var(--border)); padding-top: .3rem; }
</style>
