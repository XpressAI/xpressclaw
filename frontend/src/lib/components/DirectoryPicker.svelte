<script lang="ts">
	import { onMount } from 'svelte';
	import { setup } from '$lib/api';
	import type { DirectoryListing } from '$lib/api';

	let {
		initialPath = '',
		title = 'Choose a folder',
		onselect,
		onclose
	}: {
		initialPath?: string;
		title?: string;
		onselect: (path: string) => void;
		onclose: () => void;
	} = $props();

	let listing = $state<DirectoryListing | null>(null);
	let pathInput = $state('');
	let loading = $state(true);
	let error = $state('');

	onMount(() => {
		pathInput = initialPath;
		load(initialPath);
	});

	async function load(path?: string) {
		loading = true;
		error = '';
		try {
			listing = await setup.directories(path?.trim() || undefined);
			pathInput = listing.path;
		} catch (reason) {
			error = reason instanceof Error ? reason.message : 'Could not open this folder';
		} finally {
			loading = false;
		}
	}

	function choose() {
		if (listing) onselect(listing.path);
	}
</script>

<svelte:window onkeydown={(event) => { if (event.key === 'Escape') onclose(); }} />

<div
	class="fixed inset-0 z-[220] flex items-end justify-center bg-black/60 p-0 backdrop-blur-sm sm:items-center sm:p-4"
	role="presentation"
	onclick={(event) => { if (event.target === event.currentTarget) onclose(); }}
>
	<div
		class="flex max-h-[88vh] w-full max-w-2xl flex-col rounded-t-2xl border border-border bg-card shadow-2xl sm:max-h-[75vh] sm:rounded-2xl"
		role="dialog"
		aria-modal="true"
		aria-label={title}
		tabindex="-1"
	>
		<div class="flex shrink-0 items-center justify-between border-b border-border px-4 py-3">
			<div>
				<h2 class="text-sm font-semibold text-foreground">{title}</h2>
				<p class="mt-0.5 text-xs text-muted-foreground">Folders on the machine running XpressClaw</p>
			</div>
			<button type="button" onclick={onclose} class="rounded-md p-2 text-lg leading-none text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Close">&times;</button>
		</div>

		<form class="flex shrink-0 gap-2 border-b border-border p-3" onsubmit={(event) => { event.preventDefault(); load(pathInput); }}>
			<input
				bind:value={pathInput}
				aria-label="Folder path"
				class="min-w-0 flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<button type="submit" disabled={loading || !pathInput.trim()} class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent disabled:opacity-50">Go</button>
		</form>

		<div class="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
			<button type="button" onclick={() => listing?.parent && load(listing.parent)} disabled={!listing?.parent || loading} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent disabled:opacity-40">Up</button>
			{#if listing?.home}
				<button type="button" onclick={() => load(listing?.home ?? undefined)} disabled={loading} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent disabled:opacity-40">Home</button>
			{/if}
			{#each listing?.roots ?? [] as root}
				<button type="button" onclick={() => load(root)} disabled={loading || root === listing?.path} class="rounded-md border border-border px-2.5 py-1.5 font-mono text-xs hover:bg-accent disabled:opacity-40">{root}</button>
			{/each}
		</div>

		<div class="min-h-40 flex-1 overflow-y-auto p-2">
			{#if loading}
				<div class="flex h-32 items-center justify-center gap-2 text-xs text-muted-foreground">
					<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
					Opening folder...
				</div>
			{:else if error}
				<div class="m-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-3 text-sm text-destructive">{error}</div>
			{:else if listing?.directories.length}
				<div class="grid gap-1 sm:grid-cols-2">
					{#each listing.directories as directory}
						<button type="button" onclick={() => load(directory.path)} class="flex min-w-0 items-center gap-2 rounded-lg px-3 py-2.5 text-left hover:bg-accent">
							<span aria-hidden="true" class="text-amber-500">▰</span>
							<span class="truncate text-sm text-foreground">{directory.name}</span>
						</button>
					{/each}
				</div>
			{:else}
				<p class="px-3 py-8 text-center text-xs text-muted-foreground">This folder has no subfolders.</p>
			{/if}
		</div>

		<div class="flex shrink-0 items-center justify-between gap-3 border-t border-border px-4 py-3">
			<p class="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{listing?.path ?? pathInput}</p>
			<div class="flex shrink-0 gap-2">
				<button type="button" onclick={onclose} class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent">Cancel</button>
				<button type="button" onclick={choose} disabled={!listing || loading} class="rounded-md bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">Choose folder</button>
			</div>
		</div>
	</div>
</div>
