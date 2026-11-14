<script lang="ts" module>
	import type { Component } from 'svelte';
	import type { WorkspaceTabKind } from '$lib/workspace';

	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	type AnyComponent = Component<any>;

	const loaders: Record<string, () => Promise<{ default: AnyComponent }>> = {
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

	const resolvedViews = new Map<string, AnyComponent>();
	const inflightViews = new Map<string, Promise<AnyComponent>>();

	function viewGroup(kind: WorkspaceTabKind): string {
		if (kind === 'automations' || kind === 'schedules' || kind === 'workflows') return 'automations';
		if (kind in loaders) return kind;
		return 'settings';
	}

	function loadView(group: string): Promise<AnyComponent> {
		const cached = resolvedViews.get(group);
		if (cached) return Promise.resolve(cached);
		const inflight = inflightViews.get(group);
		if (inflight) return inflight;
		const loader = loaders[group] ?? loaders.settings;
		const pending = loader().then((mod) => {
			resolvedViews.set(group, mod.default);
			inflightViews.delete(group);
			return mod.default;
		});
		inflightViews.set(group, pending);
		return pending;
	}
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import { projectSection, type WorkspaceTab } from '$lib/workspace';

	let { tab, compact = false }: { tab: WorkspaceTab; compact?: boolean } = $props();
	let group = $derived(viewGroup(tab.kind));
	const initialGroup = untrack(() => group);
	if (!resolvedViews.has(initialGroup)) void loadView(initialGroup);
	let ActiveView = $state<AnyComponent | null>(resolvedViews.get(initialGroup) ?? null);

	$effect(() => {
		const targetGroup = group;
		const cached = resolvedViews.get(targetGroup);
		if (cached) {
			ActiveView = cached;
			return;
		}
		ActiveView = null;
		let cancelled = false;
		void loadView(targetGroup).then((component) => {
			if (!cancelled) ActiveView = component;
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
{/if}
