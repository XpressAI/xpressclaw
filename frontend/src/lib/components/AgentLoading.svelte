<script lang="ts">
	import { onMount } from 'svelte';

	let {
		label = 'Working',
		startedAt = null,
		compact = false,
	}: {
		label?: string;
		startedAt?: string | number | null;
		compact?: boolean;
	} = $props();

	let now = $state(Date.now());
	const fallbackStart = Date.now();
	let elapsed = $derived(Math.max(0, now - startTime(startedAt)));
	const pixels = Array.from({ length: 9 });

	onMount(() => {
		const timer = window.setInterval(() => (now = Date.now()), 100);
		return () => window.clearInterval(timer);
	});

	function startTime(value: string | number | null): number {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string') {
			const parsed = Date.parse(value);
			if (!Number.isNaN(parsed)) return parsed;
		}
		return fallbackStart;
	}

	function elapsedLabel(milliseconds: number): string {
		const seconds = milliseconds / 1_000;
		if (seconds < 60) return `${seconds.toFixed(1)}s`;
		const minutes = Math.floor(seconds / 60);
		return `${minutes}m ${Math.floor(seconds % 60)}s`;
	}
</script>

<div
	data-agent-loading
	class="flex w-fit items-center {compact ? 'gap-2' : 'gap-2.5'} text-xs"
	role="status"
	aria-label={label}
>
	<span aria-hidden="true" class="grid grid-cols-[repeat(3,4px)] gap-[1.5px]">
		{#each pixels as _, index}
			<span
				class="h-1 w-1 rounded-[1px] bg-current text-foreground"
				style:animation={`ai-pixel-on 650ms ease-in-out ${(index % 3 + Math.floor(index / 3)) * 90}ms infinite`}
			></span>
		{/each}
	</span>
	<span class="ai-shimmer-text font-medium">{label}</span>
	<span class="font-mono text-[11px] tabular-nums text-muted-foreground" aria-hidden="true">{elapsedLabel(elapsed)}</span>
</div>
