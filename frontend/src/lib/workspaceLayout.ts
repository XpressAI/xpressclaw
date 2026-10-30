export const MIN_PANE_WIDTH = 380;
export const MIN_DETAILS_WIDTH = 668; // 380px conversation + 288px details.
export const PANE_DIVIDER_WIDTH = 4;

export interface ViewportSegment {
	x: number;
	y: number;
	width: number;
	height: number;
}

export interface WorkspaceLayout {
	x: number;
	y: number;
	width: number;
	capacity: number;
	regions: ViewportSegment[];
	axis: 'horizontal' | 'vertical';
}

/** Intersect fold segments with the space left by browser chrome and the keyboard. */
export function measureWorkspaceLayout(bounds: ViewportSegment, segments: ViewportSegment[]): WorkspaceLayout {
	const intersections = segments.length > 1 ? segments.flatMap((segment) => {
		const x = Math.max(bounds.x, segment.x);
		const y = Math.max(bounds.y, segment.y);
		const width = Math.min(bounds.x + bounds.width, segment.x + segment.width) - x;
		const height = Math.min(bounds.y + bounds.height, segment.y + segment.height) - y;
		return width > 0 && height > 0 ? [{ x: x - bounds.x, y: y - bounds.y, width, height }] : [];
	}).sort((a, b) => a.y - b.y || a.x - b.x) : [];
	let regions = intersections.filter(region => region.width >= 320 && region.height >= 240);
	// If the keyboard leaves only a small region, keep the focused pane inside
	// that region rather than spanning the fold again.
	if (!regions.length && intersections.length) {
		regions = [intersections.reduce((largest, region) => region.width * region.height > largest.width * largest.height ? region : largest)];
	}
	return {
		x: bounds.x,
		y: bounds.y,
		width: bounds.width,
		capacity: regions.length || Math.max(1, Math.floor((bounds.width + PANE_DIVIDER_WIDTH) / (MIN_PANE_WIDTH + PANE_DIVIDER_WIDTH))),
		regions,
		axis: regions.length > 1 && regions[1].y >= regions[0].y + regions[0].height ? 'vertical' : 'horizontal',
	};
}

/** Keep a focused pane visible without moving the window when focus stays inside it. */
export function visiblePaneStart(anchor: number, focusedIndex: number, count: number, capacity: number): number {
	const start = Math.min(anchor, Math.max(0, count - capacity));
	if (focusedIndex < start) return focusedIndex;
	if (focusedIndex >= start + capacity) return focusedIndex - capacity + 1;
	return start;
}

export function regionStyle(region: ViewportSegment): string {
	return `position:absolute;left:${region.x}px;top:${region.y}px;width:${region.width}px;height:${region.height}px`;
}

/** Size hints avoid containment changing the coordinate system of fixed menus. */
export function measurePane(node: HTMLElement) {
	const update = () => {
		const { width, height } = node.getBoundingClientRect();
		node.style.setProperty('--workspace-pane-height', `${height}px`);
		node.toggleAttribute('data-short-pane', height < 400);
		node.toggleAttribute('data-pane-wide-details', width >= MIN_DETAILS_WIDTH);
	};
	const observer = new ResizeObserver(update);
	observer.observe(node);
	update();
	return { destroy: () => observer.disconnect() };
}
