<script lang="ts">
	import { page } from '$app/stores';
	import { onMount, tick, untrack } from 'svelte';
	import { conversations, agents, budget, agentHarness } from '$lib/api';
	import type { Conversation, ConversationMessage, Agent, AgentBudgetState, AgentTmuxStatus } from '$lib/api';
	import { timeAgo, agentAvatar, getCachedProfile, setCachedProfile, isProfileLoaded } from '$lib/utils';
	import { settings } from '$lib/api';
	import { renderContent } from '$lib/formatMessage';

	let conv = $state<Conversation | null>(null);
	let messages = $state<ConversationMessage[]>([]);
	let agentList = $state<Agent[]>([]);
	let input = $state('');
	let sending = $state(false);
	let error = $state<string | null>(null);
	let messagesEl: HTMLDivElement;
	let showMentionPicker = $state(false);
	let mentionQuery = $state('');
	let editingTitle = $state(false);
	let titleInput = $state('');
	let thinkingAgent = $state<string | null>(null);
	let streamingContent = $state('');
	let cancelStream = $state<(() => void) | null>(null);
	let stoppedAgents = $state<Agent[]>([]);
	let showStartDialog = $state(false);
	let startingAgents = $state(false);
	let userProfile = $state(getCachedProfile());
	let pendingMsgHandled = false;
	let composing = $state(false);

	// ADR-023 §6 task 9: per-agent budget state (so we can show the
	// "running on local (budget)" chip) + tmux availability (for the
	// terminal-attach button).
	let primaryBudget = $state<AgentBudgetState | null>(null);
	let primaryTmux = $state<AgentTmuxStatus | null>(null);

	// Derive just the ID string so the $effect below only re-runs
	// when the actual ID changes, not on every $page store emission.
	let convId = $derived($page.params.id);

	onMount(async () => {
		if (!isProfileLoaded()) {
			const p = await settings.getProfile().catch(() => null);
			if (p) {
				setCachedProfile(p);
				userProfile = p;
			}
		}
	});

	let participantAgents = $derived(
		conv?.participants.filter(p => p.participant_type === 'agent').map(p => p.participant_id) ?? []
	);

	let filteredAgents = $derived(
		agentList.filter(a =>
			participantAgents.includes(a.id) &&
			a.name.toLowerCase().includes(mentionQuery.toLowerCase())
		)
	);

	let primaryAgent = $derived(
		participantAgents.length > 0
			? agentList.find(a => a.id === participantAgents[0])
			: undefined
	);

	// Fetch budget + tmux status for the primary agent. Re-fetches
	// whenever the primary-agent id changes so the header chip updates
	// when the sidecar flips the agent's degraded_model mid-conversation.
	let primaryAgentId = $derived(primaryAgent?.id);
	$effect(() => {
		const id = primaryAgentId;
		if (!id) {
			primaryBudget = null;
			primaryTmux = null;
			return;
		}
		untrack(() => {
			budget.agent(id).then(b => { if (primaryAgentId === id) primaryBudget = b; }).catch(() => {});
			agentHarness.tmux(id).then(t => { if (primaryAgentId === id) primaryTmux = t; }).catch(() => {});
		});
	});

	$effect(() => {
		const id = convId;
		if (id) {
			// Clean up previous subscription without creating a
			// reactive dependency on cancelStream.
			untrack(() => {
				if (cancelStream) {
					cancelStream();
					cancelStream = null;
				}
			});
			load(id);
		}
	});

	async function load(id: string) {
		try {
			const [c, msgs, a] = await Promise.all([
				conversations.get(id),
				conversations.messages(id, 100),
				agents.list().catch(() => [])
			]);
			conv = c;
			messages = msgs;
			agentList = a;
			error = null;

			const participantIds = c.participants
				.filter(p => p.participant_type === 'agent')
				.map(p => p.participant_id);
			// Only prompt for agents the user hasn't already requested to run.
			// When Docker is slow/unavailable, status may show "starting" even
			// though the agent is running fine. Check desired_status to avoid
			// repeatedly prompting on every page navigation. (XCLAW-55)
			stoppedAgents = a.filter(ag =>
				participantIds.includes(ag.id) &&
				ag.status !== 'running' &&
				ag.desired_status !== 'running'
			);
			if (stoppedAgents.length > 0) {
				showStartDialog = true;
			}

			await tick();
			scrollToBottom();

			// Subscribe to live events (ADR-019)
			const sseReady = subscribeToEvents();

			// Handle ?msg= query param exactly once (e.g. from "new conversation" flow).
			// Read from window.location (not $page) and guard with a flag to
			// avoid re-triggering if the effect ever re-runs.
			if (!pendingMsgHandled) {
				const params = new URLSearchParams(window.location.search);
				const pendingMsg = params.get('msg');
				if (pendingMsg) {
					pendingMsgHandled = true;
					window.history.replaceState(window.history.state, '', window.location.pathname);
					// Wait for SSE connection before sending so events aren't missed
					await sseReady;
					input = pendingMsg;
					await tick();
					sendMessage();
				}
			}
		} catch (e) {
			error = String(e);
		}

		return () => {
			// Cleanup SSE subscription on unmount
			if (cancelStream) cancelStream();
		};
	}

	// Subscribe to SSE events for this conversation (ADR-019).
	// Handles thinking, chunks, messages, and errors from the background processor.
	// Always closes any existing subscription first to prevent duplicate streams.
	function subscribeToEvents(): Promise<void> {
		if (!conv) return Promise.resolve();

		// Close any existing subscription — prevents duplicate streams
		// when navigating away and back.
		if (cancelStream) {
			cancelStream();
			cancelStream = null;
		}

		// Reset streaming state
		thinkingAgent = null;
		streamingContent = '';
		sending = false;

		const lastId = messages.length > 0 ? messages[messages.length - 1].id : 0;
		const sub = conversations.subscribeEvents(conv.id, lastId, {
			onThinking: async (agentId) => {
				thinkingAgent = agentId;
				streamingContent = '';
				sending = true;
				await tick();
				scrollToBottom();
			},
			onChunk: async (_agentId, chunk) => {
				streamingContent += chunk;
				await tick();
				scrollToBottom();
			},
			onAgentMessage: async (msg) => {
				thinkingAgent = null;
				streamingContent = '';
				// Avoid duplicates (replay can send messages we already have)
				if (!messages.some(m => m.id === msg.id)) {
					messages = [...messages, msg];
				}
				await tick();
				scrollToBottom();
			},
			onError: (_agentId, err) => {
				error = err;
			},
			onDone: () => {
				sending = false;
				thinkingAgent = null;
				streamingContent = '';
			}
		});
		cancelStream = sub.cancel;
		return sub.ready;
	}

	async function sendMessage() {
		if (!input.trim() || !conv) return;

		const content = input.trim();
		input = '';
		error = null;

		try {
			// Fire-and-forget: store user message, background processor handles the rest
			const result = await conversations.sendMessage(conv.id, content, userProfile.name);
			// Add user message to local list (result is [userMsg])
			if (Array.isArray(result) && result.length > 0) {
				const userMsg = result[0];
				if (!messages.some(m => m.id === userMsg.id)) {
					messages = [...messages, userMsg];
				}
			}
			await tick();
			scrollToBottom();

			// Ensure we're subscribed to events (reconnect if needed)
			if (!cancelStream) {
				subscribeToEvents();
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function stopAgent() {
		if (!conv) return;
		try {
			await conversations.stop(conv.id, thinkingAgent ?? undefined);
			if (cancelStream) {
				cancelStream();
				cancelStream = null;
			}
			sending = false;
			thinkingAgent = null;
			streamingContent = '';
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	function scrollToBottom() {
		if (messagesEl) {
			messagesEl.scrollTop = messagesEl.scrollHeight;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		const imeActive = e.isComposing || composing || e.keyCode === 229;
		if (showMentionPicker) {
			if (e.key === 'Escape') {
				showMentionPicker = false;
				e.preventDefault();
			} else if (e.key === 'Enter' && !imeActive && filteredAgents.length > 0) {
				insertMention(filteredAgents[0].id);
				e.preventDefault();
			}
			return;
		}
		if (e.key === 'Enter' && !e.shiftKey && !imeActive) {
			e.preventDefault();
			sendMessage();
		}
	}

	function handleInput(e: Event) {
		const target = e.target as HTMLTextAreaElement;
		const val = target.value;
		const cursorPos = target.selectionStart;

		const textBefore = val.slice(0, cursorPos);
		const atMatch = textBefore.match(/@(\w*)$/);
		if (atMatch) {
			mentionQuery = atMatch[1];
			showMentionPicker = true;
		} else {
			showMentionPicker = false;
		}
	}

	function insertMention(agentId: string) {
		const textarea = document.querySelector('textarea') as HTMLTextAreaElement;
		if (!textarea) return;

		const cursorPos = textarea.selectionStart;
		const textBefore = input.slice(0, cursorPos);
		const textAfter = input.slice(cursorPos);
		const atMatch = textBefore.match(/@(\w*)$/);

		if (atMatch) {
			const beforeAt = textBefore.slice(0, textBefore.length - atMatch[0].length);
			input = `${beforeAt}@[AGENT:${agentId}:${agentId}] ${textAfter}`;
		}

		showMentionPicker = false;
		textarea.focus();
	}

	function convTitle(): string {
		if (conv?.title) return conv.title;
		return participantAgents.join(', ') || 'Chat';
	}

	async function saveTitle() {
		if (!conv || !titleInput.trim()) return;
		await conversations.update(conv.id, { title: titleInput.trim() });
		conv = { ...conv, title: titleInput.trim() };
		editingTitle = false;
	}

	async function startStoppedAgents() {
		startingAgents = true;
		try {
			await Promise.all(stoppedAgents.map(a => agents.start(a.id)));
			agentList = await agents.list().catch(() => agentList);
			stoppedAgents = [];
			showStartDialog = false;
		} catch (e) {
			error = `Failed to start agents: ${e instanceof Error ? e.message : String(e)}`;
			showStartDialog = false;
		}
		startingAgents = false;
	}

	async function deleteConversation() {
		if (!conv) return;
		if (!confirm('Delete this conversation?')) return;
		await conversations.delete(conv.id);
		window.location.href = '/dashboard';
	}

	function getAgentForMessage(msg: ConversationMessage): Agent | undefined {
		if (msg.sender_type === 'agent') {
			return agentList.find(a => a.id === msg.sender_id);
		}
		return undefined;
	}

	// --- AskUserQuestion support ---
	interface AskQuestion {
		header?: string;
		question: string;
		multiSelect: boolean;
		options: { label: string; description?: string }[];
	}

	function parseAskUserQuestion(content: string): AskQuestion[] | null {
		// Look for AskUserQuestion tool call with JSON payload
		const match = content.match(/<tool_call name="AskUserQuestion">([\s\S]*?)<\/tool_call>/);
		if (!match) return null;
		try {
			const data = JSON.parse(match[1].trim());
			if (data.questions && Array.isArray(data.questions)) {
				return data.questions;
			}
		} catch {}
		return null;
	}

	// Track selections and current page per message
	let questionSelections = $state<Record<string, Record<number, Set<number>>>>({});
	let questionPage = $state<Record<string, number>>({});

	function getPage(msgId: string): number {
		return questionPage[msgId] ?? 0;
	}

	function setPage(msgId: string, page: number) {
		questionPage = { ...questionPage, [msgId]: page };
	}

	function toggleOption(msgId: string, qIdx: number, optIdx: number, multiSelect: boolean) {
		if (!questionSelections[msgId]) questionSelections[msgId] = {};
		if (!questionSelections[msgId][qIdx]) questionSelections[msgId][qIdx] = new Set();

		const sel = questionSelections[msgId][qIdx];
		if (multiSelect) {
			if (sel.has(optIdx)) sel.delete(optIdx);
			else sel.add(optIdx);
		} else {
			sel.clear();
			sel.add(optIdx);
		}
		questionSelections = { ...questionSelections };
	}

	function isSelected(msgId: string, qIdx: number, optIdx: number): boolean {
		return questionSelections[msgId]?.[qIdx]?.has(optIdx) ?? false;
	}

	function getSelectionLabels(msgId: string, qIdx: number, options: { label: string }[]): string[] {
		const sel = questionSelections[msgId]?.[qIdx];
		if (!sel) return [];
		return [...sel].map(i => options[i]?.label).filter(Boolean);
	}

	async function submitQuestionResponse(msgId: string, questions: AskQuestion[]) {
		if (!conv) return;
		const parts: string[] = [];
		for (let qi = 0; qi < questions.length; qi++) {
			const q = questions[qi];
			const labels = getSelectionLabels(msgId, qi, q.options);
			if (labels.length === 0) continue;
			if (q.header) {
				parts.push(`${q.header}: ${labels.join(', ')}`);
			} else {
				parts.push(labels.join(', '));
			}
		}
		if (parts.length === 0) return;
		input = parts.join('\n');
		await sendMessage();
	}

	function stripAskUserQuestion(content: string): string {
		// Remove the AskUserQuestion tool call and its duplicate JSON from the content
		// Often the raw JSON appears twice — once in tool_call tags and once raw
		let result = content.replace(/<tool_call name="AskUserQuestion">[\s\S]*?<\/tool_call>/g, '');
		// Also remove raw JSON blocks that look like AskUserQuestion payloads
		result = result.replace(/\{[\s\S]*?"questions"[\s\S]*?"options"[\s\S]*?\}\s*$/g, '');
		return result.trim();
	}
</script>

{#if error && !conv}
	<div class="flex h-full items-center justify-center">
		<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">
			{error}
		</div>
	</div>
{:else if !conv}
	<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
		Loading...
	</div>
{:else}
	<!-- Start agents dialog -->
	{#if showStartDialog}
		<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
			<div class="mx-4 w-full max-w-sm rounded-xl border border-border bg-card p-5 shadow-2xl space-y-4">
				<h3 class="text-sm font-semibold">Agents are stopped</h3>
				<p class="text-sm text-muted-foreground">
					{#if stoppedAgents.length === 1}
						<span class="font-medium text-foreground">{stoppedAgents[0].name}</span> is not running. Start it to chat?
					{:else}
						The following agents are not running:
						<span class="font-medium text-foreground">{stoppedAgents.map(a => a.name).join(', ')}</span>. Start them to chat?
					{/if}
				</p>
				<div class="flex justify-end gap-2">
					<button
						onclick={() => (showStartDialog = false)}
						class="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-secondary transition-colors"
					>Not now</button>
					<button
						onclick={startStoppedAgents}
						disabled={startingAgents}
						class="rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
					>
						{#if startingAgents}Starting...{:else}Start {stoppedAgents.length === 1 ? stoppedAgents[0].name : 'all'}{/if}
					</button>
				</div>
			</div>
		</div>
	{/if}

	<div class="flex h-full flex-col">
		<!-- Conversation Header -->
		<div class="flex items-center gap-3 border-b border-border px-5 py-3">
			<div class="flex-1 min-w-0">
				{#if editingTitle}
					<form onsubmit={(e) => { e.preventDefault(); saveTitle(); }} class="flex items-center gap-2">
						<input
							type="text"
							bind:value={titleInput}
							class="rounded-lg border border-border bg-secondary px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
							autofocus
						/>
						<button type="submit" class="text-xs text-primary hover:underline">Save</button>
						<button type="button" onclick={() => (editingTitle = false)} class="text-xs text-muted-foreground hover:underline">Cancel</button>
					</form>
				{:else}
					<button
						onclick={() => { editingTitle = true; titleInput = conv?.title ?? ''; }}
						class="text-lg font-semibold hover:text-primary transition-colors text-left"
					>
						{convTitle()}
					</button>
				{/if}
				<div class="flex items-center gap-2 text-xs text-muted-foreground mt-0.5">
					{#each participantAgents as agentId}
						{@const agent = agentList.find(a => a.id === agentId)}
						<span class="inline-flex items-center gap-1.5">
							<span class="h-2 w-2 rounded-full {agent?.status === 'running' ? 'bg-emerald-400' : 'bg-muted-foreground/30'}"></span>
							{agentId}
						</span>
					{/each}
					{#if primaryBudget?.degraded_model}
						<!-- ADR-023 §6: transparent budget downgrade. Make it
						     visible so the user knows why responses look
						     different from what their primary model would produce. -->
						<span
							class="inline-flex items-center gap-1 rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-300"
							title="This agent hit its budget cap; the sidecar has transparently switched to {primaryBudget.degraded_model}."
						>
							<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3 10.5V6.75a.75.75 0 01.75-.75h16.5a.75.75 0 01.75.75v10.5a.75.75 0 01-.75.75H3.75a.75.75 0 01-.75-.75v-1.5M3 10.5h15M3 10.5v4.5"/></svg>
							running on {primaryBudget.degraded_model} (budget)
						</span>
					{/if}
				</div>
			</div>
			<div class="flex items-center gap-1">
				{#if primaryTmux?.available}
					<!-- ADR-023 task 9: attach to the agent's tmux session.
					     Only shown when the harness actually exposes one.
					     xterm.js integration lands alongside the first real
					     tmux-exposing harness. -->
					<button
						onclick={() => alert(`Attach to session "${primaryTmux?.session ?? ''}" — xterm.js integration pending next task iteration.`)}
						class="rounded-lg p-2 text-muted-foreground hover:bg-secondary hover:text-foreground transition-colors"
						title="Attach to agent terminal (tmux)"
					>
						<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z"/></svg>
					</button>
				{/if}
				<a href="/tasks" class="rounded-lg p-2 text-muted-foreground hover:bg-secondary hover:text-foreground transition-colors" title="Tasks">
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M9 12.75L11.25 15 15 9.75M21 12a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
				</a>
				<button
					onclick={deleteConversation}
					class="rounded-lg p-2 text-muted-foreground hover:bg-destructive/10 hover:text-destructive transition-colors"
					title="Delete conversation"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"/></svg>
				</button>
			</div>
		</div>

		<!-- Messages -->
		<div bind:this={messagesEl} class="flex-1 overflow-y-auto px-6 py-5 space-y-5">
			{#each messages as msg (msg.id)}
				{#if msg.message_type === 'task_status'}
					{@const taskData = (() => { try { return JSON.parse(msg.content); } catch { return null; } })()}
					{#if taskData}
						<div class="flex gap-3">
							<div class="flex-shrink-0 h-9 w-9 rounded-full flex items-center justify-center text-xs bg-secondary text-muted-foreground">
								&#x2611;
							</div>
							<div class="flex-1 max-w-[75%]">
								<div class="rounded-xl border px-4 py-3 text-sm
									{taskData.status === 'completed' ? 'border-emerald-500/30 bg-emerald-500/5' :
									 taskData.status === 'failed' ? 'border-red-500/30 bg-red-500/5' :
									 taskData.status === 'in_progress' ? 'border-blue-500/30 bg-blue-500/5' :
									 'border-amber-500/30 bg-amber-500/5'}">
									<div class="flex items-center gap-2">
										<span class="h-2 w-2 rounded-full
											{taskData.status === 'completed' ? 'bg-emerald-400' :
											 taskData.status === 'failed' ? 'bg-red-400' :
											 taskData.status === 'in_progress' ? 'bg-blue-400 animate-pulse' :
											 'bg-amber-400'}"></span>
										<span class="font-medium">{taskData.title}</span>
									</div>
									{#if taskData.subtasks_total > 0}
										<div class="mt-2">
											<div class="flex items-center justify-between text-xs text-muted-foreground mb-1">
												<span>{taskData.subtasks_completed}/{taskData.subtasks_total} subtasks</span>
												<span>{Math.round((taskData.subtasks_completed / taskData.subtasks_total) * 100)}%</span>
											</div>
											<div class="h-1.5 rounded-full bg-muted overflow-hidden">
												<div class="h-full rounded-full transition-all duration-500 {taskData.status === 'completed' ? 'bg-emerald-500' : taskData.status === 'failed' ? 'bg-red-500' : 'bg-blue-500'}"
													style="width: {(taskData.subtasks_completed / taskData.subtasks_total) * 100}%"></div>
											</div>
										</div>
									{/if}
									<div class="text-xs text-muted-foreground mt-1.5 flex items-center gap-2">
										<span>Task {taskData.status === 'in_progress' ? 'in progress' : taskData.status}</span>
										<span>&middot;</span>
										<span>{timeAgo(msg.created_at)}</span>
										<a href="/tasks/{taskData.task_id}" class="underline hover:text-foreground ml-auto">View task</a>
									</div>
								</div>
							</div>
						</div>
					{/if}
				{:else}
				{@const isUser = msg.sender_type === 'user'}
				{@const agent = getAgentForMessage(msg)}
				<div class="flex gap-3 {isUser ? 'flex-row-reverse' : ''}">
					<!-- Avatar -->
					{#if isUser}
						{#if userProfile.avatar}
							<img src={userProfile.avatar} alt="" class="flex-shrink-0 h-9 w-9 rounded-full object-cover" />
						{:else}
							<div class="flex-shrink-0 h-9 w-9 rounded-full flex items-center justify-center text-xs font-bold bg-primary/20 text-primary">
								{userProfile.name[0].toUpperCase()}
							</div>
						{/if}
					{:else if agent}
						<img src={agentAvatar(agent)} alt="" class="flex-shrink-0 h-9 w-9 rounded-full object-cover" />
					{:else}
						<div class="flex-shrink-0 h-9 w-9 rounded-full flex items-center justify-center text-xs font-bold bg-secondary text-foreground">
							{(msg.sender_id ?? '?')[0].toUpperCase()}
						</div>
					{/if}

					<!-- Message bubble -->
					<div class="max-w-[75%] space-y-1">
						{#if !isUser}
							{@const askQuestions = parseAskUserQuestion(msg.content)}
							{@const msgKey = msg.id.toString()}
							<div class="flex items-center gap-2">
								<span class="text-xs font-semibold text-foreground">{agent?.config?.display_name || msg.sender_name || msg.sender_id}</span>
								<span class="text-xs text-muted-foreground">{timeAgo(msg.created_at)}</span>
							</div>

							{@const displayContent = askQuestions ? stripAskUserQuestion(msg.content) : msg.content}
							{#if displayContent.trim()}
								<div class="rounded-2xl px-4 py-2.5 text-sm prose-chat bg-[hsl(var(--bubble-agent))] text-foreground border border-border/50 {msg.message_type === 'system' ? 'italic opacity-70' : ''}">
									{@html renderContent(displayContent)}
								</div>
							{/if}

							{#if askQuestions}
								{@const page = getPage(msgKey)}
								{@const totalPages = askQuestions.length}
								{@const isReviewPage = page >= totalPages}
								<div class="rounded-2xl border border-primary/30 bg-primary/5 px-4 py-3 space-y-3">
									{#if isReviewPage}
										<!-- Review page -->
										<div class="text-xs font-semibold text-primary">Review your answers</div>
										{#each askQuestions as q, qi}
											{@const labels = getSelectionLabels(msgKey, qi, q.options)}
											<div class="space-y-1">
												<div class="text-sm font-medium">{q.header ?? q.question}</div>
												{#if labels.length > 0}
													<div class="text-sm text-primary">{labels.join(', ')}</div>
												{:else}
													<div class="text-sm text-muted-foreground italic">No selection</div>
												{/if}
											</div>
										{/each}
										<div class="flex gap-2 pt-1">
											<button
												onclick={() => setPage(msgKey, totalPages - 1)}
												class="rounded-lg border border-border px-3 py-1.5 text-sm hover:bg-secondary transition-colors"
											>Back</button>
											<button
												onclick={() => submitQuestionResponse(msgKey, askQuestions)}
												disabled={sending}
												class="rounded-lg bg-primary px-4 py-1.5 text-sm text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50"
											>Submit</button>
										</div>
									{:else}
										<!-- Question page -->
										{@const q = askQuestions[page]}
										<div class="flex items-center justify-between">
											{#if q.header}
												<div class="text-xs font-semibold text-primary">{q.header}</div>
											{/if}
											<div class="text-xs text-muted-foreground">{page + 1} / {totalPages}</div>
										</div>
										<div class="text-sm font-medium">{q.question}</div>
										<div class="space-y-1.5">
											{#each q.options as opt, oi}
												<button
													onclick={() => toggleOption(msgKey, page, oi, q.multiSelect)}
													class="flex items-start gap-2.5 w-full text-left rounded-lg px-3 py-2 text-sm transition-colors {isSelected(msgKey, page, oi)
														? 'bg-primary/20 border border-primary/40'
														: 'bg-secondary/50 border border-transparent hover:bg-secondary'}"
												>
													<span class="flex-shrink-0 mt-0.5 h-4 w-4 rounded-{q.multiSelect ? 'sm' : 'full'} border {isSelected(msgKey, page, oi)
														? 'border-primary bg-primary'
														: 'border-muted-foreground/40'} flex items-center justify-center">
														{#if isSelected(msgKey, page, oi)}
															<svg class="h-2.5 w-2.5 text-primary-foreground" fill="currentColor" viewBox="0 0 20 20"><path fill-rule="evenodd" d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z" clip-rule="evenodd" /></svg>
														{/if}
													</span>
													<div>
														<div class="font-medium">{opt.label}</div>
														{#if opt.description}
															<div class="text-xs text-muted-foreground">{opt.description}</div>
														{/if}
													</div>
												</button>
											{/each}
										</div>
										<div class="flex gap-2 pt-1">
											{#if page > 0}
												<button
													onclick={() => setPage(msgKey, page - 1)}
													class="rounded-lg border border-border px-3 py-1.5 text-sm hover:bg-secondary transition-colors"
												>Back</button>
											{/if}
											<button
												onclick={() => setPage(msgKey, page + 1)}
												class="rounded-lg bg-primary px-3 py-1.5 text-sm text-primary-foreground hover:bg-primary/90 transition-colors ml-auto"
											>{page < totalPages - 1 ? 'Next' : 'Review'}</button>
										</div>
									{/if}
								</div>
							{/if}
						{:else}
							<div class="flex items-center gap-2 flex-row-reverse">
								<span class="text-xs text-muted-foreground">{timeAgo(msg.created_at)}</span>
							</div>
							<div class="rounded-2xl px-4 py-2.5 text-sm prose-chat bg-[hsl(var(--bubble-user))] text-white {msg.message_type === 'system' ? 'italic opacity-70' : ''}">
								{@html renderContent(msg.content)}
							</div>
						{/if}
					</div>
				</div>
				{/if}
			{:else}
				<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
					<div class="text-center space-y-3">
						{#if primaryAgent}
							<img src={agentAvatar(primaryAgent)} alt="" class="h-16 w-16 rounded-full mx-auto object-cover" />
							<div class="font-medium text-foreground">{primaryAgent.name}</div>
						{:else}
							<div class="text-4xl">💬</div>
						{/if}
						<div>Start a conversation</div>
						{#if participantAgents.length > 0}
							<div class="text-xs">Type a message or use @{participantAgents[0]} to mention an agent</div>
						{/if}
					</div>
				</div>
			{/each}

			{#if thinkingAgent}
				{@const thinkingAgentObj = agentList.find(a => a.id === thinkingAgent)}
				<div class="flex gap-3">
					{#if thinkingAgentObj}
						<img src={agentAvatar(thinkingAgentObj)} alt="" class="flex-shrink-0 h-9 w-9 rounded-full object-cover" />
					{:else}
						<div class="flex-shrink-0 h-9 w-9 rounded-full flex items-center justify-center text-xs font-bold bg-secondary text-foreground">
							{(thinkingAgent ?? '?')[0].toUpperCase()}
						</div>
					{/if}
					<div class="max-w-[75%] space-y-1">
						<div class="flex items-center gap-2">
							<span class="text-xs font-semibold text-foreground">{thinkingAgent}</span>
						</div>
						<div class="rounded-2xl bg-[hsl(var(--bubble-agent))] border border-border/50 px-4 py-2.5 text-sm text-foreground prose-chat">
							{#if streamingContent}
								{@html renderContent(streamingContent)}<span class="inline-block w-1.5 h-4 bg-primary/60 animate-pulse ml-0.5 align-text-bottom rounded-sm"></span>
							{:else}
								<span class="text-muted-foreground">{thinkingAgent} is thinking<span class="inline-flex gap-0.5 ml-1"><span class="animate-bounce" style="animation-delay: 0ms">.</span><span class="animate-bounce" style="animation-delay: 150ms">.</span><span class="animate-bounce" style="animation-delay: 300ms">.</span></span></span>
							{/if}
						</div>
					</div>
				</div>
			{/if}
		</div>

		<!-- Error bar -->
		{#if error}
			<div class="px-5 py-2 bg-destructive/10 text-destructive text-xs border-t border-destructive/20">
				{error}
			</div>
		{/if}

		<!-- Input Area -->
		<div class="px-5 pb-4 pt-2">
			<div class="relative">
				<!-- @mention picker -->
				{#if showMentionPicker && filteredAgents.length > 0}
					<div class="absolute bottom-full left-0 mb-1 w-52 rounded-xl border border-border bg-card shadow-xl overflow-hidden z-10">
						{#each filteredAgents as agent}
							<button
								onclick={() => insertMention(agent.id)}
								class="flex w-full items-center gap-2.5 px-3 py-2.5 text-sm hover:bg-secondary text-left transition-colors"
							>
								<img src={agentAvatar(agent)} alt="" class="h-6 w-6 rounded-full object-cover" />
								{agent.name}
							</button>
						{/each}
					</div>
				{/if}

				<div class="flex items-end gap-3">
					<div class="flex-1 rounded-xl border border-border bg-secondary/50 focus-within:border-primary/50 focus-within:ring-1 focus-within:ring-primary/30 transition-all">
						<textarea
							bind:value={input}
							oninput={handleInput}
							onkeydown={handleKeydown}
							oncompositionstart={() => (composing = true)}
							oncompositionend={() => setTimeout(() => (composing = false), 0)}
							placeholder={participantAgents.length > 0 ? `Message ${participantAgents[0]}...  (@ to mention)` : 'Write your message...'}
							rows={1}
							class="w-full resize-none rounded-xl bg-transparent px-4 py-3 text-sm text-foreground focus:outline-none placeholder:text-muted-foreground max-h-32"
							disabled={sending}
						></textarea>
					</div>
					{#if sending}
						<button
							onclick={stopAgent}
							class="flex h-11 w-11 items-center justify-center rounded-xl bg-destructive text-destructive-foreground hover:bg-destructive/90 transition-colors flex-shrink-0 shadow-lg shadow-destructive/20"
							title="Stop agent"
						>
							<svg class="h-5 w-5" fill="currentColor" viewBox="0 0 24 24"><rect x="6" y="6" width="12" height="12" rx="1"/></svg>
						</button>
					{:else}
						<button
							onclick={sendMessage}
							disabled={!input.trim()}
							class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors flex-shrink-0 shadow-lg shadow-primary/20"
						>
							<svg class="h-5 w-5" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
						</button>
					{/if}
				</div>
			</div>
		</div>
	</div>
{/if}
