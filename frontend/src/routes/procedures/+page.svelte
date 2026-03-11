<script lang="ts">
	import { onMount } from 'svelte';
	import { procedures } from '$lib/api';
	import type { Sop } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let sopList = $state<Sop[]>([]);
	let loading = $state(true);
	let showCreate = $state(false);
	let selected = $state<Sop | null>(null);
	let form = $state({ name: '', description: '', content: '' });

	const defaultContent = `summary: Describe the procedure
tools: []
inputs:
  - name: example_input
    description: An example input parameter
    required: true
outputs:
  - name: result
    description: The output of the procedure
steps:
  - name: Step 1
    description: First step of the procedure
  - name: Step 2
    description: Second step of the procedure
`;

	onMount(() => load());

	async function load() {
		sopList = await procedures.list().catch(() => []);
		loading = false;
	}

	async function create() {
		if (!form.name || !form.content) return;
		try {
			await procedures.create({
				name: form.name,
				description: form.description || undefined,
				content: form.content
			});
			form = { name: '', description: '', content: '' };
			showCreate = false;
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	async function remove(name: string) {
		if (!confirm(`Delete procedure "${name}"?`)) return;
		await procedures.delete(name);
		if (selected?.name === name) selected = null;
		await load();
	}

	function selectSop(sop: Sop) {
		selected = selected?.id === sop.id ? null : sop;
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Procedures</h1>
			<p class="text-sm text-muted-foreground mt-1">{sopList.length} SOPs</p>
		</div>
		<button
			onclick={() => { showCreate = !showCreate; if (showCreate && !form.content) form.content = defaultContent; }}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			New Procedure
		</button>
	</div>

	{#if showCreate}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<input type="text" placeholder="Procedure name (e.g. deploy-service)" bind:value={form.name} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
			<input type="text" placeholder="Description" bind:value={form.description} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
			<textarea bind:value={form.content} rows="12" class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring resize-y"></textarea>
			<div class="flex gap-2">
				<button onclick={create} class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">Create</button>
				<button onclick={() => (showCreate = false)} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">Cancel</button>
			</div>
		</div>
	{/if}

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else}
		<div class="grid grid-cols-1 lg:grid-cols-3 gap-4">
			<!-- SOP list -->
			<div class="space-y-2 {selected ? 'lg:col-span-1' : 'lg:col-span-3'}">
				{#each sopList as sop}
					<button
						onclick={() => selectSop(sop)}
						class="w-full text-left rounded-lg border bg-card p-4 transition-colors {selected?.id === sop.id ? 'border-primary' : 'border-border hover:border-muted-foreground/30'}"
					>
						<div class="flex items-start justify-between">
							<div>
								<div class="text-sm font-semibold">{sop.name}</div>
								{#if sop.description}
									<div class="text-xs text-muted-foreground mt-0.5">{sop.description}</div>
								{/if}
							</div>
							<span class="text-xs text-muted-foreground">v{sop.version}</span>
						</div>
						<div class="text-xs text-muted-foreground mt-2">
							{sop.parsed?.steps?.length ?? 0} steps
							&middot; {sop.parsed?.inputs?.length ?? 0} inputs
							&middot; Updated {timeAgo(sop.updated_at)}
						</div>
					</button>
				{:else}
					<div class="rounded-lg border border-border bg-card p-8 text-center text-sm text-muted-foreground">
						No procedures defined
					</div>
				{/each}
			</div>

			<!-- Detail panel -->
			{#if selected}
				<div class="lg:col-span-2 rounded-lg border border-border bg-card p-4 space-y-4">
					<div class="flex items-start justify-between">
						<div>
							<h2 class="text-lg font-bold">{selected.name}</h2>
							{#if selected.parsed?.summary}
								<p class="text-sm text-muted-foreground mt-1">{selected.parsed.summary}</p>
							{/if}
						</div>
						<div class="flex gap-2">
							<button
								onclick={() => remove(selected!.name)}
								class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/10"
							>
								Delete
							</button>
						</div>
					</div>

					{#if selected.parsed?.inputs?.length}
						<div>
							<h3 class="text-sm font-semibold mb-2">Inputs</h3>
							<div class="space-y-1">
								{#each selected.parsed.inputs as input}
									<div class="flex items-center gap-2 text-sm">
										<code class="bg-muted px-1.5 py-0.5 rounded text-xs">{input.name}</code>
										<span class="text-muted-foreground">{input.description}</span>
										{#if input.required}
											<span class="text-xs text-destructive">required</span>
										{/if}
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if selected.parsed?.steps?.length}
						<div>
							<h3 class="text-sm font-semibold mb-2">Steps</h3>
							<ol class="space-y-2">
								{#each selected.parsed.steps as step, i}
									<li class="flex gap-3 text-sm">
										<span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-medium">
											{i + 1}
										</span>
										<div>
											<span class="font-medium">{step.name}</span>
											{#if step.optional}<span class="text-xs text-muted-foreground ml-1">(optional)</span>{/if}
											<p class="text-muted-foreground text-xs mt-0.5">{step.description}</p>
										</div>
									</li>
								{/each}
							</ol>
						</div>
					{/if}

					{#if selected.parsed?.outputs?.length}
						<div>
							<h3 class="text-sm font-semibold mb-2">Outputs</h3>
							{#each selected.parsed.outputs as output}
								<div class="flex items-center gap-2 text-sm">
									<code class="bg-muted px-1.5 py-0.5 rounded text-xs">{output.name}</code>
									<span class="text-muted-foreground">{output.description}</span>
								</div>
							{/each}
						</div>
					{/if}

					{#if selected.parsed?.tools?.length}
						<div>
							<h3 class="text-sm font-semibold mb-2">Tools</h3>
							<div class="flex flex-wrap gap-1">
								{#each selected.parsed.tools as tool}
									<span class="bg-muted px-2 py-0.5 rounded text-xs">{tool}</span>
								{/each}
							</div>
						</div>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</div>
