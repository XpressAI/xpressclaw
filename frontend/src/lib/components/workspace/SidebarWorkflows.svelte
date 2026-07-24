<script lang="ts">
	import type { Workflow } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let {
		workflowList,
		activeWorkflowId = null,
		compact = false,
		showHeading = true,
		onnavigate,
	}: {
		workflowList: Workflow[];
		activeWorkflowId?: string | null;
		compact?: boolean;
		showHeading?: boolean;
		onnavigate?: () => void;
	} = $props();

	let sortedWorkflows = $derived([...workflowList].sort((left, right) =>
		Date.parse(right.updated_at) - Date.parse(left.updated_at)
		|| right.id.localeCompare(left.id)
	));
</script>

{#if compact}
	<div data-sidebar-mode="workflows" class="flex flex-col items-center gap-1">
		{#each sortedWorkflows as workflow (workflow.id)}
			<a
				href="/workflows/{workflow.id}"
				onclick={onnavigate}
				aria-current={activeWorkflowId === workflow.id ? 'page' : undefined}
				class="relative flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {activeWorkflowId === workflow.id ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}"
				title="{workflow.name} — {workflow.enabled ? 'Enabled' : 'Disabled'}"
			>
				W
				<span class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {workflow.enabled ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
			</a>
		{/each}
	</div>
{:else}
	<div data-sidebar-mode="workflows">
		{#if showHeading}
			<div class="mb-1.5 flex items-center justify-between px-2">
				<a href="/workflows" onclick={onnavigate} class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground hover:text-foreground">Workflows</a>
				<a href="/workflows/new" onclick={onnavigate} class="flex h-5 w-5 items-center justify-center rounded text-sm text-muted-foreground hover:bg-accent hover:text-foreground" title="New workflow">+</a>
			</div>
		{/if}

		{#if sortedWorkflows.length === 0}
			<div class="px-2 py-4 text-xs text-muted-foreground">No workflows yet.</div>
		{:else}
			<div class="space-y-0.5">
				{#each sortedWorkflows as workflow (workflow.id)}
					<a
						data-sidebar-workflow
						href="/workflows/{workflow.id}"
						onclick={onnavigate}
						aria-current={activeWorkflowId === workflow.id ? 'page' : undefined}
						class="flex items-start gap-2 rounded-lg px-2 py-2 transition-colors {activeWorkflowId === workflow.id ? 'bg-[hsl(var(--sidebar-active))] text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}"
					>
						<span class="min-w-0 flex-1">
							<span class="block truncate text-xs">{workflow.name}</span>
							<span class="mt-0.5 block truncate text-[10px]">{workflow.enabled ? 'Enabled' : 'Disabled'} · {timeAgo(workflow.updated_at)}</span>
						</span>
						<span aria-label={workflow.enabled ? 'Enabled' : 'Disabled'} class="mt-1.5 h-2 w-2 shrink-0 rounded-full {workflow.enabled ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
					</a>
				{/each}
			</div>
		{/if}
	</div>
{/if}
