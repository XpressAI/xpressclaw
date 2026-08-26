<script lang="ts">
	import { projectSection, type WorkspaceTab } from '$lib/workspace';
	import HomePage from '../../../routes/+page.svelte';
	import DashboardPage from '../../../routes/dashboard/+page.svelte';
	import AgentsPage from '../../../routes/agents/+page.svelte';
	import AgentView from '../../../routes/agents/[id]/ProjectView.svelte';
	import ProjectsPage from '../../../routes/projects/ProjectsPage.svelte';
	import ProjectOverview from '../../../routes/projects/[id]/ProjectOverview.svelte';
	import ConversationView from '../../../routes/conversations/[id]/ConversationView.svelte';
	import TasksPage from '../../../routes/tasks/+page.svelte';
	import TaskView from '../../../routes/tasks/[id]/TaskView.svelte';
	import AutomationsPage from '../../../routes/workflows/+page.svelte';
	import WorkflowView from '../../../routes/workflows/[id]/WorkflowView.svelte';
	import NewWorkflowPage from '../../../routes/workflows/new/+page.svelte';
	import SettingsView from './SettingsView.svelte';

	let { tab, compact = false }: { tab: WorkspaceTab; compact?: boolean } = $props();
</script>

{#if tab.kind === 'home'}
	<HomePage />
{:else if tab.kind === 'dashboard'}
	<DashboardPage />
{:else if tab.kind === 'projects'}
	<ProjectsPage />
{:else if tab.kind === 'project' && tab.resourceId}
	<ProjectOverview projectId={tab.resourceId} />
{:else if tab.kind === 'agents'}
	<AgentsPage />
{:else if tab.kind === 'agent' && tab.resourceId}
	<AgentView agentId={tab.resourceId} section={projectSection(tab.path)} route={tab.path} />
{:else if tab.kind === 'conversation' && tab.resourceId}
	<ConversationView conversationId={tab.resourceId} />
{:else if tab.kind === 'tasks'}
	<TasksPage />
{:else if tab.kind === 'task' && tab.resourceId}
	<TaskView taskId={tab.resourceId} {compact} />
{:else if tab.kind === 'automations' || tab.kind === 'schedules' || tab.kind === 'workflows'}
	<AutomationsPage />
{:else if tab.kind === 'workflow' && tab.resourceId}
	<WorkflowView workflowId={tab.resourceId} />
{:else if tab.kind === 'workflow-new'}
	<NewWorkflowPage />
{:else}
	<SettingsView kind={tab.kind} />
{/if}
