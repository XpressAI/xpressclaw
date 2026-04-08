<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { workflows } from '$lib/api';
	import type { Workflow } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let workflowList = $state<Workflow[]>([]);
	let loading = $state(true);
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	onMount(async () => {
		await load();
		pollTimer = setInterval(load, 10000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function load() {
		try {
			workflowList = await workflows.list();
		} catch {
			workflowList = [];
		}
		loading = false;
	}

	function parseYamlCounts(yamlStr: string): { steps: number; flows: number } {
		const stepMatches = yamlStr.match(/^\s+- id:/gm);
		const flowMatches = yamlStr.match(/^\s{2}\w+:\s*$/gm);
		return {
			steps: stepMatches?.length ?? 0,
			flows: Math.max(1, (flowMatches?.length ?? 1) - 1) // subtract non-flow top-level keys
		};
	}

	async function toggleEnabled(wf: Workflow) {
		try {
			if (wf.enabled) {
				await workflows.disable(wf.id);
			} else {
				await workflows.enable(wf.id);
			}
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	async function deleteWorkflow(id: string) {
		if (!confirm('Delete this workflow? This cannot be undone.')) return;
		try {
			await workflows.delete(id);
			await load();
		} catch (e) {
			alert(String(e));
		}
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Workflows</h1>
			<p class="text-sm text-muted-foreground mt-1">
				{workflowList.length} workflow{workflowList.length !== 1 ? 's' : ''}
			</p>
		</div>
		<a
			href="/workflows/new"
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			New Workflow
		</a>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if workflowList.length === 0}
		<div class="rounded-lg border border-border bg-card p-12 text-center space-y-4">
			<div class="mx-auto flex h-14 w-14 items-center justify-center rounded-full bg-[hsl(225,50%,25%)/0.2]">
				<svg class="h-7 w-7 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
					<path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" />
				</svg>
			</div>
			<div>
				<p class="text-sm font-medium text-[hsl(220,20%,92%)]">No workflows yet</p>
				<p class="mt-1 text-xs text-muted-foreground">
					Create a workflow to orchestrate multi-agent pipelines with triggers, tasks, and notifications.
				</p>
			</div>
			<a
				href="/workflows/new"
				class="inline-flex items-center gap-2 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
			>
				<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
					<path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15" />
				</svg>
				Create Workflow
			</a>
		</div>
	{:else}
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each workflowList as wf}
				{@const counts = parseYamlCounts(wf.yaml_content)}
				<div class="rounded-lg border border-border bg-card p-4 space-y-3 group">
					<div class="flex items-start justify-between">
						<div class="min-w-0 flex-1">
							<a href="/workflows/{wf.id}" class="text-sm font-semibold hover:underline text-[hsl(220,20%,92%)]">
								{wf.name}
							</a>
							{#if wf.description}
								<p class="mt-0.5 text-xs text-muted-foreground line-clamp-2">{wf.description}</p>
							{/if}
						</div>
						<label class="relative inline-flex items-center cursor-pointer shrink-0 ml-3" title={wf.enabled ? 'Disable' : 'Enable'}>
							<input type="checkbox" checked={wf.enabled} onchange={() => toggleEnabled(wf)} class="sr-only peer" />
							<div class="w-8 h-[18px] bg-muted rounded-full peer peer-checked:bg-emerald-600 transition-colors after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:rounded-full after:h-3.5 after:w-3.5 after:transition-all peer-checked:after:translate-x-full"></div>
						</label>
					</div>

					<div class="flex items-center gap-3 text-[10px] text-muted-foreground">
						<span class="flex items-center gap-1">
							<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6A2.25 2.25 0 016 3.75h2.25A2.25 2.25 0 0110.5 6v2.25a2.25 2.25 0 01-2.25 2.25H6a2.25 2.25 0 01-2.25-2.25V6z" />
							</svg>
							{counts.steps} step{counts.steps !== 1 ? 's' : ''}
						</span>
						<span class="flex items-center gap-1">
							<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5" />
							</svg>
							{counts.flows} flow{counts.flows !== 1 ? 's' : ''}
						</span>
						<span>v{wf.version}</span>
					</div>

					<div class="flex items-center justify-between">
						<span class="text-[10px] text-muted-foreground">
							Updated {timeAgo(wf.updated_at)}
						</span>
						<div class="flex items-center gap-1.5">
							<a
								href="/workflows/{wf.id}"
								class="rounded-md border border-border bg-secondary px-2.5 py-1 text-[10px] font-medium hover:bg-accent transition-colors"
							>
								View
							</a>
							<button
								onclick={() => deleteWorkflow(wf.id)}
								class="rounded-md border border-border bg-secondary px-2.5 py-1 text-[10px] font-medium text-destructive hover:bg-destructive/10 transition-colors opacity-0 group-hover:opacity-100"
								title="Delete"
							>
								Delete
							</button>
						</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
