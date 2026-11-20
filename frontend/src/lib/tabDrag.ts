import { PointerSensor, KeyboardSensor } from '@dnd-kit/svelte';
import { PointerActivationConstraints } from '@dnd-kit/dom';

/**
 * Pointer travel, in pixels, before a mouse press on a tab becomes a drag.
 *
 * Native HTML5 drag-and-drop gave us no say here: the spec leaves the
 * threshold platform-dependent, and a press on a text node could start a drag
 * before the click that should have followed. Owning the gesture lets a press
 * stay a click until the pointer actually travels.
 */
export const TAB_DRAG_ACTIVATION_DISTANCE = 6;

/**
 * How long a touch has to hold still before it becomes a drag, and how far it
 * may wander meanwhile.
 *
 * A finger has to be able to scroll the strip, and a browser only starts a
 * native pan after several pixels of travel while still delivering pointer
 * moves for the first few. A distance threshold inside that window claims the
 * gesture as a drag and blocks the pan, which is what made the strip
 * unscrollable on phones. A hold-to-drag lets a flick abort the drag and
 * scroll, while a deliberate press-and-hold still picks the tab up.
 */
export const TAB_TOUCH_HOLD_MS = 250;
export const TAB_TOUCH_HOLD_TOLERANCE = 5;

/** Controls inside a tab that must keep their click rather than start a drag. */
const NO_DRAG_SELECTOR = '[data-no-drag]';

export const tabDragSensors = [
	PointerSensor.configure({
		activationConstraints: (event: PointerEvent) =>
			event.pointerType === 'touch'
				? [new PointerActivationConstraints.Delay({ value: TAB_TOUCH_HOLD_MS, tolerance: TAB_TOUCH_HOLD_TOLERANCE })]
				: [new PointerActivationConstraints.Distance({ value: TAB_DRAG_ACTIVATION_DISTANCE })],
		preventActivation: (event: PointerEvent) =>
			event.target instanceof Element && event.target.closest(NO_DRAG_SELECTOR) !== null,
	}),
	KeyboardSensor.configure({}),
];
