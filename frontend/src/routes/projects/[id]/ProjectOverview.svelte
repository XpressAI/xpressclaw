<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, conversations, projects, type Agent, type Conversation, type Project, type Task } from '$lib/api';
	import { agentRuntimeSummary, harnessMark, timeAgo } from '$lib/utils';
	import AgentLoading from '$lib/components/AgentLoading.svelte';

	let { projectId }: { projectId: string } = $props();
	let project = $state<Project | null>(null);
	let projectConversations = $state<Conversation[]>([]);
	let agentList = $state<Agent[]>([]);
	let taskList = $state<Task[]>([]);
	let loading = $state(true);
	let creatingConversation = $state(false);
	let addingAgent = $state(false);
	let conversationTitle = $state('');
	let selectedAgents = $state<string[]>([]);
	let selectedAgentToMove = $state('');
	let projectIdCopied = $state(false);
	let error = $state('');

	let projectAgents = $derived(agentList.filter((agent) => agent.project_id === projectId));
	let availableAgents = $derived(agentList.filter((agent) => agent.project_id !== projectId));

	onMount(() => void load());

	async function load() {
		try {
			[project, projectConversations, agentList, taskList] = await Promise.all([
				projects.get(projectId),
				conversations.list(projectId),
				agents.list(),
				projects.tasks(projectId),
			]);
			if (selectedAgents.length === 0) selectedAgents = project?.agent_ids.slice() ?? [];
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not load this project.';
		} finally {
			loading = false;
		}
	}

	function toggleAgent(agentId: string) {
		selectedAgents = selectedAgents.includes(agentId)
			? selectedAgents.filter((id) => id !== agentId)
			: [...selectedAgents, agentId];
	}

	async function copyProjectId() {
		if (!project) return;
		try {
			await navigator.clipboard.writeText(project.id);
			projectIdCopied = true;
		} catch {
			error = 'Could not copy the Project ID. It remains visible so you can select it.';
		}
	}

	async function createConversation() {
		if (!conversationTitle.trim()) return;
		error = '';
		try {
			const conversation = await conversations.create({
				project_id: projectId,
				title: conversationTitle.trim(),
				participant_ids: selectedAgents,
			});
			projectConversations = [conversation, ...projectConversations];
			conversationTitle = '';
			creatingConversation = false;
			if (project) project = { ...project, conversation_count: project.conversation_count + 1 };
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not create the conversation.';
		}
	}

	async function moveAgent() {
		if (!selectedAgentToMove) return;
		error = '';
		try {
			await projects.assignAgent(projectId, selectedAgentToMove);
			selectedAgentToMove = '';
			addingAgent = false;
			await load();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not add the Agent.';
		}
	}

	function taskDot(status: string): string {
		if (status === 'failed' || status === 'blocked') return 'bg-red-500';
		if (status === 'in_progress' || status === 'running') return 'bg-blue-500 animate-pulse';
		if (status === 'pending' || status === 'queued') return 'bg-amber-400';
		return 'bg-emerald-500';
	}
</script>

