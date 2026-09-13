<script lang="ts">
	import { createDroppable } from '@dnd-kit/svelte';
	import { CollisionPriority } from '@dnd-kit/abstract';
	import { pointerIntersection } from '@dnd-kit/collision';
	import type { Snippet } from 'svelte';

	// The strip itself accepts tabs so the empty run past the last tab appends,
	// which matters once "Split active tab right" leaves a pane with a short tab
	// list and a wide strip. It sits at the lowest collision priority so a tab
	// under the pointer always wins over the strip behind it.
	let {
		paneId,
		element = $bindable(),
		children,
		...rest
	}: {
		paneId: string;
		element?: HTMLDivElement;
		children: Snippet;
		[key: string]: unknown;
	} = $props();

	const dropZone = createDroppable({
		get id() {
			return `tab-strip:${paneId}`;
		},
		get data() {
			return { paneId };
		},
		type: 'workspace-tab-strip',
		accept: 'workspace-tab',
		collisionPriority: CollisionPriority.Lowest,
		collisionDetector: pointerIntersection,
	});
</script>

<div bind:this={element} {@attach dropZone.attach} {...rest}>
	{@render children()}
</div>
