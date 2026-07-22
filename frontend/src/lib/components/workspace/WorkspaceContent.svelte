<script lang="ts">
	import { projectSection, type WorkspaceTab } from '$lib/workspace';
	import HomePage from '../../../routes/+page.svelte';
	import ProjectsPage from '../../../routes/agents/+page.svelte';
	import ProjectView from '../../../routes/agents/[id]/ProjectView.svelte';
	import TasksPage from '../../../routes/tasks/+page.svelte';
	import TaskView from '../../../routes/tasks/[id]/TaskView.svelte';
	import SchedulesPage from '../../../routes/schedules/+page.svelte';
	import WorkflowsPage from '../../../routes/workflows/+page.svelte';
	import WorkflowView from '../../../routes/workflows/[id]/WorkflowView.svelte';
	import NewWorkflowPage from '../../../routes/workflows/new/+page.svelte';
	import SettingsView from './SettingsView.svelte';

	let { tab, compact = false }: { tab: WorkspaceTab; compact?: boolean } = $props();
</script>

{#if tab.kind === 'home'}
	<HomePage />
{:else if tab.kind === 'projects'}
	<ProjectsPage />
{:else if tab.kind === 'project' && tab.resourceId}
	<ProjectView agentId={tab.resourceId} section={projectSection(tab.path)} />
{:else if tab.kind === 'tasks'}
	<TasksPage />
{:else if tab.kind === 'task' && tab.resourceId}
	<TaskView taskId={tab.resourceId} {compact} />
{:else if tab.kind === 'schedules'}
	<SchedulesPage />
{:else if tab.kind === 'workflows'}
	<WorkflowsPage />
{:else if tab.kind === 'workflow' && tab.resourceId}
	<WorkflowView workflowId={tab.resourceId} />
{:else if tab.kind === 'workflow-new'}
	<NewWorkflowPage />
{:else}
	<SettingsView kind={tab.kind} />
{/if}
