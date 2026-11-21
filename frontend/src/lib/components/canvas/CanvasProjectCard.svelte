<script lang="ts">
	import { Code2, Globe, Image as ImageIcon, MessageSquare, SquareTerminal, GitBranch, FolderKanban, Bot, Plus } from '@lucide/svelte';
	import type { Agent, Conversation, Project, Task } from '$lib/api';
	import { harnessMark, timeAgo } from '$lib/utils';

	interface Props {
		nodeId: string;
		project: Project | null;
		agents: Agent[];
		conversations: Conversation[];
		tasks: Task[];
		onSpawnTask: (task: Task, parentNodeId: string) => void;
		onSpawnConversation: (conversation: Conversation, parentNodeId: string) => void;
		onSpawnAgent: (agent: Agent, parentNodeId: string) => void;
		onSpawnTool: (kind: 'editor' | 'terminal' | 'image' | 'app', agentId: string | null, parentNodeId: string) => void;
		onExpandActiveTasks: (projectId: string, parentNodeId: string) => void;
	}

	let {
		nodeId,
		project,
		agents,
		conversations,
		tasks,
		onSpawnTask,
		onSpawnConversation,
		onSpawnAgent,
		onSpawnTool,
		onExpandActiveTasks,
	}: Props = $props();

	let projectAgents = $derived(project ? agents.filter((agent) => agent.project_id === project.id) : []);
	let projectConversations = $derived(project ? conversations.filter((conversation) => conversation.project_id === project.id) : []);
	let projectTasks = $derived((() => {
		if (!project) return [];
		const agentIds = new Set(projectAgents.map((agent) => agent.id));
		return tasks
			.filter((task) => task.project_id ? task.project_id === project.id : Boolean(task.agent_id && agentIds.has(task.agent_id)))
			.sort((a, b) => (b.updated_at || '').localeCompare(a.updated_at || ''));
	})());
	let activeTasks = $derived(projectTasks.filter((task) => !['completed', 'cancelled'].includes(task.status)));
	let delegatedCount = $derived(projectTasks.filter((task) => Boolean(task.parent_task_id)).length);
	let primaryAgentId = $derived(projectAgents[0]?.id ?? null);

	function statusDot(status: string): string {
		if (['failed', 'error', 'blocked'].includes(status)) return 'bg-red-500';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (['running', 'in_progress', 'preparing', 'review'].includes(status)) return 'bg-blue-500 animate-pulse';
		if (['queued', 'pending', 'waiting_for_subtasks', 'waiting_for_dependencies'].includes(status)) return 'bg-amber-400';
		return 'bg-emerald-500';
	}
</script>

