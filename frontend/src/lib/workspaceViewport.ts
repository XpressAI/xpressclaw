import type { ViewportSegment } from './workspaceLayout';

export interface WorkspaceViewport {
	width: number;
	segments: ViewportSegment[];
}

type FoldableWindow = Window & { viewport?: { segments: readonly DOMRectReadOnly[] | null } };
type FoldableNavigator = Navigator & { devicePosture?: EventTarget };

/** Fit the workspace to the visible area and report optional fold geometry. */
export function workspaceViewport(node: HTMLElement, onchange?: (viewport: WorkspaceViewport) => void) {
	const viewport = window.visualViewport;
	const posture = (navigator as FoldableNavigator).devicePosture;
	const segmentQueries = [
		window.matchMedia('(horizontal-viewport-segments: 2)'),
		window.matchMedia('(vertical-viewport-segments: 2)'),
	];
	let frame = 0;
	const update = () => {
		frame = 0;
		// Pinch zoom must magnify the existing layout, not reflow it.
		if (viewport && Math.abs(viewport.scale - 1) > 0.01) return;
		const height = viewport?.height ?? window.innerHeight;
		node.style.setProperty('--workspace-viewport-height', `${height}px`);
		node.style.setProperty('--workspace-viewport-top', `${viewport?.offsetTop ?? 0}px`);
		// Browser chrome can also shrink the viewport slightly. Reserve the
		// navigation space unless a keyboard-sized part of the screen is covered.
		node.toggleAttribute('data-keyboard-open', window.innerHeight - height > 120);
		const segments = ((window as FoldableWindow).viewport?.segments ?? [])
			.filter(({ x, y, width, height }) => [x, y, width, height].every(Number.isFinite) && width > 0 && height > 0)
			.map(({ x, y, width, height }) => ({ x, y, width, height }));
		onchange?.({ width: window.innerWidth, segments });
	};
	const scheduleUpdate = () => {
		if (!frame) frame = window.requestAnimationFrame(update);
	};
	update();
	viewport?.addEventListener('resize', scheduleUpdate);
	viewport?.addEventListener('scroll', scheduleUpdate);
	window.addEventListener('resize', scheduleUpdate);
	posture?.addEventListener('change', scheduleUpdate);
	for (const query of segmentQueries) query.addEventListener('change', scheduleUpdate);
	return {
		destroy() {
			window.cancelAnimationFrame(frame);
			viewport?.removeEventListener('resize', scheduleUpdate);
			viewport?.removeEventListener('scroll', scheduleUpdate);
			window.removeEventListener('resize', scheduleUpdate);
			posture?.removeEventListener('change', scheduleUpdate);
			for (const query of segmentQueries) query.removeEventListener('change', scheduleUpdate);
		},
	};
}
