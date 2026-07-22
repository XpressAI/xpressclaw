<script lang="ts">
	import { onMount } from 'svelte';
	import type { ContextMenuItem } from '$lib/contextMenu';

	let {
		x,
		y,
		label = 'Context menu',
		items,
		onselect,
		onclose,
	}: {
		x: number;
		y: number;
		label?: string;
		items: ContextMenuItem[];
		onselect: (id: string) => void;
		onclose: () => void;
	} = $props();

	let menu = $state<HTMLDivElement>();
	let left = $state<number | null>(null);
	let top = $state<number | null>(null);

	function enabledItems(): HTMLButtonElement[] {
		return Array.from(menu?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? []);
	}

	function placeAndFocus() {
		if (!menu) return;
		const bounds = menu.getBoundingClientRect();
		left = Math.max(8, Math.min(x, window.innerWidth - bounds.width - 8));
		top = Math.max(8, Math.min(y, window.innerHeight - bounds.height - 8));
		enabledItems()[0]?.focus({ preventScroll: true });
	}

	function handleKeydown(event: KeyboardEvent) {
		const buttons = enabledItems();
		const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
		let next = current;

		if (event.key === 'ArrowDown') next = (current + 1) % buttons.length;
		else if (event.key === 'ArrowUp') next = (current - 1 + buttons.length) % buttons.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = buttons.length - 1;
		else if (event.key === 'Escape' || event.key === 'Tab') {
			event.preventDefault();
			onclose();
			return;
		} else {
			return;
		}

		event.preventDefault();
		buttons[next]?.focus({ preventScroll: true });
	}

	function choose(item: ContextMenuItem) {
		if (item.disabled) return;
		onselect(item.id);
		onclose();
	}

	onMount(() => {
		const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		const closeOutside = (event: PointerEvent) => {
			if (menu && !event.composedPath().includes(menu)) onclose();
		};
		const close = () => onclose();

		document.addEventListener('pointerdown', closeOutside);
		window.addEventListener('resize', close);
		placeAndFocus();

		return () => {
			document.removeEventListener('pointerdown', closeOutside);
			window.removeEventListener('resize', close);
			if (previousFocus?.isConnected) previousFocus.focus();
		};
	});
</script>

<div
	bind:this={menu}
	role="menu"
	aria-label={label}
	tabindex="-1"
	onkeydown={handleKeydown}
	class="fixed z-[300] min-w-52 overflow-hidden rounded-lg border border-border bg-card p-1 text-foreground shadow-2xl {left === null || top === null ? 'invisible' : ''}"
	style:left="{left ?? x}px"
	style:top="{top ?? y}px"
>
	{#each items as item (item.id)}
		{#if item.separatorBefore}<div role="separator" class="my-1 border-t border-border"></div>{/if}
		<button
			type="button"
			role="menuitem"
			disabled={item.disabled}
			onclick={() => choose(item)}
			class="flex w-full items-center rounded-md px-3 py-1.5 text-left text-xs outline-none hover:bg-accent focus:bg-accent disabled:pointer-events-none disabled:opacity-40"
		>
			{item.label}
		</button>
	{/each}
</div>
