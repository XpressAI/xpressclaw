<script lang="ts">
	import { onMount } from 'svelte';
	import { elapsedTimeLabel, serverTimestampMs } from '$lib/serverTime';

	let {
		label = 'Working',
		startedAt = null,
		compact = false,
		phase = 'active',
	}: {
		label?: string;
		startedAt?: string | number | null;
		compact?: boolean;
		phase?: 'active' | 'preparing' | 'queued' | 'loading';
	} = $props();

	let now = $state(Date.now());
	let visible = $state(true);
	let parsedStart = $derived(serverTimestampMs(startedAt));
	let elapsed = $derived(parsedStart === null ? null : Math.max(0, now - parsedStart));
	const pixels = Array.from({ length: 9 });

	onMount(() => {
		const updateVisibility = () => (visible = !document.hidden);
		updateVisibility();
		document.addEventListener('visibilitychange', updateVisibility);
		return () => document.removeEventListener('visibilitychange', updateVisibility);
	});

	$effect(() => {
		if (!visible || parsedStart === null) return;
		now = Date.now();
		const timer = window.setInterval(() => (now = Date.now()), 250);
		return () => window.clearInterval(timer);
	});
</script>

<div
	data-agent-loading
	data-agent-phase={phase}
	class="flex w-fit items-center {compact ? 'gap-2' : 'gap-2.5'} text-xs"
>
	<span class="flex items-center {compact ? 'gap-2' : 'gap-2.5'}" role="status" aria-live="polite">
		{#if phase === 'queued'}
			<span aria-hidden="true" class="h-2 w-2 rounded-full bg-amber-400"></span>
		{:else}
			<span aria-hidden="true" class="grid grid-cols-[repeat(3,4px)] gap-[1.5px]">
				{#each pixels as _, index}
					<span
						class="h-1 w-1 rounded-[1px] bg-current {phase === 'loading' ? 'text-muted-foreground' : 'text-foreground'}"
						style:animation={phase === 'loading' ? undefined : `ai-pixel-on 650ms ease-in-out ${(index % 3 + Math.floor(index / 3)) * 90}ms infinite`}
					></span>
				{/each}
			</span>
		{/if}
		<span class={phase === 'queued' ? 'font-medium text-amber-500' : 'ai-shimmer-text font-medium'}>{label}</span>
	</span>
	{#if elapsed !== null}
		<span
			data-elapsed-time
			class="font-mono text-[11px] tabular-nums text-muted-foreground"
			aria-label={`Elapsed ${elapsedTimeLabel(elapsed)}`}
		>{elapsedTimeLabel(elapsed)}</span>
	{/if}
</div>
