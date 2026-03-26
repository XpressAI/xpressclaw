<script lang="ts">
	import { onMount } from 'svelte';

	interface Skill {
		name: string;
		description: string;
	}

	let skills = $state<Skill[]>([]);
	let selectedSkill = $state<{ name: string; description: string; content: string } | null>(null);
	let loading = $state(true);

	onMount(async () => {
		try {
			const resp = await fetch('/api/skills');
			skills = await resp.json();
		} catch {}
		loading = false;
	});

	async function selectSkill(name: string) {
		try {
			const resp = await fetch(`/api/skills/${name}`);
			selectedSkill = await resp.json();
		} catch {}
	}
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Skills</h1>
		<p class="text-sm text-muted-foreground mt-1">
			Skills teach agents how to perform specific tasks. Assign skills to agents in their config.
		</p>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if skills.length === 0}
		<div class="rounded-lg border border-border bg-card p-8 text-center space-y-3">
			<p class="text-sm text-muted-foreground">No skills found.</p>
			<p class="text-xs text-muted-foreground/70">Add SKILL.md files to templates/skills/</p>
		</div>
	{:else}
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
			{#each skills as skill}
				<button
					onclick={() => selectSkill(skill.name)}
					class="rounded-lg border border-border bg-card p-4 text-left hover:border-primary/50 transition-colors space-y-2 {selectedSkill?.name === skill.name ? 'border-primary/50 bg-primary/5' : ''}"
				>
					<div class="text-sm font-semibold">{skill.name}</div>
					<div class="text-xs text-muted-foreground line-clamp-2">{skill.description}</div>
				</button>
			{/each}
		</div>

		{#if selectedSkill}
			<div class="rounded-lg border border-border bg-card p-5 space-y-3">
				<div class="flex items-center justify-between">
					<h2 class="text-lg font-semibold">{selectedSkill.name}</h2>
					<button
						onclick={() => (selectedSkill = null)}
						class="text-xs text-muted-foreground hover:text-foreground"
					>Close</button>
				</div>
				<p class="text-sm text-muted-foreground">{selectedSkill.description}</p>
				<div class="border-t border-border pt-3">
					<div class="text-xs text-muted-foreground mb-2">
						Add to an agent: <code class="bg-muted px-1.5 py-0.5 rounded">skills: ["{selectedSkill.name}"]</code>
					</div>
					<pre class="text-xs bg-muted/50 rounded-lg p-4 overflow-x-auto max-h-96 overflow-y-auto whitespace-pre-wrap">{selectedSkill.content}</pre>
				</div>
			</div>
		{/if}
	{/if}
</div>
