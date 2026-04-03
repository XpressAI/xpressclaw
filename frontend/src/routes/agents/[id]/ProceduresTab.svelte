<script lang="ts">
	import { onMount } from 'svelte';
	import { procedures } from '$lib/api';
	import type { Sop } from '$lib/api';

	interface Props {
		agentId: string;
	}

	let { agentId }: Props = $props();

	let procedureList = $state<Sop[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let expandedName = $state<string | null>(null);
	let runningName = $state<string | null>(null);
	let runMessage = $state<string | null>(null);

	onMount(() => {
		loadProcedures();
	});

	async function loadProcedures() {
		loading = true;
		error = null;
		try {
			procedureList = await procedures.list();
		} catch (e) {
			error = `Failed to load procedures: ${e}`;
		}
		loading = false;
	}

	function toggleExpand(name: string) {
		expandedName = expandedName === name ? null : name;
	}

	async function runProcedure(name: string) {
		runningName = name;
		runMessage = null;
		try {
			const task = await procedures.run(name, { agent_id: agentId });
			runMessage = `Started task: ${task.title} (${task.id})`;
		} catch (e) {
			runMessage = `Failed to run: ${e}`;
		}
		runningName = null;
	}
</script>

<div class="space-y-6">
	<div class="rounded-lg border border-border bg-card p-4 space-y-4">
		<div class="flex items-center justify-between">
			<h2 class="text-sm font-semibold">Procedures (SOPs)</h2>
			<button
				onclick={loadProcedures}
				class="rounded-md border border-border px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
			>
				Refresh
			</button>
		</div>

		{#if runMessage}
			<div class="rounded-lg border border-border bg-muted p-3 text-sm text-foreground">
				{runMessage}
			</div>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading procedures...</p>
		{:else if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{error}
			</div>
		{:else if procedureList.length === 0}
			<p class="text-sm text-muted-foreground italic">No procedures defined. Create procedures in the Procedures section.</p>
		{:else}
			<div class="space-y-2">
				{#each procedureList as proc}
					{@const isExpanded = expandedName === proc.name}
					<div class="rounded-md border border-border hover:bg-accent/30 transition-colors">
						<div class="flex items-center justify-between p-3">
							<button
								onclick={() => toggleExpand(proc.name)}
								class="flex-1 text-left min-w-0"
							>
								<span class="text-sm font-medium text-foreground">{proc.name}</span>
								{#if proc.description}
									<p class="text-xs text-muted-foreground mt-0.5 line-clamp-1">{proc.description}</p>
								{/if}
							</button>
							<div class="flex items-center gap-2 shrink-0 ml-3">
								<button
									onclick={() => runProcedure(proc.name)}
									disabled={runningName === proc.name}
									class="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
								>
									{runningName === proc.name ? 'Running...' : 'Run'}
								</button>
								<span class="text-muted-foreground text-xs">
									{isExpanded ? '\u25B2' : '\u25BC'}
								</span>
							</div>
						</div>

						{#if isExpanded}
							<div class="border-t border-border px-3 pb-3 pt-2 space-y-2">
								{#if proc.parsed?.summary}
									<p class="text-xs text-muted-foreground">{proc.parsed.summary}</p>
								{/if}

								{#if proc.parsed?.steps && proc.parsed.steps.length > 0}
									<div>
										<h4 class="text-xs font-medium text-muted-foreground mb-1">Steps</h4>
										<ol class="list-decimal list-inside space-y-1">
											{#each proc.parsed.steps as step}
												<li class="text-xs text-foreground">
													<span class="font-medium">{step.name}</span>
													{#if step.description}
														<span class="text-muted-foreground"> - {step.description}</span>
													{/if}
													{#if step.optional}
														<span class="text-muted-foreground italic"> (optional)</span>
													{/if}
												</li>
											{/each}
										</ol>
									</div>
								{/if}

								{#if proc.parsed?.tools && proc.parsed.tools.length > 0}
									<div class="flex items-center gap-1.5">
										<span class="text-xs text-muted-foreground">Tools:</span>
										{#each proc.parsed.tools as tool}
											<span class="inline-flex rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
												{tool}
											</span>
										{/each}
									</div>
								{/if}

								<details class="mt-2">
									<summary class="cursor-pointer text-xs text-muted-foreground hover:text-foreground select-none">
										Raw content
									</summary>
									<pre class="mt-1 rounded-md bg-background border border-border p-2 text-xs font-mono text-foreground whitespace-pre-wrap overflow-x-auto max-h-64 overflow-y-auto">{proc.content}</pre>
								</details>
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
