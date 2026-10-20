/** Fit the mobile workspace to the visible area above the software keyboard. */
export function workspaceViewport(node: HTMLElement) {
	const viewport = window.visualViewport;
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
	};
	const scheduleUpdate = () => {
		if (!frame) frame = window.requestAnimationFrame(update);
	};
	update();
	viewport?.addEventListener('resize', scheduleUpdate);
	viewport?.addEventListener('scroll', scheduleUpdate);
	window.addEventListener('resize', scheduleUpdate);
	return {
		destroy() {
			window.cancelAnimationFrame(frame);
			viewport?.removeEventListener('resize', scheduleUpdate);
			viewport?.removeEventListener('scroll', scheduleUpdate);
			window.removeEventListener('resize', scheduleUpdate);
		},
	};
}
