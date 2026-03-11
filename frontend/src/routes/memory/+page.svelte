<script lang="ts">
	import { onMount } from 'svelte';
	import { memory } from '$lib/api';
	import type { MemorySearchResult, MemoryStats } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let memories = $state<MemorySearchResult[]>([]);
	let stats = $state<MemoryStats | null>(null);
	let searchQuery = $state('');
	let loading = $state(true);
	let searching = $state(false);

	onMount(async () => {
		const [list, s] = await Promise.all([
			memory.list(50).catch(() => []),
			memory.stats().catch(() => null)
		]);
		memories = list;
		stats = s;
		loading = false;
	});

	async function handleSearch() {
		if (!searchQuery.trim()) {
			memories = await memory.list(50);
			return;
		}
		searching = true;
		memories = await memory.search(searchQuery, 20).catch(() => []);
		searching = false;
	}

	async function handleDelete(id: string) {
		if (!confirm('Delete this memory?')) return;
		await memory.delete(id);
		memories = memories.filter((m) => m.memory.id !== id);
	}
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Memory</h1>
		<p class="text-sm text-muted-foreground mt-1">
			{#if stats}
				{stats.zettelkasten.total_memories} memories &middot;
				{stats.vector.embedding_count} embeddings &middot;
				{stats.zettelkasten.total_links} links
			{:else}
				Loading...
			{/if}
		</p>
	</div>

	<!-- Search -->
	<div class="flex gap-2">
		<input
			type="text"
			placeholder="Search memories (semantic)..."
			bind:value={searchQuery}
			onkeydown={(e) => e.key === 'Enter' && handleSearch()}
			class="flex-1 rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
		/>
		<button
			onclick={handleSearch}
			disabled={searching}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50"
		>
			{searching ? 'Searching...' : 'Search'}
		</button>
	</div>

	<!-- Memory list -->
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else}
		<div class="space-y-2">
			{#each memories as result}
				<div class="rounded-lg border border-border bg-card p-4 space-y-2">
					<div class="flex items-start justify-between gap-4">
						<div class="flex-1 min-w-0">
							<div class="text-sm font-semibold">{result.memory.summary}</div>
							<p class="text-sm text-muted-foreground mt-1 line-clamp-3">{result.memory.content}</p>
						</div>
						<div class="flex items-center gap-2 shrink-0">
							{#if result.relevance_score < 1}
								<span class="text-xs text-muted-foreground" title="Relevance">
									{(result.relevance_score * 100).toFixed(0)}%
								</span>
							{/if}
							<button
								onclick={() => handleDelete(result.memory.id)}
								class="text-xs text-muted-foreground hover:text-destructive"
								title="Delete"
							>&times;</button>
						</div>
					</div>
					<div class="flex flex-wrap gap-2 text-xs text-muted-foreground">
						<span class="bg-muted px-1.5 py-0.5 rounded">{result.memory.layer}</span>
						<span class="bg-muted px-1.5 py-0.5 rounded">{result.memory.source}</span>
						{#if result.memory.agent_id}
							<span class="bg-muted px-1.5 py-0.5 rounded">{result.memory.agent_id}</span>
						{/if}
						{#each result.memory.tags ?? [] as tag}
							<span class="bg-primary/10 text-primary px-1.5 py-0.5 rounded">#{tag}</span>
						{/each}
						<span>&middot; {timeAgo(result.memory.accessed_at)}</span>
						<span>&middot; accessed {result.memory.access_count}x</span>
					</div>
				</div>
			{:else}
				<div class="rounded-lg border border-border bg-card p-8 text-center text-sm text-muted-foreground">
					{searchQuery ? 'No results found' : 'No memories yet'}
				</div>
			{/each}
		</div>
	{/if}
</div>
