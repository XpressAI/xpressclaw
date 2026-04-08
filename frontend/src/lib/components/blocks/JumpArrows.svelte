<script lang="ts">
	import { onMount, tick } from 'svelte';

	/** Each arrow: from element → to element, with a color and optional label */
	interface Arrow {
		fromId: string;
		toId: string;
		color: string;
		label?: string;
		side: 'right' | 'left';
	}

	let {
		arrows = [],
		containerEl = null as HTMLElement | null
	}: {
		arrows?: Arrow[];
		containerEl?: HTMLElement | null;
	} = $props();

	let paths = $state<{ d: string; color: string; label?: string; labelX: number; labelY: number }[]>([]);

	function computePaths() {
		if (!containerEl || arrows.length === 0) { paths = []; return; }
		const containerRect = containerEl.getBoundingClientRect();
		const scrollTop = containerEl.scrollTop;
		const result: typeof paths = [];

		for (const arrow of arrows) {
			const fromEl = containerEl.querySelector(`[data-step-id="${arrow.fromId}"]`);
			const toEl = containerEl.querySelector(`[data-step-id="${arrow.toId}"]`);
			if (!fromEl || !toEl) continue;

			const fromRect = fromEl.getBoundingClientRect();
			const toRect = toEl.getBoundingClientRect();

			// Positions relative to container
			const fromY = fromRect.top - containerRect.top + scrollTop + fromRect.height / 2;
			const toY = toRect.top - containerRect.top + scrollTop + toRect.height / 2;

			const side = arrow.side;
			const margin = side === 'right' ? 40 : -40;
			const fromX = side === 'right'
				? fromRect.right - containerRect.left + 8
				: fromRect.left - containerRect.left - 8;
			const toX = side === 'right'
				? toRect.right - containerRect.left + 8
				: toRect.left - containerRect.left - 8;

			// Curve control point — offset to the side
			const cpX = side === 'right'
				? Math.max(fromX, toX) + margin
				: Math.min(fromX, toX) + margin;

			const d = `M ${fromX} ${fromY} C ${cpX} ${fromY}, ${cpX} ${toY}, ${toX} ${toY}`;

			// Arrow head at end — points inward toward the target block (left)
			const headSize = 6;
			const headInward = side === 'right' ? 1 : -1;
			const headD = `M ${toX} ${toY} l ${headInward * headSize} ${-headSize} M ${toX} ${toY} l ${headInward * headSize} ${headSize}`;

			result.push({
				d: d + ' ' + headD,
				color: arrow.color,
				label: arrow.label,
				labelX: cpX + (side === 'right' ? 4 : -4),
				labelY: (fromY + toY) / 2
			});
		}

		paths = result;
	}

	$effect(() => {
		// Re-compute when arrows change
		arrows;
		tick().then(computePaths);
	});

	onMount(() => {
		computePaths();
		// Recompute on scroll/resize
		const observer = new ResizeObserver(computePaths);
		if (containerEl) {
			observer.observe(containerEl);
			containerEl.addEventListener('scroll', computePaths);
		}
		return () => {
			observer.disconnect();
			containerEl?.removeEventListener('scroll', computePaths);
		};
	});
</script>

{#if paths.length > 0}
	<svg class="absolute inset-0 pointer-events-none z-10 overflow-visible" style="width: 100%; height: 100%;">
		{#each paths as path}
			<path d={path.d} fill="none" stroke={path.color} stroke-width="1.5"
				stroke-dasharray="4 3" opacity="0.6" />
			{#if path.label}
				<text x={path.labelX} y={path.labelY} fill={path.color} font-size="9"
					text-anchor="middle" dominant-baseline="middle" opacity="0.7"
					font-family="monospace">{path.label}</text>
			{/if}
		{/each}
	</svg>
{/if}
