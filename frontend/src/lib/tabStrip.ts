/** Reveal a tab without scrolling the workspace or the browser's keyboard viewport. */
export function revealActiveTab(strip: HTMLElement): void {
	if (!strip.clientWidth) return;
	const tab = strip.querySelector<HTMLElement>('[data-workspace-tab-active="true"]');
	if (!tab) return;
	const stripBounds = strip.getBoundingClientRect();
	const tabBounds = tab.getBoundingClientRect();
	const left = stripBounds.left + strip.clientLeft;
	const right = left + strip.clientWidth;
	if (tabBounds.left < left) strip.scrollLeft += tabBounds.left - left;
	else if (tabBounds.right > right) strip.scrollLeft += tabBounds.right - right;
}

/** A breakpoint or keyboard resize can reveal a previously hidden tab strip. */
export function revealActiveTabOnResize(strip: HTMLElement) {
	const observer = new ResizeObserver(() => revealActiveTab(strip));
	observer.observe(strip);
	return { destroy: () => observer.disconnect() };
}
