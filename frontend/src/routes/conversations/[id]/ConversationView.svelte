<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import yaml from 'js-yaml';
	import { agents, conversations, projects, workflows, type Agent, type Conversation, type ConversationMessage, type ConversationMessageUpload, type ConversationTurn, type Project, type Task, type Workflow } from '$lib/api';
	import { clearComposerDraft, loadComposerDraft, saveComposerDraft } from '$lib/composerDrafts';
	import { clipboardFiles, imageDataUrl, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';
	import { PROJECT_MUTATION_EVENT, type ProjectMutation } from '$lib/projectEvents';
	import { harnessMark, timeAgo } from '$lib/utils';
	import AgentLoading from '$lib/components/AgentLoading.svelte';
	import AiMessage from '$lib/components/AiMessage.svelte';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';

	const MESSAGE_PAGE_SIZE = 80;

	let { conversationId }: { conversationId: string } = $props();
	let draftScope = $derived(`conversation-${conversationId}`);
	let conversation = $state<Conversation | null>(null);
	let project = $state<Project | null>(null);
	let agentList = $state<Agent[]>([]);
	let taskList = $state<Task[]>([]);
	let workflowList = $state<Workflow[]>([]);
	let messages = $state<ConversationMessage[]>([]);
	let hasOlderMessages = $state(false);
	let loadingOlderMessages = $state(false);
	let olderMessagesLoaded = $state(false);
	let turns = $state<ConversationTurn[]>([]);
	let loading = $state(true);
	let sending = $state(false);
	let draftReady = $state(false);
	let composing = $state(false);
	let content = $state('');
	let attachments = $state<ConversationMessageUpload[]>([]);
	let error = $state('');
	let attachmentError = $state('');
	let viewMode = $state<'conversation' | 'files'>('conversation');
	let showPeople = $state(false);
	let showTaskComposer = $state(false);
	let taskTitle = $state('');
	let taskDescription = $state('');
	let taskAgent = $state('');
	let taskWorkflow = $state('');
	let workflowValues = $state<Record<string, string | boolean | number>>({});
	let fileInput = $state<HTMLInputElement>();
	let composerInput = $state<HTMLTextAreaElement>();
	let messagePane = $state<HTMLDivElement>();
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let eventSource: EventSource | null = null;
	let stopInitialScrollPin: (() => void) | null = null;
	let projectMutationVersion = 0;
	const projectMutations = new Map<string, ProjectMutation>();

	let projectAgents = $derived(agentList.filter((agent) => agent.project_id === conversation?.project_id));
	let participantAgentIds = $derived(conversation?.participants.filter((participant) => participant.participant_type === 'agent').map((participant) => participant.participant_id) ?? []);
	let participantAgents = $derived(projectAgents.filter((agent) => participantAgentIds.includes(agent.id)));
	let availableAgents = $derived(projectAgents.filter((agent) => !participantAgentIds.includes(agent.id)));
	let activeTurns = $derived(turns.filter((turn) => turn.status === 'queued' || turn.status === 'running'));
	let allFiles = $derived(messages.flatMap((message) => (message.attachments ?? []).map((attachment) => ({ attachment, message }))));
	let imageAttachmentPreviews = $derived(attachments.flatMap((attachment, attachmentIndex) =>
		attachment.mime_type.startsWith('image/')
			? [{ name: attachment.name, src: imageDataUrl(attachment), attachmentIndex }]
			: []
	));
	let otherAttachmentChips = $derived(attachments.flatMap((attachment, attachmentIndex) =>
		attachment.mime_type.startsWith('image/') ? [] : [{ attachment, attachmentIndex }]
	));
	let workflowDefinition = $derived(parseWorkflow(workflowList.find((workflow) => workflow.id === taskWorkflow)));
	let workflowInputs = $derived(Object.entries(workflowDefinition?.inputs ?? {}));

	interface WorkflowInput {
		type?: 'string' | 'number' | 'boolean' | 'agent';
		required?: boolean;
		default?: string | number | boolean;
		description?: string;
	}
	interface WorkflowSummary { inputs?: Record<string, WorkflowInput> }

	$effect(() => { if (draftReady) saveComposerDraft(draftScope, content); });

	onMount(() => {
		window.addEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
		content = loadComposerDraft(draftScope);
		draftReady = true;
		void loadAll(true);
		connectEvents();
		pollTimer = setInterval(() => void refreshActivity(), 2500);
		return () => window.removeEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		eventSource?.close();
		stopInitialScrollPin?.();
	});

	function parseWorkflow(workflow: Workflow | undefined): WorkflowSummary | null {
		if (!workflow) return null;
		try { return yaml.load(workflow.yaml_content) as WorkflowSummary; } catch { return null; }
	}

	function turnAgentName(turn: ConversationTurn): string {
		return projectAgents.find((agent) => agent.id === turn.agent_id)?.title || turn.agent_id;
	}

	function responseStartedAt(turn: ConversationTurn): string | null {
		// Older servers do not expose the explicit response timestamp, so retain
		// their legacy running anchor without conflating preparation on new APIs.
		return turn.response_started_at === undefined ? turn.started_at : turn.response_started_at;
	}

	async function loadAll(scroll = false) {
		let loaded = false;
		try {
			conversation = await conversations.get(conversationId);
			const projectMutationVersionAtStart = projectMutationVersion;
			const [nextProject, nextAgents, nextTasks, nextWorkflows, nextMessages, nextTurns] = await Promise.all([
				conversation.project_id ? projects.get(conversation.project_id) : Promise.resolve(null),
				agents.list(),
				conversations.tasks(conversationId),
				workflows.list(),
				conversations.messages(conversationId, MESSAGE_PAGE_SIZE),
				conversations.turns(conversationId),
			]);
			if (projectMutationVersion === projectMutationVersionAtStart) {
				const pendingMutation = conversation.project_id ? projectMutations.get(conversation.project_id) : undefined;
				project = pendingMutation ? applyProjectMutation(nextProject, pendingMutation) : nextProject;
			}
			agentList = nextAgents;
			taskList = nextTasks;
			workflowList = nextWorkflows;
			messages = nextMessages;
			turns = nextTurns;
			hasOlderMessages = nextMessages.length === MESSAGE_PAGE_SIZE;
			if (!taskAgent) taskAgent = projectAgents[0]?.id ?? '';
			loaded = true;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not load the conversation.';
		} finally {
			loading = false;
		}
		if (scroll && loaded) await scrollInitialHistoryToLatest();
	}

	function handleProjectMutation(event: Event) {
		const mutation = (event as CustomEvent<ProjectMutation>).detail;
		if (!mutation || (mutation.kind !== 'updated' && mutation.kind !== 'deleted')) return;
		const projectId = mutation.kind === 'updated' ? mutation.project.id : mutation.projectId;
		projectMutations.set(projectId, mutation);
		if (conversation?.project_id !== projectId) return;
		projectMutationVersion += 1;
		project = applyProjectMutation(project, mutation);
	}

	function applyProjectMutation(currentProject: Project | null, mutation: ProjectMutation): Project | null {
		if (mutation.kind === 'deleted') return null;
		if (mutation.authoritative || !currentProject || currentProject.updated_at < mutation.project.updated_at) {
			return mutation.project;
		}
		return currentProject;
	}

	async function refreshActivity() {
		try {
			const [nextMessages, nextTurns, nextTasks] = await Promise.all([
				conversations.messages(conversationId, MESSAGE_PAGE_SIZE),
				conversations.turns(conversationId),
				conversations.tasks(conversationId),
			]);
			if (olderMessagesLoaded) messages = mergeMessages(messages, nextMessages);
			else {
				messages = nextMessages;
				hasOlderMessages = nextMessages.length === MESSAGE_PAGE_SIZE;
			}
			turns = nextTurns;
			taskList = nextTasks;
		} catch {}
	}

	function mergeMessages(existing: ConversationMessage[], incoming: ConversationMessage[]): ConversationMessage[] {
		const byId = new Map(existing.map((message) => [message.id, message]));
		for (const message of incoming) byId.set(message.id, message);
		return [...byId.values()].sort((left, right) => left.id - right.id);
	}

	function afterRender(): Promise<void> {
		return new Promise((resolve) => requestAnimationFrame(() => resolve()));
	}

	async function loadOlderMessages() {
		const beforeId = messages[0]?.id;
		if (loadingOlderMessages || !hasOlderMessages || beforeId === undefined) return;
		loadingOlderMessages = true;
		error = '';
		const previousHeight = messagePane?.scrollHeight ?? 0;
		const previousTop = messagePane?.scrollTop ?? 0;
		try {
			const older = await conversations.messages(conversationId, MESSAGE_PAGE_SIZE, beforeId);
			messages = mergeMessages(older, messages);
			olderMessagesLoaded = true;
			hasOlderMessages = older.length === MESSAGE_PAGE_SIZE;
			await afterRender();
			if (messagePane) messagePane.scrollTop = previousTop + messagePane.scrollHeight - previousHeight;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not load earlier messages.';
		} finally {
			loadingOlderMessages = false;
		}
	}

	function connectEvents() {
		eventSource = new EventSource(`/api/conversations/${encodeURIComponent(conversationId)}/events`);
		eventSource.onmessage = () => void refreshActivity().then(() => scrollToLatest());
		eventSource.onerror = () => {
			// EventSource hides the handshake status. Probe one protected route so
			// the shared API client can route an expired session to login.
			void conversations.get(conversationId).catch(() => undefined);
		};
	}

	async function scrollToLatest(behavior: ScrollBehavior = 'smooth') {
		await afterRender();
		messagePane?.scrollTo({ top: messagePane.scrollHeight, behavior });
	}

	async function scrollInitialHistoryToLatest() {
		await afterRender();
		if (!messagePane) return;
		const pane: HTMLDivElement = messagePane;

		let pinned = true;
		const pinToLatest = () => {
			if (pinned && messagePane === pane) pane.scrollTo({ top: pane.scrollHeight, behavior: 'auto' });
		};
		const content = pane.firstElementChild;
		const resizeObserver = new ResizeObserver(pinToLatest);
		if (content) resizeObserver.observe(content);
		let cancelMediaWait: (() => void) | null = null;
		function stopPinning() {
			cleanup();
		}
		function cleanup() {
			pinned = false;
			resizeObserver.disconnect();
			pane.removeEventListener('wheel', stopPinning);
			pane.removeEventListener('touchstart', stopPinning);
			pane.removeEventListener('pointerdown', stopPinning);
			cancelMediaWait?.();
			cancelMediaWait = null;
			if (stopInitialScrollPin === cleanup) stopInitialScrollPin = null;
		}
		stopInitialScrollPin?.();
		stopInitialScrollPin = cleanup;
		pane.addEventListener('wheel', stopPinning, { passive: true });
		pane.addEventListener('touchstart', stopPinning, { passive: true });
		pane.addEventListener('pointerdown', stopPinning, { passive: true });
		pinToLatest();

		const images = Array.from(pane.querySelectorAll('img'));
		if (images.length > 0) {
			await Promise.race([
				Promise.allSettled(images.map((image) => image.decode())),
				new Promise<void>((resolve) => {
					cancelMediaWait = resolve;
				}),
			]);
			cancelMediaWait = null;
			await afterRender();
			pinToLatest();
		}
		cleanup();
	}

	async function send() {
		if (sending || (!content.trim() && attachments.length === 0)) return;
		sending = true;
		error = '';
		try {
			await conversations.sendMessage(conversationId, content.trim(), attachments);
			content = '';
			attachments = [];
			attachmentError = '';
			clearComposerDraft(draftScope);
			await refreshActivity();
			await scrollToLatest();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not send the message.';
		} finally {
			sending = false;
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && !composing && event.keyCode !== 229) {
			event.preventDefault();
			void send();
		}
	}

	function mention(agent: Agent) {
		const prefix = content && !content.endsWith(' ') ? ' ' : '';
		content += `${prefix}@[AGENT:${agent.id}:${agent.title || agent.name}] `;
	}

	function handleSelectionAction(action: string, selection: string) {
		const prompts: Record<string, string> = {
			Explain: 'Explain this passage',
			Improve: 'Improve this passage',
			Shorten: 'Shorten this passage',
			Tone: 'Adjust the tone of this passage',
			Grammar: 'Correct the grammar in this passage',
		};
		const selectionPrompt = `${prompts[action] ?? action}:\n\n> ${selection.replaceAll('\n', '\n> ')}`;
		content = content.trim() ? `${content.trimEnd()}\n\n${selectionPrompt}` : selectionPrompt;
		setTimeout(() => composerInput?.focus(), 0);
	}

	async function addFiles(files: File[]) {
		try {
			const currentSize = attachments.reduce((sum, attachment) => sum + Math.floor(attachment.data.length * 3 / 4), 0);
			const addedSize = files.reduce((sum, file) => sum + file.size, 0);
			if (attachments.length + files.length > 10) throw new Error('A message can include up to 10 files.');
			if (currentSize + addedSize > 20 * 1024 * 1024) throw new Error('Files in one message cannot exceed 20 MiB.');
			attachments = [...attachments, ...await Promise.all(files.map(fileUpload))];
			attachmentError = '';
		} catch (cause) {
			attachmentError = cause instanceof Error ? cause.message : 'Could not attach the files.';
		}
	}

	function removeAttachment(index: number) {
		attachments = attachments.filter((_, itemIndex) => itemIndex !== index);
		attachmentError = '';
	}

	function fileUpload(file: File): Promise<ConversationMessageUpload> {
		return new Promise((resolve, reject) => {
			const reader = new FileReader();
			reader.onerror = () => reject(new Error(`Could not read ${file.name}.`));
			reader.onload = () => {
				const encoded = typeof reader.result === 'string' ? reader.result.split(',', 2)[1] : '';
				if (!encoded) reject(new Error(`Could not encode ${file.name}.`));
				else resolve({ name: file.name, mime_type: file.type || 'application/octet-stream', data: encoded });
			};
			reader.readAsDataURL(file);
		});
	}

	function handleFileInput(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		void addFiles(Array.from(input.files ?? [])).finally(() => (input.value = ''));
	}

	function handlePaste(event: ClipboardEvent) {
		const files = clipboardFiles(event);
		if (files.length > 0) {
			event.preventDefault();
			void addFiles(files);
			return;
		}
		if (!shouldHandleImagePaste(event)) return;
		event.preventDefault();
		void pastedImageFiles(event)
			.then(addFiles)
			.catch((cause) => (attachmentError = cause instanceof Error ? cause.message : 'Could not attach the clipboard image.'));
	}

	async function toggleParticipant(agent: Agent) {
		try {
			if (participantAgentIds.includes(agent.id)) await conversations.removeParticipant(conversationId, agent.id);
			else await conversations.addParticipant(conversationId, agent.id);
			conversation = await conversations.get(conversationId);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not update conversation members.';
		}
	}

	function selectWorkflow(id: string) {
		taskWorkflow = id;
		workflowValues = {};
		const definition = parseWorkflow(workflowList.find((workflow) => workflow.id === id));
		for (const [name, input] of Object.entries(definition?.inputs ?? {})) {
			if (input.default !== undefined) workflowValues[name] = input.default;
			else if (input.type === 'agent') workflowValues[name] = projectAgents[0]?.id ?? '';
			else if (name === 'goal') workflowValues[name] = taskDescription || taskTitle;
			else if (input.type === 'boolean') workflowValues[name] = false;
			else workflowValues[name] = '';
		}
	}

	function workflowReady(): boolean {
		return workflowInputs.every(([name, input]) => !input.required || workflowValues[name] !== '' && workflowValues[name] !== undefined);
	}

	async function createTask() {
		if (!taskTitle.trim()) return;
		error = '';
		try {
			if (taskWorkflow) {
				const inputs = { ...workflowValues };
				if ('goal' in (workflowDefinition?.inputs ?? {}) && !String(inputs.goal ?? '').trim()) inputs.goal = taskDescription.trim() || taskTitle.trim();
				await conversations.createTask(conversationId, { title: taskTitle.trim(), workflow_id: taskWorkflow, workflow_inputs: inputs });
			} else {
				await conversations.createTask(conversationId, { title: taskTitle.trim(), description: taskDescription.trim() || undefined, agent_id: taskAgent || undefined });
			}
			taskTitle = '';
			taskDescription = '';
			taskWorkflow = '';
			workflowValues = {};
			showTaskComposer = false;
			await refreshActivity();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not create the work.';
		}
	}

	function statusDot(status: string): string {
		if (status === 'failed' || status === 'blocked') return 'bg-red-500';
		if (status === 'in_progress' || status === 'running') return 'bg-blue-500 animate-pulse';
		if (status === 'pending' || status === 'queued') return 'bg-amber-400';
		return 'bg-emerald-500';
	}

	function formatBytes(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background">
	{#if loading}
		<div class="flex flex-1 items-center justify-center"><AgentLoading label="Loading conversation" phase="loading" /></div>
	{:else if conversation}
		<header class="shrink-0 border-b border-border bg-card/30 px-4 pt-3 sm:px-6">
			<div class="flex flex-wrap items-center justify-between gap-3">
				<div class="min-w-0"><p class="truncate text-xs text-muted-foreground"><a href="/projects/{encodeURIComponent(project?.id || '')}" class="hover:text-foreground">{project?.name || 'Project'}</a> / Conversation</p><h1 class="mt-1 truncate text-lg font-semibold"><span class="mr-2 text-muted-foreground">#</span>{conversation.title || 'Untitled conversation'}</h1></div>
				<div class="relative flex items-center gap-2"><div class="hidden -space-x-2 sm:flex">{#each participantAgents.slice(0, 5) as agent}<span title={agent.title || agent.name} class="flex h-8 w-8 items-center justify-center rounded-full border-2 border-background bg-muted text-[10px] font-semibold">{harnessMark(agent.backend)}</span>{/each}</div><button type="button" onclick={() => (showPeople = !showPeople)} class="rounded-lg border border-border px-3 py-1.5 text-xs">{participantAgents.length} Agent{participantAgents.length === 1 ? '' : 's'}</button><button type="button" onclick={() => (showTaskComposer = true)} class="rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground">Continue with task</button>
					{#if showPeople}<div class="absolute right-0 top-10 z-30 w-72 rounded-xl border border-border bg-card p-3 shadow-xl"><p class="mb-2 text-xs font-semibold">Conversation Agents</p><div class="space-y-1">{#each projectAgents as agent}<button type="button" onclick={() => void toggleParticipant(agent)} class="flex w-full items-center gap-2 rounded-lg px-2 py-2 text-left text-xs hover:bg-accent"><span class="flex h-7 w-7 items-center justify-center rounded-full bg-muted text-[9px]">{harnessMark(agent.backend)}</span><span class="min-w-0 flex-1 truncate">{agent.title || agent.name}</span><span class="text-primary">{participantAgentIds.includes(agent.id) ? '✓' : '+'}</span></button>{/each}</div>{#if availableAgents.length === 0}<a href="/projects/{encodeURIComponent(project?.id || '')}" class="mt-2 block text-xs text-primary">Manage project Agents</a>{/if}</div>{/if}
				</div>
			</div>
			<nav class="mt-3 flex gap-5 text-xs"><button type="button" onclick={() => (viewMode = 'conversation')} class="border-b-2 px-1 pb-2 {viewMode === 'conversation' ? 'border-primary text-foreground' : 'border-transparent text-muted-foreground'}">Conversation</button><button type="button" onclick={() => (viewMode = 'files')} class="border-b-2 px-1 pb-2 {viewMode === 'files' ? 'border-primary text-foreground' : 'border-transparent text-muted-foreground'}">Files <span class="ml-1 rounded-full bg-muted px-1.5">{allFiles.length}</span></button></nav>
		</header>

		{#if error}<div class="mx-4 mt-3 shrink-0 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive sm:mx-6">{error}</div>{/if}

		{#if viewMode === 'files'}
			<div class="workspace-scroll-y flex-1 p-4 sm:p-6"><div class="mx-auto max-w-4xl"><h2 class="font-semibold">Published files</h2><p class="mb-4 text-sm text-muted-foreground">Files shared by people and Agents in this conversation.</p>{#if hasOlderMessages}<button type="button" onclick={() => void loadOlderMessages()} disabled={loadingOlderMessages} class="mb-4 rounded-lg border border-border px-3 py-2 text-xs text-muted-foreground hover:text-foreground disabled:opacity-50">{loadingOlderMessages ? 'Loading…' : 'Load files from earlier messages'}</button>{/if}{#if allFiles.length === 0}<div class="rounded-xl border border-dashed border-border p-10 text-center text-sm text-muted-foreground">No files have been published yet.</div>{:else}<div class="grid gap-3 sm:grid-cols-2">{#each allFiles as item (item.attachment.id)}<a href={conversations.attachmentUrl(conversationId, item.attachment.id)} target="_blank" rel="noopener noreferrer" class="flex items-center gap-3 rounded-xl border border-border bg-card p-4 hover:border-primary/40"><span class="flex h-10 w-10 items-center justify-center rounded-lg bg-muted text-lg">{item.attachment.mime_type.startsWith('image/') ? '▧' : '↧'}</span><span class="min-w-0 flex-1"><span class="block truncate text-sm font-medium">{item.attachment.name}</span><span class="text-[11px] text-muted-foreground">{formatBytes(item.attachment.size)} · {item.message.sender_name || item.message.sender_id}</span></span></a>{/each}</div>{/if}</div></div>
		{:else}
			<div class="flex min-h-0 flex-1">
				<div class="flex min-w-0 flex-1 flex-col">
					<div bind:this={messagePane} data-conversation-message-pane class="workspace-scroll-y flex-1 px-4 py-5 sm:px-6"><div class="mx-auto max-w-4xl space-y-5">
						{#if hasOlderMessages}<div class="flex justify-center"><button type="button" onclick={() => void loadOlderMessages()} disabled={loadingOlderMessages} class="rounded-full border border-border bg-card px-4 py-2 text-xs text-muted-foreground hover:text-foreground disabled:opacity-50">{loadingOlderMessages ? 'Loading earlier messages…' : 'Load earlier messages'}</button></div>{/if}
						{#if messages.length === 0}<div class="rounded-xl border border-dashed border-border p-10 text-center"><h2 class="font-medium">Start the conversation</h2><p class="mt-2 text-sm text-muted-foreground">Every participating Agent can answer, even while its task lane is busy.</p></div>{/if}
						{#each messages as message (message.id)}
							{@const fromUser = message.sender_type === 'user'}
							{@const linkedTask = taskList.find((task) => task.id === message.linked_task_id)}
							<AiMessage
								role={fromUser ? 'user' : 'assistant'}
								sender={message.sender_name || message.sender_id}
								timestampLabel={timeAgo(message.created_at)}
								content={message.content}
								avatar={fromUser ? 'Y' : message.sender_name?.slice(0, 1).toUpperCase() || 'A'}
								selectionActions={!fromUser}
								onselectionaction={handleSelectionAction}
							>
								{#if linkedTask}
									<a href="/tasks/{linkedTask.id}" class="mt-3 flex items-center gap-2 rounded-lg bg-background/40 px-3 py-2 text-xs shadow-[var(--shadow-hairline)]">
										<span class="h-2 w-2 rounded-full {statusDot(linkedTask.status)}"></span>
										<span class="min-w-0 flex-1 truncate">{linkedTask.title}</span>
										<span class="text-muted-foreground">{linkedTask.status.replaceAll('_', ' ')}</span>
									</a>
								{/if}
								{#if message.attachments?.length}
									<div class="mt-3 grid gap-2 sm:grid-cols-2">
										{#each message.attachments as attachment}
											<a href={conversations.attachmentUrl(conversationId, attachment.id)} target="_blank" rel="noopener noreferrer" class="overflow-hidden rounded-lg bg-background/40 shadow-[var(--shadow-hairline)]">
												{#if attachment.mime_type.startsWith('image/')}<img src={conversations.attachmentUrl(conversationId, attachment.id)} alt={attachment.name} class="max-h-48 w-full object-cover" />{/if}
												<span class="flex items-center justify-between gap-2 px-2 py-1.5 text-[11px]"><span class="truncate">{attachment.name}</span><span class="opacity-70">{formatBytes(attachment.size)}</span></span>
											</a>
										{/each}
									</div>
								{/if}
							</AiMessage>
						{/each}
						{#if activeTurns.length}
							<div class="space-y-2 py-2 pl-10">
								{#each activeTurns as turn (turn.id)}
									{@const responseStart = responseStartedAt(turn)}
									<AgentLoading
										label={turn.status === 'queued'
											? `${turnAgentName(turn)} is queued to respond`
											: responseStart
												? `${turnAgentName(turn)} is responding`
												: `Preparing ${turnAgentName(turn)}`}
										phase={turn.status === 'queued' ? 'queued' : responseStart ? 'active' : 'preparing'}
										startedAt={turn.status === 'queued'
											? turn.response_queued_at ?? turn.queued_at
											: responseStart ?? turn.started_at}
									/>
								{/each}
							</div>
						{/if}
					</div></div>

					<div class="shrink-0 border-t border-border bg-background p-3 sm:p-4">
						<div data-conversation-composer class="ai-card mx-auto max-w-4xl focus-within:ring-1 focus-within:ring-ring/40">
							<div class="flex flex-wrap gap-1 px-3 pt-2">
								{#each participantAgents as agent}
									<button type="button" onclick={() => mention(agent)} class="rounded-full bg-muted px-2 py-1 text-[10px] text-muted-foreground hover:text-foreground">@{agent.title || agent.name}</button>
								{/each}
							</div>
							<ImageAttachmentPreviews
								attachments={imageAttachmentPreviews}
								onremove={(previewIndex) => removeAttachment(imageAttachmentPreviews[previewIndex].attachmentIndex)}
							/>
							{#if otherAttachmentChips.length}
								<div class="flex flex-wrap gap-2 px-3 pb-2">
									{#each otherAttachmentChips as item}
										<span class="flex max-w-48 items-center gap-2 rounded-lg bg-muted px-2 py-1 text-[11px] shadow-[var(--shadow-hairline)]">
											<span class="truncate">{item.attachment.name}</span>
											<button type="button" aria-label="Remove {item.attachment.name}" onclick={() => removeAttachment(item.attachmentIndex)}>×</button>
										</span>
									{/each}
								</div>
							{/if}
							<textarea bind:this={composerInput} bind:value={content} onkeydown={handleKeydown} onpaste={handlePaste} oncompositionstart={() => (composing = true)} oncompositionend={() => (composing = false)} rows="3" placeholder="Message #{conversation.title || 'conversation'}…" class="block max-h-36 w-full resize-none bg-transparent px-3.5 py-2.5 text-sm outline-none"></textarea>
							{#if attachmentError}<div class="px-3.5 pb-2 text-xs text-destructive" role="alert">{attachmentError}</div>{/if}
							<div class="flex items-center justify-between px-2 pb-2">
								<div>
									<input bind:this={fileInput} data-conversation-attachment-input onchange={handleFileInput} type="file" multiple class="hidden" />
									<button type="button" onclick={() => fileInput?.click()} class="ai-icon-button" title="Attach files" aria-label="Attach files"><svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg></button>
								</div>
								<button type="button" onclick={() => void send()} disabled={sending || !content.trim() && attachments.length === 0} class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary text-primary-foreground transition-transform enabled:active:scale-95 disabled:opacity-35" aria-label="Send">{#if sending}<span class="h-3 w-3 animate-spin rounded-full border-2 border-current border-r-transparent"></span>{:else}<svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path stroke-linecap="round" stroke-linejoin="round" d="M12 19V5M5 12l7-7 7 7" /></svg>{/if}</button>
							</div>
						</div>
					</div>
				</div>

				<aside class="hidden w-72 shrink-0 overflow-y-auto border-l border-border bg-card/20 p-4 xl:block"><div class="mb-3 flex items-center justify-between"><h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Tasks</h2><button type="button" onclick={() => (showTaskComposer = true)} class="text-xs text-primary">+ Add</button></div><div class="space-y-2">{#each taskList.slice(0, 10) as task}<a href="/tasks/{task.id}" class="block rounded-lg border border-border bg-card p-3 hover:border-primary/40"><div class="flex items-start gap-2"><span class="mt-1 h-2 w-2 shrink-0 rounded-full {statusDot(task.status)}"></span><span class="min-w-0 flex-1"><span class="line-clamp-2 text-xs font-medium">{task.title}</span><span class="mt-1 block text-[10px] text-muted-foreground">{task.status.replaceAll('_', ' ')} · {timeAgo(task.updated_at)}</span></span></div></a>{/each}{#if taskList.length === 0}<p class="rounded-lg border border-dashed border-border p-4 text-center text-xs text-muted-foreground">No tasks yet.</p>{/if}</div></aside>
			</div>
		{/if}
	{:else}
		<div class="p-6 text-sm text-destructive">{error || 'Conversation not found.'}</div>
	{/if}
</div>

{#if showTaskComposer}
	<div class="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 p-4" role="presentation" onclick={(event) => { if (event.currentTarget === event.target) showTaskComposer = false; }}>
		<form onsubmit={(event) => { event.preventDefault(); void createTask(); }} class="max-h-[90dvh] w-full max-w-lg space-y-4 overflow-y-auto rounded-xl border border-border bg-card p-5 shadow-2xl">
			<div class="flex items-center justify-between"><div><h2 class="font-semibold">Continue with work</h2><p class="text-xs text-muted-foreground">Create one Agent task or run a reusable workflow in this conversation.</p></div><button type="button" onclick={() => (showTaskComposer = false)} class="text-xl text-muted-foreground">×</button></div>
			<div><label for="task-title" class="text-xs font-medium">Title</label><input id="task-title" bind:value={taskTitle} class="mt-1 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm" placeholder="What needs to be done?" /></div>
			<div><label for="task-description" class="text-xs font-medium">Details</label><textarea id="task-description" bind:value={taskDescription} rows="3" class="mt-1 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm" placeholder="Context, constraints, or expected outcome"></textarea></div>
			<div class="grid gap-3 sm:grid-cols-2"><div><label for="task-workflow" class="text-xs font-medium">Workflow</label><select id="task-workflow" value={taskWorkflow} onchange={(event) => selectWorkflow(event.currentTarget.value)} class="mt-1 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"><option value="">No workflow</option>{#each workflowList as workflow}<option value={workflow.id}>{workflow.name}</option>{/each}</select></div>{#if !taskWorkflow}<div><label for="task-agent" class="text-xs font-medium">Agent</label><select id="task-agent" bind:value={taskAgent} class="mt-1 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"><option value="">Unassigned</option>{#each projectAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}</select></div>{/if}</div>
			{#if taskWorkflow && workflowInputs.length}<div class="space-y-3 rounded-lg border border-border bg-background/50 p-3"><p class="text-xs font-semibold">Workflow inputs</p>{#each workflowInputs as [name, input]}<label class="block text-xs"><span class="mb-1 block font-medium">{name}{input.required ? ' *' : ''}</span>{#if input.type === 'agent'}<select value={String(workflowValues[name] ?? '')} onchange={(event) => (workflowValues[name] = event.currentTarget.value)} class="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"><option value="">Choose an Agent</option>{#each projectAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}</select>{:else if input.type === 'boolean'}<input type="checkbox" checked={Boolean(workflowValues[name])} onchange={(event) => (workflowValues[name] = event.currentTarget.checked)} />{:else}<input type={input.type === 'number' ? 'number' : 'text'} value={String(workflowValues[name] ?? '')} oninput={(event) => (workflowValues[name] = input.type === 'number' ? Number(event.currentTarget.value) : event.currentTarget.value)} class="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm" placeholder={input.description || name} />{/if}</label>{/each}</div>{/if}
			<div class="flex justify-end gap-2"><button type="button" onclick={() => (showTaskComposer = false)} class="rounded-lg border border-border px-3 py-2 text-sm">Cancel</button><button disabled={!taskTitle.trim() || taskWorkflow !== '' && !workflowReady()} class="rounded-lg bg-primary px-4 py-2 text-sm text-primary-foreground disabled:opacity-40">{taskWorkflow ? 'Run workflow' : 'Create task'}</button></div>
		</form>
	</div>
{/if}