<div class="workspace-scroll-y flex h-full flex-col gap-4 p-3.5 text-xs" data-canvas-project-card={project?.id ?? ''}>
	{#if !project}
		<div class="m-auto text-center text-muted-foreground">Project is no longer available.</div>
	{:else}
		<div class="flex items-start gap-2.5">
			<span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-sm font-semibold text-primary">
				{project.icon || project.name.slice(0, 1).toUpperCase()}
			</span>
			<div class="min-w-0 flex-1">
				<div class="truncate text-sm font-semibold text-foreground">{project.name}</div>
				<p class="mt-0.5 line-clamp-2 text-[11px] text-muted-foreground">
					{project.description || 'Spatial hub for agents, conversations, tasks, and contextual tools.'}
				</p>
			</div>
		</div>

		<div class="flex flex-wrap gap-1.5">
			<button
				type="button"
				onclick={() => onExpandActiveTasks(project.id, nodeId)}
				class="inline-flex items-center gap-1 rounded-md border border-primary/30 bg-primary/10 px-2 py-1 text-[11px] font-medium text-primary hover:bg-primary/15"
			>
				<GitBranch class="h-3 w-3" />
				<span>Fan out work ({activeTasks.length})</span>
			</button>
			<button
				type="button"
				onclick={() => onSpawnTool('editor', primaryAgentId, nodeId)}
				disabled={!primaryAgentId}
				class="inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] text-foreground hover:bg-accent disabled:opacity-40"
				title="Open Code Editor in context"
			>
				<Code2 class="h-3 w-3 text-sky-500" />
				<span>Editor</span>
			</button>
			<button
				type="button"
				onclick={() => onSpawnTool('terminal', primaryAgentId, nodeId)}
				disabled={!primaryAgentId}
				class="inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] text-foreground hover:bg-accent disabled:opacity-40"
				title="Open Terminal in context"
			>
				<SquareTerminal class="h-3 w-3 text-emerald-500" />
				<span>Terminal</span>
			</button>
			<button
				type="button"
				onclick={() => onSpawnTool('image', primaryAgentId, nodeId)}
				disabled={!primaryAgentId}
				class="inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] text-foreground hover:bg-accent disabled:opacity-40"
				title="Open Image Viewer in context"
			>
				<ImageIcon class="h-3 w-3 text-violet-500" />
				<span>Image</span>
			</button>
			<button
				type="button"
				onclick={() => onSpawnTool('app', primaryAgentId, nodeId)}
				class="inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] text-foreground hover:bg-accent"
				title="Open Web App Preview in context"
			>
				<Globe class="h-3 w-3 text-amber-500" />
				<span>App</span>
			</button>
		</div>

		<section class="space-y-1.5">
			<div class="flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
				<span class="inline-flex items-center gap-1"><Bot class="h-3 w-3" /> Agents ({projectAgents.length})</span>
			</div>
			{#if projectAgents.length === 0}
				<p class="rounded-lg border border-dashed border-border px-2.5 py-2 text-[11px] text-muted-foreground">No agents assigned yet.</p>
			{:else}
				<div class="space-y-1">
					{#each projectAgents as agent (agent.id)}
						<div class="flex items-center gap-1.5 rounded-lg border border-border/80 bg-[hsl(var(--inset))] px-2 py-1.5">
							<button
								type="button"
								onclick={() => onSpawnAgent(agent, nodeId)}
								class="flex min-w-0 flex-1 items-center gap-2 text-left hover:text-primary"
								title="Open Agent window on canvas"
							>
								<span class="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted text-[9px] font-semibold">{harnessMark(agent.backend)}</span>
								<span class="min-w-0 flex-1 truncate font-medium text-foreground">{agent.title || agent.name}</span>
								<span class="h-2 w-2 shrink-0 rounded-full {statusDot(agent.status)}"></span>
							</button>
							<div class="flex shrink-0 items-center gap-0.5 border-l border-border/70 pl-1">
								<button type="button" onclick={() => onSpawnTool('editor', agent.id, nodeId)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-sky-500" title="Open {agent.title || agent.name} editor" aria-label="Open {agent.title || agent.name} editor"><Code2 class="h-3 w-3" /></button>
								<button type="button" onclick={() => onSpawnTool('terminal', agent.id, nodeId)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-emerald-500" title="Open {agent.title || agent.name} terminal" aria-label="Open {agent.title || agent.name} terminal"><SquareTerminal class="h-3 w-3" /></button>
								<button type="button" onclick={() => onSpawnTool('image', agent.id, nodeId)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-violet-500" title="Open {agent.title || agent.name} image" aria-label="Open {agent.title || agent.name} image"><ImageIcon class="h-3 w-3" /></button>
								<button type="button" onclick={() => onSpawnTool('app', agent.id, nodeId)} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-amber-500" title="Open {agent.title || agent.name} app" aria-label="Open {agent.title || agent.name} app"><Globe class="h-3 w-3" /></button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</section>

		<section class="space-y-1.5">
			<div class="flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
				<span class="inline-flex items-center gap-1"><FolderKanban class="h-3 w-3" /> Tasks ({projectTasks.length}{#if delegatedCount > 0} · {delegatedCount} delegated{/if})</span>
			</div>
			{#if projectTasks.length === 0}
				<p class="rounded-lg border border-dashed border-border px-2.5 py-2 text-[11px] text-muted-foreground">No tasks in this project yet.</p>
			{:else}
				<div class="space-y-1">
					{#each projectTasks.slice(0, 8) as task (task.id)}
						{@const status = task.activity_status ?? task.status}
						<button
							type="button"
							onclick={() => onSpawnTask(task, nodeId)}
							class="flex w-full items-center gap-2 rounded-lg border border-border/70 bg-card px-2.5 py-1.5 text-left transition hover:border-primary/40 hover:bg-[hsl(var(--hover))]"
						>
							<span class="h-2 w-2 shrink-0 rounded-full {statusDot(status)}"></span>
							<span class="min-w-0 flex-1">
								<span class="flex items-center gap-1 truncate font-medium text-foreground">
									{#if task.parent_task_id}<GitBranch class="h-3 w-3 shrink-0 text-muted-foreground" />{/if}
									<span class="truncate">{task.title}</span>
								</span>
								<span class="block truncate text-[10px] text-muted-foreground">
									{status.replaceAll('_', ' ')} · {timeAgo(task.updated_at)}
								</span>
							</span>
							<Plus class="h-3 w-3 shrink-0 text-muted-foreground" />
						</button>
					{/each}
				</div>
			{/if}
		</section>

		<section class="space-y-1.5">
			<div class="flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
				<span class="inline-flex items-center gap-1"><MessageSquare class="h-3 w-3" /> Conversations ({projectConversations.length})</span>
			</div>
			{#if projectConversations.length === 0}
				<p class="rounded-lg border border-dashed border-border px-2.5 py-2 text-[11px] text-muted-foreground">No conversations yet.</p>
			{:else}
				<div class="space-y-1">
					{#each projectConversations.slice(0, 5) as conversation (conversation.id)}
						<button
							type="button"
							onclick={() => onSpawnConversation(conversation, nodeId)}
							class="flex w-full items-center gap-2 rounded-lg border border-border/70 bg-card px-2.5 py-1.5 text-left transition hover:border-primary/40 hover:bg-[hsl(var(--hover))]"
						>
							<span class="text-xs text-muted-foreground">{conversation.icon || '#'}</span>
							<span class="min-w-0 flex-1 truncate font-medium text-foreground">{conversation.title || 'Untitled conversation'}</span>
							<Plus class="h-3 w-3 shrink-0 text-muted-foreground" />
						</button>
					{/each}
				</div>
			{/if}
		</section>
	{/if}
</div>
