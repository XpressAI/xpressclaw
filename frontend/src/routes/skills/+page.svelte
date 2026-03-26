<script lang="ts">
	import { onMount } from 'svelte';

	interface Skill {
		name: string;
		description: string;
	}

	let skills = $state<Skill[]>([]);
	let selectedSkill = $state<{ name: string; description: string; content: string } | null>(null);
	let loading = $state(true);
	let showCreate = $state(false);
	let creating = $state(false);
	let createError = $state('');

	// Create form fields
	let newName = $state('');
	let newDescription = $state('');
	let newContent = $state('');

	onMount(loadSkills);

	async function loadSkills() {
		loading = true;
		try {
			const resp = await fetch('/api/skills');
			skills = await resp.json();
		} catch {}
		loading = false;
	}

	async function selectSkill(name: string) {
		try {
			const resp = await fetch(`/api/skills/${name}`);
			selectedSkill = await resp.json();
		} catch {}
	}

	async function createSkill() {
		if (!newName.trim() || !newDescription.trim()) return;
		creating = true;
		createError = '';
		try {
			const resp = await fetch('/api/skills', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					name: newName.trim().toLowerCase().replace(/\s+/g, '-'),
					description: newDescription.trim(),
					content: newContent.trim() || `# ${newName.trim()}\n\nAdd your skill instructions here.`,
				}),
			});
			if (!resp.ok) {
				const err = await resp.json();
				createError = err.error || 'Failed to create skill';
			} else {
				showCreate = false;
				newName = '';
				newDescription = '';
				newContent = '';
				await loadSkills();
			}
		} catch (e) {
			createError = String(e);
		}
		creating = false;
	}

	async function deleteSkill(name: string) {
		if (!confirm(`Delete skill "${name}"?`)) return;
		try {
			await fetch(`/api/skills/${name}`, { method: 'DELETE' });
			if (selectedSkill?.name === name) selectedSkill = null;
			await loadSkills();
		} catch {}
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Skills</h1>
			<p class="text-sm text-muted-foreground mt-1">
				Skills teach agents how to perform specific tasks. Assign skills to agents in their config.
			</p>
		</div>
		<button
			onclick={() => (showCreate = !showCreate)}
			class="rounded-lg bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			{showCreate ? 'Cancel' : '+ Create Skill'}
		</button>
	</div>

	<!-- Create Skill Form -->
	{#if showCreate}
		<div class="rounded-lg border border-primary/30 bg-primary/5 p-5 space-y-4">
			<h2 class="text-sm font-semibold">Create a New Skill</h2>

			<div>
				<label for="skill-name" class="block text-xs font-medium text-foreground mb-1">Name</label>
				<input id="skill-name" type="text" bind:value={newName} placeholder="my-skill (lowercase, hyphens)"
					class="w-full rounded-lg border border-border bg-secondary px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
			</div>

			<div>
				<label for="skill-desc" class="block text-xs font-medium text-foreground mb-1">Description</label>
				<input id="skill-desc" type="text" bind:value={newDescription} placeholder="When should the agent use this skill?"
					class="w-full rounded-lg border border-border bg-secondary px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				<p class="text-xs text-muted-foreground mt-1">Describes when the agent should apply this skill.</p>
			</div>

			<div>
				<label for="skill-content" class="block text-xs font-medium text-foreground mb-1">Instructions (Markdown)</label>
				<textarea id="skill-content" bind:value={newContent} rows={10}
					placeholder="# My Skill&#10;&#10;## Tools Available&#10;&#10;- `tool_name(args)` — description&#10;&#10;## Rules&#10;&#10;- Always do X&#10;- Never do Y"
					class="w-full rounded-lg border border-border bg-secondary px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring resize-y"></textarea>
				<p class="text-xs text-muted-foreground mt-1">This content is injected into the agent's system prompt when the skill is assigned.</p>
			</div>

			{#if createError}
				<p class="text-xs text-destructive">{createError}</p>
			{/if}

			<button
				onclick={createSkill}
				disabled={!newName.trim() || !newDescription.trim() || creating}
				class="rounded-lg bg-primary px-4 py-2 text-sm text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
			>
				{creating ? 'Creating...' : 'Create Skill'}
			</button>
		</div>
	{/if}

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if skills.length === 0 && !showCreate}
		<div class="rounded-lg border border-border bg-card p-8 text-center space-y-3">
			<p class="text-sm text-muted-foreground">No skills found.</p>
			<p class="text-xs text-muted-foreground/70">Click "Create Skill" to add your first skill.</p>
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
					<div class="flex items-center gap-2">
						<button
							onclick={() => deleteSkill(selectedSkill!.name)}
							class="text-xs text-destructive hover:underline"
						>Delete</button>
						<button
							onclick={() => (selectedSkill = null)}
							class="text-xs text-muted-foreground hover:text-foreground"
						>Close</button>
					</div>
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
