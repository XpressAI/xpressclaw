<script lang="ts">
	import { page } from '$app/stores';
	import { onMount, tick } from 'svelte';
	import { conversations, agents } from '$lib/api';
	import type { Conversation, ConversationMessage, Agent } from '$lib/api';
	import { timeAgo } from '$lib/utils';

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

	// Participant agent names for @mention
	let participantAgents = $derived(
		conv?.participants.filter(p => p.participant_type === 'agent').map(p => p.participant_id) ?? []
	);

	let filteredAgents = $derived(
		agentList.filter(a =>
			participantAgents.includes(a.id) &&
			a.name.toLowerCase().includes(mentionQuery.toLowerCase())
		)
	);

	$effect(() => {
		// Reload when conversation ID changes
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
			await tick();
			scrollToBottom();
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

		try {
			const newMsgs = await conversations.sendMessage(conv.id, content, 'You');
			messages = [...messages, ...newMsgs];
			await tick();
			scrollToBottom();
		} catch (e) {
			error = String(e);
		} finally {
			sending = false;
		}
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

		// Check if user just typed @
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
		// Replace @[AGENT:id:name] with styled badges
		return content.replace(/@\[AGENT:([^:]+):([^\]]+)\]/g, '<span class="inline-block rounded bg-blue-500/20 text-blue-400 px-1 text-xs font-medium">@$2</span>');
	}

	function isThinking(agentId: string): boolean {
		if (!sending) return false;
		return participantAgents.includes(agentId);
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

	async function deleteConversation() {
		if (!conv) return;
		if (!confirm('Delete this conversation?')) return;
		await conversations.delete(conv.id);
		window.location.href = '/dashboard';
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
	<div class="flex h-full flex-col">
		<!-- Conversation Header -->
		<div class="flex items-center gap-3 border-b border-border px-4 py-3">
			<div class="flex-1 min-w-0">
				{#if editingTitle}
					<form onsubmit={(e) => { e.preventDefault(); saveTitle(); }} class="flex items-center gap-2">
						<input
							type="text"
							bind:value={titleInput}
							class="rounded-md border border-border bg-card px-2 py-1 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
							autofocus
						/>
						<button type="submit" class="text-xs text-primary hover:underline">Save</button>
						<button type="button" onclick={() => (editingTitle = false)} class="text-xs text-muted-foreground hover:underline">Cancel</button>
					</form>
				{:else}
					<button
						onclick={() => { editingTitle = true; titleInput = conv?.title ?? ''; }}
						class="text-base font-semibold hover:text-primary transition-colors text-left"
					>
						{convTitle()}
					</button>
				{/if}
				<div class="flex items-center gap-2 text-xs text-muted-foreground mt-0.5">
					{#each participantAgents as agentId}
						<span class="inline-flex items-center gap-1">
							<span class="h-1.5 w-1.5 rounded-full bg-emerald-400"></span>
							{agentId}
						</span>
					{/each}
					{#if participantAgents.length === 0}
						<span>No agents in this conversation</span>
					{/if}
				</div>
			</div>
			<button
				onclick={deleteConversation}
				class="rounded-md p-1.5 text-muted-foreground hover:bg-destructive/10 hover:text-destructive transition-colors"
				title="Delete conversation"
			>
				<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"/></svg>
			</button>
		</div>

		<!-- Messages -->
		<div bind:this={messagesEl} class="flex-1 overflow-y-auto p-4 space-y-4">
			{#each messages as msg (msg.id)}
				{@const isUser = msg.sender_type === 'user'}
				<div class="flex gap-3 {isUser ? 'flex-row-reverse' : ''}">
					<!-- Avatar -->
					<div class="flex-shrink-0 h-8 w-8 rounded-full flex items-center justify-center text-xs font-bold {isUser ? 'bg-primary text-primary-foreground' : 'bg-accent text-accent-foreground'}">
						{#if isUser}
							Y
						{:else}
							{(msg.sender_id ?? '?')[0].toUpperCase()}
						{/if}
					</div>

					<!-- Message bubble -->
					<div class="max-w-[70%] space-y-1">
						<div class="flex items-center gap-2 {isUser ? 'flex-row-reverse' : ''}">
							<span class="text-xs font-medium">{msg.sender_name ?? msg.sender_id}</span>
							<span class="text-xs text-muted-foreground">{timeAgo(msg.created_at)}</span>
						</div>
						<div class="rounded-lg px-3 py-2 text-sm {isUser
							? 'bg-primary text-primary-foreground'
							: 'bg-accent text-accent-foreground'} {msg.message_type === 'system' ? 'italic opacity-70' : ''}">
							{@html renderContent(msg.content)}
						</div>
					</div>
				</div>
			{:else}
				<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
					<div class="text-center space-y-2">
						<div class="text-4xl">💬</div>
						<div>Start a conversation</div>
						{#if participantAgents.length > 0}
							<div class="text-xs">Type a message or use @{participantAgents[0]} to mention an agent</div>
						{/if}
					</div>
				</div>
			{/each}

			{#if sending}
				<div class="flex gap-3">
					<div class="flex-shrink-0 h-8 w-8 rounded-full flex items-center justify-center text-xs font-bold bg-accent text-accent-foreground">
						{participantAgents.length > 0 ? participantAgents[0][0].toUpperCase() : '?'}
					</div>
					<div class="space-y-1">
						<span class="text-xs font-medium">{participantAgents[0] ?? 'Agent'}</span>
						<div class="rounded-lg bg-accent px-3 py-2 text-sm text-accent-foreground">
							<span class="inline-flex gap-1">
								<span class="animate-bounce" style="animation-delay: 0ms">.</span>
								<span class="animate-bounce" style="animation-delay: 150ms">.</span>
								<span class="animate-bounce" style="animation-delay: 300ms">.</span>
							</span>
						</div>
					</div>
				</div>
			{/if}
		</div>

		<!-- Error bar -->
		{#if error}
			<div class="px-4 py-2 bg-destructive/10 text-destructive text-xs border-t border-destructive/20">
				{error}
			</div>
		{/if}

		<!-- Input Area -->
		<div class="border-t border-border p-3">
			<div class="relative">
				<!-- @mention picker -->
				{#if showMentionPicker && filteredAgents.length > 0}
					<div class="absolute bottom-full left-0 mb-1 w-48 rounded-lg border border-border bg-card shadow-lg overflow-hidden z-10">
						{#each filteredAgents as agent}
							<button
								onclick={() => insertMention(agent.id)}
								class="flex w-full items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
							>
								<span class="h-1.5 w-1.5 rounded-full {agent.status === 'running' ? 'bg-emerald-400' : 'bg-muted-foreground/30'}"></span>
								{agent.name}
							</button>
						{/each}
					</div>
				{/if}

				<div class="flex items-end gap-2">
					<textarea
						bind:value={input}
						oninput={handleInput}
						onkeydown={handleKeydown}
						placeholder={participantAgents.length > 0 ? `Message ${participantAgents.join(', ')}... (@ to mention)` : 'Write your message...'}
						rows={1}
						class="flex-1 resize-none rounded-lg border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring placeholder:text-muted-foreground max-h-32"
						disabled={sending}
					></textarea>
					<button
						onclick={sendMessage}
						disabled={!input.trim() || sending}
						class="flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex-shrink-0"
					>
						<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 12h14M12 5l7 7-7 7"/></svg>
					</button>
				</div>
			</div>
		</div>
	</div>
{/if}
