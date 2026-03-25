<script lang="ts">
	import { page } from '$app/stores';
	import { onMount, tick } from 'svelte';
	import { conversations, agents } from '$lib/api';
	import type { Conversation, ConversationMessage, Agent } from '$lib/api';
	import { timeAgo, agentAvatar, getUserProfile } from '$lib/utils';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';

	marked.setOptions({ breaks: true, gfm: true });

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
	let userProfile = $state(getUserProfile());

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

	$effect(() => {
		const id = $page.params.id;
		if (id) load(id);
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
			stoppedAgents = a.filter(ag => participantIds.includes(ag.id) && ag.status !== 'running');
			if (stoppedAgents.length > 0) {
				showStartDialog = true;
			}

			await tick();
			scrollToBottom();

			const pendingMsg = $page.url.searchParams.get('msg');
			if (pendingMsg) {
				const url = new URL($page.url);
				url.searchParams.delete('msg');
				history.replaceState({}, '', url.pathname);
				input = pendingMsg;
				await tick();
				sendMessage();
			}
		} catch (e) {
			error = String(e);
		}
	}

	async function sendMessage() {
		if (!input.trim() || sending || !conv) return;

		const content = input.trim();
		input = '';
		sending = true;
		error = null;
		thinkingAgent = null;
		streamingContent = '';

		const abort = conversations.streamMessage(conv.id, content, userProfile.name, {
			onUserMessage: async (msg) => {
				messages = [...messages, msg];
				await tick();
				scrollToBottom();
			},
			onThinking: async (agentId) => {
				thinkingAgent = agentId;
				streamingContent = '';
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
				messages = [...messages, msg];
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
				cancelStream = null;
			}
		});

		cancelStream = abort;
	}

	function scrollToBottom() {
		if (messagesEl) {
			messagesEl.scrollTop = messagesEl.scrollHeight;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (showMentionPicker) {
			if (e.key === 'Escape') {
				showMentionPicker = false;
				e.preventDefault();
			} else if (e.key === 'Enter' && filteredAgents.length > 0) {
				insertMention(filteredAgents[0].id);
				e.preventDefault();
			}
			return;
		}
		if (e.key === 'Enter' && !e.shiftKey) {
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

	function renderContent(content: string): string {
		let result = content;

		const thinkingBlocks: string[] = [];

		result = result.replace(/<think>([\s\S]*?)<\/think>/g, (_match: string, thinking: string) => {
			const trimmed = thinking.trim();
			if (!trimmed) return '';
			const idx = thinkingBlocks.length;
			thinkingBlocks.push(trimmed);
			return `%%THINK_${idx}%%`;
		});

		result = result.replace(/<think>([\s\S]*)$/g, (_match: string, thinking: string) => {
			const trimmed = thinking.trim();
			const idx = thinkingBlocks.length;
			thinkingBlocks.push(trimmed || '');
			return `%%THINKSTREAM_${idx}%%`;
		});

		const toolCallBlocks: { name: string; args: string }[] = [];
		result = result.replace(/<tool_call name="([^"]*)">([\s\S]*?)<\/tool_call>/g, (_match: string, name: string, args: string) => {
			const idx = toolCallBlocks.length;
			toolCallBlocks.push({ name, args: args.trim() });
			return `%%TOOL_${idx}%%`;
		});

		result = result.replace(/@\[AGENT:([^:]+):([^\]]+)\]/g, '**@$2**');

		result = DOMPurify.sanitize(marked.parse(result) as string, {
			ADD_TAGS: ['details', 'summary'],
			ADD_ATTR: ['open']
		});

		for (let i = 0; i < thinkingBlocks.length; i++) {
			const thinking = thinkingBlocks[i];
			const escaped = DOMPurify.sanitize(marked.parse(thinking) as string);

			result = result.replace(
				`%%THINK_${i}%%`,
				`<details class="mb-2 rounded-lg border border-border/50 bg-[hsl(228_22%_13%)] text-xs not-prose"><summary class="cursor-pointer px-3 py-1.5 text-muted-foreground select-none">Thinking...</summary><div class="px-3 py-2 text-muted-foreground/80 border-t border-border/30">${escaped}</div></details>`
			);

			const streamHtml = thinking
				? `<div class="mb-2 rounded-lg border border-border/50 bg-[hsl(228_22%_13%)] text-xs not-prose"><div class="px-3 py-1.5 text-muted-foreground select-none flex items-center gap-1.5"><span class="inline-block h-2 w-2 rounded-full bg-amber-400 animate-pulse"></span> Thinking...</div><div class="px-3 py-2 text-muted-foreground/80 border-t border-border/30">${escaped}</div></div>`
				: '<span class="text-xs text-muted-foreground italic">Thinking...</span>';
			result = result.replace(`%%THINKSTREAM_${i}%%`, streamHtml);
		}

		for (let i = 0; i < toolCallBlocks.length; i++) {
			const { name, args } = toolCallBlocks[i];
			let prettyArgs = args;
			try { prettyArgs = JSON.stringify(JSON.parse(args), null, 2); } catch {}
			const escapedArgs = prettyArgs.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
			result = result.replace(
				`%%TOOL_${i}%%`,
				`<details class="mb-2 rounded-lg border border-blue-500/30 bg-blue-500/5 text-xs not-prose"><summary class="cursor-pointer px-3 py-1.5 text-blue-400 select-none flex items-center gap-1.5"><span>&#x1f527;</span> ${name}</summary><pre class="px-3 py-2 text-muted-foreground/80 border-t border-blue-500/20 overflow-x-auto">${escapedArgs}</pre></details>`
			);
		}

		return result;
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
				</div>
			</div>
			<div class="flex items-center gap-1">
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
							<div class="flex items-center gap-2">
								<span class="text-xs font-semibold text-foreground">{msg.sender_name ?? msg.sender_id}</span>
								<span class="text-xs text-muted-foreground">{timeAgo(msg.created_at)}</span>
							</div>
						{:else}
							<div class="flex items-center gap-2 flex-row-reverse">
								<span class="text-xs text-muted-foreground">{timeAgo(msg.created_at)}</span>
							</div>
						{/if}
						<div class="rounded-2xl px-4 py-2.5 text-sm prose-chat {isUser
							? 'bg-[hsl(var(--bubble-user))] text-white'
							: 'bg-[hsl(var(--bubble-agent))] text-foreground border border-border/50'} {msg.message_type === 'system' ? 'italic opacity-70' : ''}">
							{@html renderContent(msg.content)}
						</div>
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
							placeholder={participantAgents.length > 0 ? `Message ${participantAgents[0]}...  (@ to mention)` : 'Write your message...'}
							rows={1}
							class="w-full resize-none rounded-xl bg-transparent px-4 py-3 text-sm text-foreground focus:outline-none placeholder:text-muted-foreground max-h-32"
							disabled={sending}
						></textarea>
					</div>
					<button
						onclick={sendMessage}
						disabled={!input.trim() || sending}
						class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors flex-shrink-0 shadow-lg shadow-primary/20"
					>
						<svg class="h-5 w-5" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
					</button>
				</div>
			</div>
		</div>
	</div>
{/if}
