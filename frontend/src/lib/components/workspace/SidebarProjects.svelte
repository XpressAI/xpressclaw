<script lang="ts">
	import type { Agent, Conversation, Project, Task } from '$lib/api';
	import { agentRuntimeSummary, harnessMark } from '$lib/utils';

	let {
		projectList,
		conversationList,
		agentList,
		taskList,
		activeProjectId = null,
		activeConversationId = null,
		activeAgentId = null,
		compact = false,
		onnavigate,
		onagentcontext,
	}: {
		projectList: Project[];
		conversationList: Conversation[];
		agentList: Agent[];
		taskList: Task[];
		activeProjectId?: string | null;
		activeConversationId?: string | null;
		activeAgentId?: string | null;
		compact?: boolean;
		onnavigate?: () => void;
		onagentcontext?: (event: MouseEvent, agent: Agent) => void;
	} = $props();

	let visibleConversations = $derived(activeProjectId
		? conversationList.filter((conversation) => conversation.project_id === activeProjectId)
		: conversationList);
	let visibleAgents = $derived(activeProjectId
		? agentList.filter((agent) => agent.project_id === activeProjectId)
		: agentList);
	let createAgentHref = $derived(activeProjectId
		? `/setup?mode=add-session&project_id=${encodeURIComponent(activeProjectId)}`
		: '/setup?mode=add-session');

	function projectStatus(project: Project): string {
		const agents = agentList.filter((agent) => agent.project_id === project.id);
		const agentIds = new Set(agents.map((agent) => agent.id));
		const statuses = [
			...agents.map((agent) => agent.status),
			...taskList.filter((task) => task.agent_id && agentIds.has(task.agent_id) && !['completed', 'cancelled'].includes(task.status)).map((task) => task.status),
		];
		if (statuses.some((status) => status === 'failed' || status === 'error' || status === 'blocked')) return 'error';
		if (statuses.some((status) => status === 'waiting_for_input')) return 'waiting';
		if (statuses.some((status) => ['running', 'in_progress', 'preparing'].includes(status))) return 'working';
		if (statuses.some((status) => ['queued', 'pending'].includes(status))) return 'queued';
		return 'ready';
	}

	function dot(status: string): string {
		if (status === 'error') return 'bg-red-500';
		if (status === 'waiting') return 'bg-orange-500 animate-pulse';
		if (status === 'working') return 'bg-blue-500 animate-pulse';
		if (status === 'queued') return 'bg-amber-400';
		return 'bg-emerald-500';
	}

	function agentDot(agent: Agent): string {
		if (agent.status === 'error' || agent.status === 'failed') return 'bg-red-500';
		if (agent.status === 'running') return 'bg-blue-500 animate-pulse';
		return 'bg-emerald-500';
	}

	function link(active: boolean): string {
		return `flex items-center gap-2 rounded-lg px-2 py-1.5 text-xs transition-colors ${active ? 'bg-[hsl(var(--sidebar-active))] text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}`;
	}
</script>

{#if compact}
	<div class="flex flex-col items-center gap-1">
		{#each projectList.slice(0, 8) as project (project.id)}
			<a href="/projects/{encodeURIComponent(project.id)}" onclick={onnavigate} class="relative flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {activeProjectId === project.id ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}" title={project.name}>
				{project.icon || project.name.slice(0, 1).toUpperCase()}<span class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {dot(projectStatus(project))}"></span>
			</a>
		{/each}
	</div>
{:else}
	<div class="space-y-4">
		<section>
			<div class="mb-1 flex items-center justify-between px-2"><span class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Projects</span><a href="/projects" onclick={onnavigate} title="Create project" class="text-sm text-muted-foreground hover:text-foreground">+</a></div>
			<div class="space-y-0.5">{#each projectList.slice(0, 8) as project (project.id)}<a href="/projects/{encodeURIComponent(project.id)}" onclick={onnavigate} class={link(activeProjectId === project.id)}><span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-[10px] font-semibold">{project.icon || project.name.slice(0, 1).toUpperCase()}</span><span class="min-w-0 flex-1 truncate">{project.name}</span><span class="h-2 w-2 shrink-0 rounded-full {dot(projectStatus(project))}"></span></a>{/each}{#if projectList.length > 8}<a href="/projects" onclick={onnavigate} class={link(false)}>+ {projectList.length - 8} more projects</a>{/if}</div>
		</section>

		<section>
			<div class="mb-1 flex items-center justify-between px-2"><span class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Conversations</span>{#if activeProjectId}<a href="/projects/{encodeURIComponent(activeProjectId)}" onclick={onnavigate} title="New conversation" class="text-sm text-muted-foreground hover:text-foreground">+</a>{/if}</div>
			<div class="space-y-0.5">{#each visibleConversations.slice(0, 9) as conversation (conversation.id)}<a href="/conversations/{encodeURIComponent(conversation.id)}" onclick={onnavigate} class={link(activeConversationId === conversation.id)}><span class="text-sm text-muted-foreground">#</span><span class="min-w-0 flex-1 truncate">{conversation.title || 'Untitled conversation'}</span>{#if conversation.participants.some((participant) => participant.participant_type === 'agent')}<span class="h-1.5 w-1.5 rounded-full bg-emerald-500"></span>{/if}</a>{/each}{#if visibleConversations.length === 0}<p class="px-2 py-1 text-[11px] text-muted-foreground">No conversations yet.</p>{/if}{#if visibleConversations.length > 9}<a href={activeProjectId ? `/projects/${encodeURIComponent(activeProjectId)}` : '/projects'} onclick={onnavigate} class={link(false)}>+ More conversations</a>{/if}</div>
		</section>

		<section>
			<div class="mb-1 flex items-center justify-between px-2"><span class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">Agents</span><a href="/agents" onclick={onnavigate} class="text-[10px] text-muted-foreground hover:text-foreground">All agents</a></div>
			<div class="space-y-0.5">{#each visibleAgents as agent (agent.id)}<a href="/agents/{encodeURIComponent(agent.id)}" onclick={onnavigate} oncontextmenu={(event) => onagentcontext?.(event, agent)} class={link(activeAgentId === agent.id)}><span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-[9px] font-semibold">{harnessMark(agent.backend)}</span><span class="min-w-0 flex-1"><span class="block truncate">{agent.title || agent.name}</span><span class="block truncate text-[9px] text-muted-foreground">{agentRuntimeSummary(agent)}</span></span><span class="h-2 w-2 shrink-0 rounded-full {agentDot(agent)}"></span></a>{/each}{#if visibleAgents.length === 0}<a href={createAgentHref} onclick={onnavigate} class="block rounded-lg border border-dashed border-border p-2 text-center text-[11px] text-muted-foreground">+ Create Agent</a>{/if}</div>
		</section>
	</div>
{/if}