<div class="workspace-scroll-y h-full">
	{#if loading}
		<div class="flex h-full items-center justify-center"><AgentLoading label="Loading project" /></div>
	{:else if project}
		<div class="mx-auto max-w-6xl space-y-8 p-4 sm:p-6">
			<header class="flex flex-wrap items-start justify-between gap-4">
				<div class="flex min-w-0 items-center gap-4">
					<span class="flex h-12 w-12 shrink-0 items-center justify-center rounded-2xl bg-primary/10 text-xl font-semibold text-primary">{project.icon || project.name.slice(0, 1).toUpperCase()}</span>
					<div class="min-w-0"><p class="text-xs text-muted-foreground"><a href="/projects" class="hover:text-foreground">Projects</a> /</p><h1 class="truncate text-2xl font-bold">{project.name}</h1><p class="mt-1 text-sm text-muted-foreground">{project.description || 'A shared context for conversations, Agents, tasks, and memory.'}</p><div class="mt-2 flex min-w-0 items-center gap-2 text-[11px] text-muted-foreground"><span class="shrink-0 font-medium">Project ID</span><code data-project-id class="min-w-0 select-all truncate rounded bg-muted px-1.5 py-0.5 font-mono">{project.id}</code><button type="button" aria-label="Copy project ID" onclick={() => void copyProjectId()} class="shrink-0 font-medium text-primary hover:underline">{projectIdCopied ? 'Copied' : 'Copy ID'}</button></div></div>
				</div>
				<button type="button" onclick={() => (creatingConversation = !creatingConversation)} class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">+ Conversation</button>
			</header>

			{#if error}<div class="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>{/if}

			{#if creatingConversation}
				<form onsubmit={(event) => { event.preventDefault(); void createConversation(); }} class="ai-card space-y-4 p-4">
					<div><label for="conversation-title" class="text-xs font-medium">Conversation name</label><input id="conversation-title" bind:value={conversationTitle} placeholder="What are we working through?" class="mt-1 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring" /></div>
					<div><p class="mb-2 text-xs font-medium">Agents in this conversation</p><div class="flex flex-wrap gap-2">{#each projectAgents as agent (agent.id)}<button type="button" onclick={() => toggleAgent(agent.id)} class="flex items-center gap-2 rounded-full border px-3 py-1.5 text-xs {selectedAgents.includes(agent.id) ? 'border-primary bg-primary/10 text-primary' : 'border-border text-muted-foreground'}"><span class="flex h-5 w-5 items-center justify-center rounded-full bg-muted text-[9px]">{harnessMark(agent.backend)}</span>{agent.title || agent.name}</button>{/each}</div></div>
					<div class="flex justify-end gap-2"><button type="button" onclick={() => (creatingConversation = false)} class="rounded-lg border border-border px-3 py-2 text-sm">Cancel</button><button disabled={!conversationTitle.trim()} class="rounded-lg bg-primary px-4 py-2 text-sm text-primary-foreground disabled:opacity-40">Create conversation</button></div>
				</form>
			{/if}

			<section>
				<div class="mb-3 flex items-end justify-between"><div><h2 class="font-semibold">Conversations</h2><p class="text-xs text-muted-foreground">Talk with several Agents and turn decisions into durable work.</p></div><span class="text-xs text-muted-foreground">{projectConversations.length}</span></div>
				{#if projectConversations.length === 0}
					<button type="button" onclick={() => (creatingConversation = true)} class="w-full rounded-xl border border-dashed border-border p-8 text-center text-sm text-muted-foreground hover:border-primary/40 hover:text-foreground">Start the first conversation</button>
				{:else}
					<div class="grid gap-3 md:grid-cols-2">{#each projectConversations as conversation (conversation.id)}<a href="/conversations/{encodeURIComponent(conversation.id)}" data-project-conversation={conversation.id} class="ai-card p-4 transition hover:bg-[hsl(var(--hover))]"><div class="flex items-start gap-3"><span class="text-lg">{conversation.icon || '#'}</span><div class="min-w-0 flex-1"><h3 class="truncate text-sm font-medium">{conversation.title || 'Untitled conversation'}</h3><p class="mt-1 text-xs text-muted-foreground">{conversation.participants.filter((participant) => participant.participant_type === 'agent').length} Agents · {timeAgo(conversation.last_message_at || conversation.created_at)}</p></div><span class="text-muted-foreground">→</span></div></a>{/each}</div>
				{/if}
			</section>

			<div class="grid gap-8 lg:grid-cols-[minmax(0,1fr)_minmax(18rem,0.65fr)]">
				<section>
					<div class="mb-3 flex items-center justify-between"><div><h2 class="font-semibold">Recent tasks</h2><p class="text-xs text-muted-foreground">Work created here or from a conversation.</p></div><a href="/tasks" class="text-xs text-primary hover:underline">All tasks</a></div>
					<div class="ai-card overflow-hidden">{#if taskList.length === 0}<p class="p-5 text-sm text-muted-foreground">No tasks in this project yet.</p>{:else}{#each taskList.slice(0, 8) as task (task.id)}<a href="/tasks/{task.id}" class="flex items-center gap-3 border-b border-border/70 px-4 py-3 last:border-0 hover:bg-[hsl(var(--hover))]"><span class="h-2 w-2 shrink-0 rounded-full {taskDot(task.status)}"></span><span class="min-w-0 flex-1"><span class="block truncate text-sm">{task.title}</span><span class="text-[11px] text-muted-foreground">{task.status.replaceAll('_', ' ')} · {timeAgo(task.updated_at)}</span></span><span class="text-muted-foreground">→</span></a>{/each}{/if}</div>
				</section>

				<section>
					<div class="mb-3 flex items-center justify-between"><div><h2 class="font-semibold">Agents</h2><p class="text-xs text-muted-foreground">Durable harnesses and workspaces available here.</p></div><button type="button" onclick={() => (addingAgent = !addingAgent)} class="text-xs text-primary">+ Add</button></div>
					{#if addingAgent}<div class="mb-3 space-y-2 rounded-lg border border-border bg-card p-2"><div class="flex gap-2"><select bind:value={selectedAgentToMove} class="min-w-0 flex-1 rounded-md border border-input bg-background px-2 py-1.5 text-xs"><option value="">Choose an existing Agent…</option>{#each availableAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}</select><button type="button" onclick={() => void moveAgent()} disabled={!selectedAgentToMove} class="rounded-md bg-primary px-3 text-xs text-primary-foreground disabled:opacity-40">Move here</button></div><a href="/setup?mode=add-session&amp;project_id={encodeURIComponent(projectId)}" class="block rounded-md border border-dashed border-border px-3 py-2 text-center text-xs text-muted-foreground hover:border-primary/40 hover:text-foreground">Create a new Agent in this Project</a></div>{/if}
					<div class="space-y-2">{#each projectAgents as agent (agent.id)}<a href="/agents/{encodeURIComponent(agent.id)}" class="flex items-center gap-3 rounded-xl border border-border bg-card p-3 hover:border-primary/40"><span class="flex h-9 w-9 items-center justify-center rounded-xl bg-muted text-xs font-semibold">{harnessMark(agent.backend)}</span><span class="min-w-0 flex-1"><span class="block truncate text-sm font-medium">{agent.title || agent.name}</span><span class="block truncate text-[11px] text-muted-foreground">{agentRuntimeSummary(agent)}</span></span><span class="h-2 w-2 rounded-full {agent.status === 'running' ? 'bg-blue-500 animate-pulse' : agent.status === 'error' ? 'bg-red-500' : 'bg-emerald-500'}"></span></a>{/each}{#if projectAgents.length === 0}<a href="/setup?mode=add-session&amp;project_id={encodeURIComponent(projectId)}" class="block rounded-xl border border-dashed border-border p-5 text-center text-sm text-muted-foreground">Create an Agent in this Project</a>{/if}</div>
				</section>
			</div>
		</div>
	{:else}
		<div class="p-6 text-sm text-destructive">{error || 'Project not found.'}</div>
	{/if}
</div>
