<script lang="ts">
	import { onMount } from 'svelte';
	import { openExternal } from '$lib/utils';

	let error = $state('');

	function handleLinkClick(event: MouseEvent) {
		if (event.defaultPrevented || event.button !== (event.type === 'auxclick' ? 1 : 0)) return;
		const link = event.composedPath().find((element): element is HTMLAnchorElement => element instanceof HTMLAnchorElement);
		if (!link?.href || link.hasAttribute('download')) return;

		let url: URL;
		try {
			url = new URL(link.href);
		} catch {
			return;
		}
		if (!['http:', 'https:'].includes(url.protocol) || url.origin === window.location.origin) return;

		event.preventDefault();
		error = '';
		void openExternal(url.href).catch((cause) => {
			error = `Could not open link: ${cause instanceof Error ? cause.message : String(cause)}`;
		});
	}

	onMount(() => {
		// Run after component handlers, but before the opener plugin's window
		// listener, so Desktop links use the identity-checked native command.
		document.addEventListener('click', handleLinkClick);
		document.addEventListener('auxclick', handleLinkClick);
		return () => {
			document.removeEventListener('click', handleLinkClick);
			document.removeEventListener('auxclick', handleLinkClick);
		};
	});
</script>

{#if error}
	<div role="alert" class="fixed bottom-4 left-1/2 z-[100] flex w-[calc(100%-2rem)] max-w-lg -translate-x-1/2 items-center gap-3 rounded-lg border border-destructive/30 bg-card p-3 text-sm text-destructive shadow-lg">
		<span class="min-w-0 flex-1 break-words">{error}</span>
		<button type="button" onclick={() => error = ''} aria-label="Dismiss link error" class="shrink-0 rounded px-2 py-1 hover:bg-accent">Dismiss</button>
	</div>
{/if}
