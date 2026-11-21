<script lang="ts">
	export interface JumpArrow {
		fromId: string;
		toId: string;
		armIndex?: number;
		color: string;
		label: string;
	}

	let { arrows = [], containerEl = null, ongutter = () => {} }: {
		arrows?: JumpArrow[];
		containerEl?: HTMLElement | null;
		ongutter?: (width: number) => void;
	} = $props();

	interface RoutedArrow extends JumpArrow {
		d: string;
		fromX: number;
		fromY: number;
		toX: number;
		toY: number;
		lane: number;
	}
	let paths = $state<RoutedArrow[]>([]);
	let svgEl: SVGSVGElement;

	$effect(() => {
		const container = containerEl;
		const connections = arrows;
		if (!container) return;
		let frame = 0;
		let gutter = -1;
		const observed = new Set<Element>();
		const schedule = () => {
			if (!frame) frame = requestAnimationFrame(measure);
		};
		const resizeObserver = new ResizeObserver(schedule);

		function measure() {
			frame = 0;
			const blocks = Array.from(container!.querySelectorAll<HTMLElement>('[data-step-id]'));
			const elements = new Set<Element>([container!, ...blocks]);
			for (const element of observed) {
				if (!elements.has(element)) { resizeObserver.unobserve(element); observed.delete(element); }
			}
			for (const element of elements) {
				if (!observed.has(element)) { resizeObserver.observe(element); observed.add(element); }
			}
			const bounds = container!.getBoundingClientRect();
			if (!bounds.width || !bounds.height) return;
			const scaleX = container!.clientWidth / bounds.width;
			const scaleY = container!.clientHeight / bounds.height;
			const byId = new Map(blocks.map(block => [block.dataset.stepId, block]));
			const right = Math.max(0, ...blocks.map(block => (block.getBoundingClientRect().right - bounds.left) * scaleX));
			const measured = connections.flatMap(arrow => {
				const source = byId.get(arrow.fromId);
				const target = byId.get(arrow.toId);
				if (!source || !target) return [];
				const from = source.getBoundingClientRect();
				const to = target.getBoundingClientRect();
				const arm = arrow.armIndex === undefined ? null : source.querySelector<HTMLElement>(`[data-jump-arm="${arrow.armIndex}"]`);
				const port = arm?.getBoundingClientRect();
				let fromY = port ? (port.top + port.height / 2 - bounds.top) * scaleY : (from.top - bounds.top) * scaleY + 18;
				let toY = (to.top - bounds.top) * scaleY + 18;
				if (Math.abs(fromY - toY) < 12) { fromY += 8; toY -= 8; }
				return [{ ...arrow, fromX: (from.right - bounds.left) * scaleX + 2, fromY, toX: (to.right - bounds.left) * scaleX + 2, toY }];
			});

			// Keep incoming arrowheads distinct when several branches share a step.
			const targets = new Map<string, typeof measured>();
			for (const arrow of measured) {
				const group = targets.get(arrow.toId) ?? [];
				group.push(arrow);
				targets.set(arrow.toId, group);
			}
			for (const group of targets.values()) {
				group.sort((a, b) => a.fromY - b.fromY);
				const spacing = Math.min(6, 24 / group.length);
				group.forEach((arrow, index) => arrow.toY += (index - (group.length - 1) / 2) * spacing);
			}

			// Reuse a lane only after its earlier connection has ended.
			measured.sort((a, b) => Math.min(a.fromY, a.toY) - Math.min(b.fromY, b.toY) || Math.max(a.fromY, a.toY) - Math.max(b.fromY, b.toY));
			const ends: number[] = [];
			paths = measured.map(arrow => {
				const start = Math.min(arrow.fromY, arrow.toY) - 12;
				let lane = ends.findIndex(end => end < start);
				if (lane < 0) lane = ends.length;
				ends[lane] = Math.max(arrow.fromY, arrow.toY) + 12;
				const x = right + 24 + lane * 20;
				const direction = arrow.toY > arrow.fromY ? 1 : -1;
				const radius = Math.min(8, Math.abs(arrow.toY - arrow.fromY) / 2);
				const d = `M ${arrow.fromX} ${arrow.fromY} H ${x - radius} Q ${x} ${arrow.fromY} ${x} ${arrow.fromY + direction * radius} V ${arrow.toY - direction * radius} Q ${x} ${arrow.toY} ${x - radius} ${arrow.toY} H ${arrow.toX + 7}`;
				return { ...arrow, lane, d };
			});
			const width = ends.length ? 48 + (ends.length - 1) * 20 : 0;
			if (width !== gutter) { gutter = width; ongutter(width); }
		}

		// Observe the cards, not SVG mutations produced by measuring them.
		const mutationObserver = new MutationObserver(records => {
			if (records.some(record => !svgEl?.contains(record.target))) schedule();
		});
		mutationObserver.observe(container, { childList: true, subtree: true, attributes: true, characterData: true });
		resizeObserver.observe(container);
		observed.add(container);
		schedule();
		return () => {
			cancelAnimationFrame(frame);
			resizeObserver.disconnect();
			mutationObserver.disconnect();
		};
	});
</script>

<svg bind:this={svgEl} data-workflow-arrows aria-hidden="true" class="pointer-events-none absolute inset-0 z-10 h-full w-full overflow-visible">
	{#each paths as path}
		<g data-jump-connection data-from-step={path.fromId} data-to-step={path.toId} data-lane={path.lane}>
			<path d={path.d} fill="none" stroke={path.color} stroke-width="2" stroke-linecap="round" class="connection">
				<title>{path.label}</title>
			</path>
			<circle cx={path.fromX} cy={path.fromY} r="3" fill={path.color} />
			<path d="M {path.toX} {path.toY} l 7 -4 v 8 Z" fill={path.color} />
		</g>
	{/each}
</svg>

<style>
	.connection { pointer-events: stroke; opacity: 0.75; }
	.connection:hover { stroke-width: 3; opacity: 1; }
</style>
