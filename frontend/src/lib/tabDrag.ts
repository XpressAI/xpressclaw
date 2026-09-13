import { PointerSensor, KeyboardSensor } from '@dnd-kit/svelte';
import { PointerActivationConstraints } from '@dnd-kit/dom';

/**
 * Pointer travel, in pixels, before a press on a tab becomes a drag.
 *
 * Native HTML5 drag-and-drop gave us no say here: the spec leaves the
 * threshold platform-dependent, and WebKit starts a drag from a plain press on
 * a text node. That swallowed the click that follows, so activating or closing
 * a tab failed on macOS. Owning the gesture lets a press stay a click until the
 * pointer actually travels.
 */
export const TAB_DRAG_ACTIVATION_DISTANCE = 6;

/** Controls inside a tab that must keep their click rather than start a drag. */
const NO_DRAG_SELECTOR = '[data-no-drag]';

export const tabDragSensors = [
	PointerSensor.configure({
		activationConstraints: [
			new PointerActivationConstraints.Distance({ value: TAB_DRAG_ACTIVATION_DISTANCE }),
		],
		preventActivation: (event: PointerEvent) =>
			event.target instanceof Element && event.target.closest(NO_DRAG_SELECTOR) !== null,
	}),
	KeyboardSensor.configure({}),
];
