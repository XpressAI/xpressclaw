<script lang="ts">
	import { onMount } from 'svelte';
	import { memory } from '$lib/api';
	import type { MemorySearchResult } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	interface Props {
		agentId: string;
	}

	let { agentId }: Props = $props();

	let memories = $state<MemorySearchResult[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let searchQuery = $state('');
	let searching = $state(false);
	let deletingId = $state<string | null>(null);

	onMount(() => {
		loadMemories();
	});

	async function loadMemories() {
		loading = true;
		error = null;
		try {
			memories = await memory.list(20, agentId);
		} catch (e) {
			error = `Failed to load memories: ${e}`;
		}
		loading = false;
	}

	async function handleSearch() {
		if (!searchQuery.trim()) {
			await loadMemories();
			return;
		}
		searching = true;
		error = null;
		try {
			memories = await memory.search(searchQuery.trim(), 20);
		} catch (e) {
			error = `Search failed: ${e}`;
		}
		searching = false;
	}

	async function handleDelete(id: string) {
		deletingId = id;
		try {
			await memory.delete(id);
			memories = memories.filter(m => m.memory.id !== id);
		} catch (e) {
			error = `Failed to delete: ${e}`;
		}
		deletingId = null;
	}

	function truncate(text: string, maxLen: number): string {
		if (text.length <= maxLen) return text;
		return text.slice(0, maxLen) + '...';
	}
</script>

<div class="space-y-6">
	<!-- Search -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Memory</h2>
		<div class="flex gap-2">
			<input
				type="text"
				bind:value={searchQuery}
				placeholder="Search memories..."
				onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') handleSearch(); }}
				class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
			/>
			<button
				onclick={handleSearch}
				disabled={searching}
				class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
			>
				{searching ? 'Searching...' : 'Search'}
			</button>
			{#if searchQuery}
				<button
					onclick={() => { searchQuery = ''; loadMemories(); }}
					class="rounded-md border border-border px-3 py-2 text-sm text-muted-foreground hover:bg-accent transition-colors"
				>
					Clear
				</button>
			{/if}
		</div>
	</div>

	<!-- Memory list -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<div class="flex items-center justify-between">
			<h2 class="text-sm font-semibold">
				{searchQuery ? 'Search Results' : 'Recent Memories'}
			</h2>
			<button
				onclick={loadMemories}
				class="rounded-md border border-border px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
			>
				Refresh
			</button>
		</div>

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading memories...</p>
		{:else if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{error}
			</div>
		{:else if memories.length === 0}
			<p class="text-sm text-muted-foreground italic">
				{searchQuery ? 'No memories match your search.' : 'No memories stored yet.'}
			</p>
		{:else}
			<div class="space-y-2">
				{#each memories as result}
					{@const mem = result.memory}
					<div class="rounded-md border border-border p-3 space-y-2 hover:bg-accent/30">
						<div class="flex items-start justify-between gap-2">
							<p class="text-sm text-foreground flex-1 min-w-0">
								{truncate(mem.content, 200)}
							</p>
							<button
								onclick={() => handleDelete(mem.id)}
								disabled={deletingId === mem.id}
								class="shrink-0 rounded p-1 text-muted-foreground hover:bg-accent hover:text-destructive disabled:opacity-50 transition-colors"
								title="Delete memory"
							>
								{#if deletingId === mem.id}
									<span class="text-xs">...</span>
								{:else}
									&#x2715;
								{/if}
							</button>
						</div>
						<div class="flex items-center gap-3 text-xs text-muted-foreground">
							<span>{timeAgo(mem.created_at)}</span>
							{#if result.relevance_score > 0}
								<span>relevance: {(result.relevance_score * 100).toFixed(0)}%</span>
							{/if}
							{#if mem.tags.length > 0}
								<div class="flex gap-1">
									{#each mem.tags as tag}
										<span class="inline-flex rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
											{tag}
										</span>
									{/each}
								</div>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
