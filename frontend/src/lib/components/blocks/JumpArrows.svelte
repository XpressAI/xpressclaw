<script lang="ts">
	import { onMount, tick } from 'svelte';

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
	let svgHeight = $state(0);
	let svgWidth = $state(0);

	function computePaths() {
		if (!containerEl || arrows.length === 0) { paths = []; return; }

		const containerRect = containerEl.getBoundingClientRect();
		const scrollTop = containerEl.scrollTop;
		svgHeight = containerEl.scrollHeight;
		svgWidth = containerEl.scrollWidth;

		const result: typeof paths = [];
		const R = 8; // corner radius

		for (const arrow of arrows) {
			const fromEl = containerEl.querySelector(`[data-step-id="${arrow.fromId}"]`);
			const toEl = containerEl.querySelector(`[data-step-id="${arrow.toId}"]`);
			if (!fromEl || !toEl) continue;

			const fromRect = fromEl.getBoundingClientRect();
			const toRect = toEl.getBoundingClientRect();

			// Y positions relative to scroll container
			const fromY = fromRect.top - containerRect.top + scrollTop + fromRect.height / 2;
			const toY = toRect.top - containerRect.top + scrollTop + toRect.height / 2;

			// X: right edge of the blocks + offset
			const rightEdge = Math.max(fromRect.right, toRect.right) - containerRect.left + 16;
			const offset = 28; // how far right the vertical line goes
			const xRight = rightEdge + offset;

			// Arrowhead tip goes to the right edge of the target block
			const toX = toRect.right - containerRect.left + 4;
			const fromX = fromRect.right - containerRect.left + 4;

			// Draw a rounded rectangle path: right from source → down/up → left to target
			// Like a bracket on the right side: ┐ │ └→
			const goingDown = toY > fromY;

			let d: string;
			if (goingDown) {
				d = [
					`M ${fromX} ${fromY}`,              // start at source right edge
					`L ${xRight - R} ${fromY}`,         // horizontal to near corner
					`Q ${xRight} ${fromY} ${xRight} ${fromY + R}`,  // round top-right corner
					`L ${xRight} ${toY - R}`,           // vertical down
					`Q ${xRight} ${toY} ${xRight - R} ${toY}`,      // round bottom-right corner
					`L ${toX + 6} ${toY}`,              // horizontal to arrowhead
					// Arrowhead
					`M ${toX + 6} ${toY} L ${toX + 12} ${toY - 4}`,
					`M ${toX + 6} ${toY} L ${toX + 12} ${toY + 4}`,
				].join(' ');
			} else {
				d = [
					`M ${fromX} ${fromY}`,
					`L ${xRight - R} ${fromY}`,
					`Q ${xRight} ${fromY} ${xRight} ${fromY - R}`,
					`L ${xRight} ${toY + R}`,
					`Q ${xRight} ${toY} ${xRight - R} ${toY}`,
					`L ${toX + 6} ${toY}`,
					`M ${toX + 6} ${toY} L ${toX + 12} ${toY - 4}`,
					`M ${toX + 6} ${toY} L ${toX + 12} ${toY + 4}`,
				].join(' ');
			}

			result.push({
				d,
				color: arrow.color,
				label: arrow.label,
				labelX: xRight + 6,
				labelY: (fromY + toY) / 2,
			});
		}

		paths = result;
	}

	// Recompute on any change
	$effect(() => {
		arrows;
		tick().then(computePaths);
	});

	onMount(() => {
		computePaths();

		// Recompute on resize, scroll, and DOM mutations (compact toggle)
		const resizeObs = new ResizeObserver(computePaths);
		const mutationObs = new MutationObserver(() => { requestAnimationFrame(computePaths); });

		if (containerEl) {
			resizeObs.observe(containerEl);
			containerEl.addEventListener('scroll', computePaths);
			mutationObs.observe(containerEl, { childList: true, subtree: true, attributes: true });
		}

		// Also recompute on window resize
		window.addEventListener('resize', computePaths);

		return () => {
			resizeObs.disconnect();
			mutationObs.disconnect();
			containerEl?.removeEventListener('scroll', computePaths);
			window.removeEventListener('resize', computePaths);
		};
	});
</script>

{#if paths.length > 0}
	<svg class="absolute inset-0 pointer-events-none z-10" style="width: {svgWidth}px; height: {svgHeight}px; overflow: visible;">
		{#each paths as path}
			<path d={path.d} fill="none" stroke={path.color} stroke-width="1.5"
				stroke-dasharray="4 3" opacity="0.5" />
			{#if path.label}
				<text x={path.labelX} y={path.labelY} fill={path.color} font-size="9"
					dominant-baseline="middle" opacity="0.6"
					font-family="monospace">{path.label}</text>
			{/if}
		{/each}
	</svg>
{/if}
