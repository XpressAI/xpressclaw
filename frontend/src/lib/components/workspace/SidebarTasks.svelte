<script lang="ts">
	import type { Agent, Conversation, Project, Task } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	const TASKS_PER_PROJECT = 5;

	let {
		projectList,
		conversationList,
		agentList,
		taskList,
		activeTaskId = null,
		compact = false,
		showHeading = true,
		onnavigate,
	}: {
		projectList: Project[];
		conversationList: Conversation[];
		agentList: Agent[];
		taskList: Task[];
		activeTaskId?: string | null;
		compact?: boolean;
		showHeading?: boolean;
		onnavigate?: () => void;
	} = $props();

	let sortedTasks = $derived([...taskList].sort(compareTaskRecency));
	let taskGroups = $derived((() => {
		const projectIds = new Set(projectList.map((project) => project.id));
		const groups = projectList.map((project) => ({
			id: project.id,
			label: project.name,
			href: `/projects/${project.id}`,
			tasks: sortedTasks.filter((task) => taskProjectId(task) === project.id).slice(0, TASKS_PER_PROJECT),
		}));
		const unassigned = sortedTasks
			.filter((task) => {
				const projectId = taskProjectId(task);
				return !projectId || !projectIds.has(projectId);
			})
			.slice(0, TASKS_PER_PROJECT);
		return unassigned.length > 0
			? [...groups, { id: 'unassigned', label: 'Unassigned', href: null, tasks: unassigned }]
			: groups;
	})());
	let compactTasks = $derived(sortedTasks.slice(0, TASKS_PER_PROJECT));

	function compareTaskRecency(left: Task, right: Task): number {
		return Date.parse(right.updated_at) - Date.parse(left.updated_at)
			|| Date.parse(right.created_at) - Date.parse(left.created_at)
			|| right.id.localeCompare(left.id);
	}

	function taskProjectId(task: Task): string | null {
		if (task.project_id) return task.project_id;
		if (task.conversation_id) {
			const conversation = conversationList.find((candidate) => candidate.id === task.conversation_id);
			if (conversation?.project_id) return conversation.project_id;
		}
		return agentList.find((agent) => agent.id === task.agent_id)?.project_id ?? null;
	}

	function statusDot(status: string): string {
		if (status === 'failed' || status === 'error' || status === 'blocked') return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (status === 'running' || status === 'in_progress' || status === 'preparing' || status === 'review') return 'bg-blue-500 animate-pulse';
		if (status === 'queued' || status === 'pending') return 'bg-amber-400';
		if (status === 'cancelled') return 'bg-muted-foreground';
		return 'bg-emerald-500';
	}

	function statusLabel(status: string): string {
		if (status === 'failed' || status === 'error') return 'Failed';
		if (status === 'blocked') return 'Blocked';
		if (status === 'waiting_for_input') return 'Waiting for you';
		if (status === 'running' || status === 'in_progress' || status === 'preparing') return 'Working';
		if (status === 'review') return 'Ready for review';
		if (status === 'queued' || status === 'pending') return 'Queued';
		if (status === 'cancelled') return 'Cancelled';
		return 'Completed';
	}
</script>

{#if compact}
	<div data-sidebar-mode="tasks" class="flex flex-col items-center gap-1">
		{#each compactTasks as task (task.id)}
			<a
				href="/tasks/{task.id}"
				onclick={onnavigate}
				aria-current={activeTaskId === task.id ? 'page' : undefined}
				class="relative flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {activeTaskId === task.id ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}"
				title="{task.title} — {statusLabel(task.status)}"
			>
				T
				<span data-task-status={task.status} class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {statusDot(task.status)}"></span>
			</a>
		{/each}
	</div>
{:else}
	<div data-sidebar-mode="tasks">
		{#if showHeading}
			<div class="mb-1.5 flex items-center justify-between px-2">
				<span class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Tasks</span>
				<a href="/tasks" onclick={onnavigate} class="text-[10px] text-muted-foreground hover:text-foreground">All tasks</a>
			</div>
		{/if}

		{#if taskGroups.length === 0}
			<div class="px-2 py-4 text-xs text-muted-foreground">Create an agent to start adding tasks.</div>
		{:else}
			<div class="space-y-3">
				{#each taskGroups as group (group.id)}
					<section data-sidebar-project-group={group.id}>
						<h2 class="mb-1 truncate px-2 text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground" title={group.label}>
							{#if group.href}<a href={group.href} onclick={onnavigate} class="hover:text-foreground">{group.label}</a>{:else}{group.label}{/if}
						</h2>
						{#if group.tasks.length === 0}
							<div class="px-2 py-1.5 text-[10px] text-muted-foreground/70">No tasks yet</div>
						{:else}
							<div class="space-y-0.5">
								{#each group.tasks as task (task.id)}
									<a
										data-sidebar-task
										href="/tasks/{task.id}"
										onclick={onnavigate}
										aria-current={activeTaskId === task.id ? 'page' : undefined}
										class="group flex items-start gap-2 rounded-lg px-2 py-2 text-left transition-colors {activeTaskId === task.id ? 'bg-[hsl(var(--sidebar-active))] text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}"
									>
										<span class="min-w-0 flex-1">
											<span class="block truncate text-xs">{task.title}</span>
											<span class="mt-0.5 block truncate text-[10px] font-normal text-muted-foreground">{statusLabel(task.status)} · {timeAgo(task.updated_at)}</span>
										</span>
										<span
											data-task-status={task.status}
											aria-label={statusLabel(task.status)}
											class="mt-1.5 h-2 w-2 shrink-0 rounded-full {statusDot(task.status)}"
										></span>
									</a>
								{/each}
							</div>
						{/if}
					</section>
				{/each}
			</div>
		{/if}
	</div>
{/if}
