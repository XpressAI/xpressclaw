<script lang="ts" module>
	import { createLazyComponentRegistry, type AnyLazyComponent } from '$lib/lazyComponent';
	import type { WorkspaceTabKind } from '$lib/workspace';

	const loaders = {
		home: () => import('../../../routes/+page.svelte'),
		dashboard: () => import('../../../routes/dashboard/+page.svelte'),
		projects: () => import('../../../routes/projects/ProjectsPage.svelte'),
		project: () => import('../../../routes/projects/[id]/ProjectOverview.svelte'),
		agents: () => import('../../../routes/agents/+page.svelte'),
		agent: () => import('../../../routes/agents/[id]/ProjectView.svelte'),
		conversation: () => import('../../../routes/conversations/[id]/ConversationView.svelte'),
		tasks: () => import('../../../routes/tasks/+page.svelte'),
		task: () => import('../../../routes/tasks/[id]/TaskView.svelte'),
		automations: () => import('../../../routes/workflows/+page.svelte'),
		workflow: () => import('../../../routes/workflows/[id]/WorkflowView.svelte'),
		'workflow-new': () => import('../../../routes/workflows/new/+page.svelte'),
		settings: () => import('./SettingsView.svelte')
	};

	type ViewGroupKey = keyof typeof loaders;
	const views = createLazyComponentRegistry<ViewGroupKey>(loaders);

	function viewGroup(kind: WorkspaceTabKind): ViewGroupKey {
		if (kind === 'automations' || kind === 'schedules' || kind === 'workflows') return 'automations';
		if (kind in loaders) return kind as ViewGroupKey;
		return 'settings';
	}
</script>

<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { projectSection, type WorkspaceTab } from '$lib/workspace';

	let { tab, compact = false }: { tab: WorkspaceTab; compact?: boolean } = $props();
	let group = $derived(viewGroup(tab.kind));
	const initialGroup = untrack(() => group);
	let initialLoad: Promise<AnyLazyComponent> | null = views.has(initialGroup)
		? null
		: views.load(initialGroup);
	initialLoad?.catch(() => {});

	let ActiveView = $state<AnyLazyComponent | null>(views.get(initialGroup));
	let viewLoadError = $state<string | null>(null);
	let retryNonce = $state(0);
	let failedRetries = $state(0);

	onMount(() => {
		const handleOnline = () => {
			if (viewLoadError) retryNonce += 1;
		};
		window.addEventListener('online', handleOnline);
		return () => window.removeEventListener('online', handleOnline);
	});

	$effect(() => {
		const targetGroup = group;
		const currentRetry = retryNonce;
		const cached = views.get(targetGroup);
		if (cached) {
			initialLoad = null;
			ActiveView = cached;
			viewLoadError = null;
			failedRetries = 0;
			return;
		}
		ActiveView = null;
		viewLoadError = null;
		let cancelled = false;
		const pending =
			initialLoad && targetGroup === initialGroup && currentRetry === 0
				? initialLoad
				: views.load(targetGroup);
		initialLoad = null;
		pending
			.then((component) => {
				if (!cancelled) {
					ActiveView = component;
					viewLoadError = null;
					failedRetries = 0;
				}
			})
			.catch((err) => {
				if (!cancelled) {
					viewLoadError =
						err instanceof Error && err.message ? err.message : 'Could not load this view.';
					if (currentRetry > 0) failedRetries += 1;
				}
			});
		return () => {
			cancelled = true;
		};
	});
</script>

{#if ActiveView}
	{#if tab.kind === 'project' && tab.resourceId}
		<ActiveView projectId={tab.resourceId} />
	{:else if tab.kind === 'agent' && tab.resourceId}
		<ActiveView agentId={tab.resourceId} section={projectSection(tab.path)} route={tab.path} />
	{:else if tab.kind === 'conversation' && tab.resourceId}
		<ActiveView conversationId={tab.resourceId} />
	{:else if tab.kind === 'tasks'}
		<ActiveView route={tab.path} />
	{:else if tab.kind === 'task' && tab.resourceId}
		<ActiveView taskId={tab.resourceId} {compact} />
	{:else if tab.kind === 'workflow' && tab.resourceId}
		<ActiveView workflowId={tab.resourceId} />
	{:else if group === 'settings'}
		<ActiveView kind={tab.kind} />
	{:else}
		<ActiveView />
	{/if}
{:else if viewLoadError}
	<div role="alert" data-view-load-error class="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
		<p class="text-sm text-destructive">Could not load this view. Check your connection and try again.</p>
		<div class="flex items-center gap-2">
			<button
				type="button"
				onclick={() => { retryNonce += 1; }}
				class="rounded-md bg-primary px-4 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90"
			>
				Retry
			</button>
			{#if failedRetries > 0}
				<button
					type="button"
					onclick={() => window.location.reload()}
					class="rounded-md border border-border bg-card px-4 py-1.5 text-xs font-medium hover:bg-accent"
				>
					Reload page
				</button>
			{/if}
		</div>
	</div>
{/if}
