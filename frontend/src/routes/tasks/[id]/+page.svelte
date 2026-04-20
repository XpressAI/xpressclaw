<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { tasks, agents } from '$lib/api';
	import type { Task, TaskMessage, Agent } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import { renderContent } from '$lib/formatMessage';

	let task = $state<Task | null>(null);
	let messages = $state<TaskMessage[]>([]);
	let subtaskList = $state<Task[]>([]);
	let agentList = $state<Agent[]>([]);
	let allTasks = $state<Task[]>([]);
	let error = $state<string | null>(null);
	let loading = $state(true);
	let editing = $state(false);
	let editTitle = $state('');
	let editDesc = $state('');
	let editAgentId = $state('');
	let editPriority = $state(0);
	let editDeps = $state<string[]>([]);
	let messageInput = $state('');
	let messageSending = $state(false);
	let composing = $state(false);
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let messagesEl: HTMLDivElement;
	let prevMessageCount = 0;
	let initialLoad = true;

	let availableDeps = $derived(
		allTasks.filter(t => t.id !== task?.id && t.status !== 'completed' && t.status !== 'cancelled')
	);

	onMount(async () => {
		await load();
		loading = false;
		// Auto-poll while task is in progress
		pollTimer = setInterval(async () => {
			if (task && (task.status === 'in_progress' || task.status === 'pending' || task.status === 'waiting_for_input')) {
				await poll();
			}
		}, 3000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function load() {
		try {
			const id = $page.params.id!;
			[task, agentList] = await Promise.all([
				tasks.get(id),
				agents.list().catch(() => []),
			]);
			messages = await tasks.messages(id);
			prevMessageCount = messages.length;
			try {
				const sub = await tasks.subtasks(id);
				subtaskList = sub.tasks;
			} catch { subtaskList = []; }
			try {
				const all = await tasks.list();
				allTasks = all.tasks;
			} catch { allTasks = []; }
			if (initialLoad) {
				initialLoad = false;
				scrollToBottom();
			}
		} catch (e) {
			error = String(e);
		}
	}

	/** Poll for updates without resetting scroll. Only scroll if new messages arrived. */
	async function poll() {
		try {
			const id = $page.params.id!;
			const [newTask, newMessages] = await Promise.all([
				tasks.get(id),
				tasks.messages(id),
			]);
			// Update task status/details in-place
			task = newTask;
			// Only update messages and scroll if count changed
			if (newMessages.length !== prevMessageCount) {
				messages = newMessages;
				prevMessageCount = newMessages.length;
				scrollToBottom();
			}
			try {
				const sub = await tasks.subtasks(id);
				subtaskList = sub.tasks;
			} catch { subtaskList = []; }
		} catch { /* ignore poll errors */ }
	}

	function scrollToBottom() {
		setTimeout(() => {
			if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
		}, 50);
	}

	async function sendTaskMessage() {
		if (!messageInput.trim() || !task) return;
		const content = messageInput.trim();
		messageInput = '';
		messageSending = true;
		try {
			await tasks.addMessage(task.id, 'user', content);
			await poll();
			scrollToBottom();
		} catch (e) {
			alert(String(e));
		} finally {
			messageSending = false;
		}
	}

	function handleMessageKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !composing && e.keyCode !== 229) {
			e.preventDefault();
			sendTaskMessage();
		}
	}

	async function updateStatus(status: string) {
		if (!task) return;
		try {
			task = await tasks.updateStatus(task.id, status);
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	function startEditing() {
		if (!task) return;
		editTitle = task.title;
		editDesc = task.description || '';
		editAgentId = task.agent_id || '';
		editPriority = task.priority;
		editDeps = task.depends_on ? [...task.depends_on] : [];
		editing = true;
	}

	function toggleEditDep(id: string) {
		if (editDeps.includes(id)) {
			editDeps = editDeps.filter(d => d !== id);
		} else {
			editDeps = [...editDeps, id];
		}
	}

	async function saveEdit() {
		if (!task) return;
		try {
			// Update task fields
			await tasks.update(task.id, {
				title: editTitle,
				description: editDesc || undefined,
				agent_id: editAgentId || undefined,
				priority: editPriority,
			});
			// Add new dependencies
			const currentDeps = task.depends_on || [];
			for (const depId of editDeps) {
				if (!currentDeps.includes(depId)) {
					await tasks.addDependency(task.id, depId).catch(() => {});
				}
			}
			editing = false;
			await load();
		} catch (e) {
			console.error('Save failed:', e);
		}
	}

	function statusColor(status: string): string {
		switch (status) {
			case 'completed': return 'text-emerald-400';
			case 'in_progress': return 'text-blue-400';
			case 'pending': return 'text-amber-400';
			case 'blocked': return 'text-red-400';
			case 'waiting_for_input': return 'text-orange-400';
			case 'cancelled': return 'text-muted-foreground';
			default: return 'text-muted-foreground';
		}
	}

	function statusBg(status: string): string {
		switch (status) {
			case 'completed': return 'bg-emerald-500/10 border-emerald-500/30';
			case 'in_progress': return 'bg-blue-500/10 border-blue-500/30';
			case 'pending': return 'bg-amber-500/10 border-amber-500/30';
			case 'blocked': return 'bg-red-500/10 border-red-500/30';
			case 'waiting_for_input': return 'bg-orange-500/10 border-orange-500/30';
			default: return 'bg-muted/10 border-border';
		}
	}

	function priorityLabel(p: number): string {
		if (p >= 3) return 'Urgent';
		if (p >= 2) return 'High';
		if (p >= 1) return 'Normal';
		return 'Low';
	}
</script>

<div class="flex h-full flex-col">
	<!-- Header -->
	<div class="border-b border-border p-4">
		<div class="flex items-center gap-2 text-sm text-muted-foreground mb-2">
			<a href="/tasks" class="hover:text-foreground">Tasks</a>
			<span>/</span>
			<span class="text-foreground truncate">{task?.title ?? '...'}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">{error}</div>
		{:else if task}
			<div class="flex items-start justify-between">
				<div>
					<h1 class="text-xl font-bold">{task.title}</h1>
					<div class="flex items-center gap-3 mt-1 text-sm">
						<span class="flex items-center gap-1.5">
							<span class="h-2 w-2 rounded-full {task.status === 'in_progress' ? 'animate-pulse' : ''}
								{task.status === 'completed' ? 'bg-emerald-400' :
								 task.status === 'in_progress' ? 'bg-blue-400' :
								 task.status === 'pending' ? 'bg-amber-400' :
								 task.status === 'blocked' ? 'bg-red-400' :
								 'bg-muted-foreground'}"></span>
							<span class="{statusColor(task.status)}">{task.status.replace('_', ' ')}</span>
						</span>
						{#if task.agent_id}
							<span class="text-muted-foreground">{task.agent_id}</span>
						{/if}
						<span class="text-muted-foreground">{priorityLabel(task.priority)}</span>
						<span class="text-xs text-muted-foreground">{timeAgo(task.created_at)}</span>
					</div>
				</div>
				<div class="flex gap-2">
					{#if task.status !== 'completed' && task.status !== 'cancelled'}
						<button onclick={startEditing}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">
							Edit
						</button>
					{/if}
					{#if task.status === 'pending'}
						<button onclick={() => updateStatus('in_progress')}
							class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
							Start
						</button>
					{/if}
					{#if task.status === 'in_progress' || task.status === 'pending'}
						<button onclick={() => updateStatus('completed')}
							class="rounded-md border border-emerald-500/50 px-3 py-1.5 text-xs font-medium text-emerald-400 hover:bg-emerald-500/10">
							Complete
						</button>
						<button onclick={() => updateStatus('cancelled')}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent">
							Cancel
						</button>
					{/if}
				</div>
			</div>
		{/if}
	</div>

	{#if editing && task}
		<div class="border-b border-border p-4 space-y-3 bg-card/50">
			<input type="text" bind:value={editTitle} placeholder="Task title..."
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" />
			<textarea bind:value={editDesc} placeholder="Description..." rows="2"
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring resize-none"></textarea>
			<div class="flex gap-3">
				<div class="flex-1">
					<div class="text-xs text-muted-foreground mb-1">Assign to Agent</div>
					<select bind:value={editAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
						<option value="">Unassigned</option>
						{#each agentList as agent}
							<option value={agent.id}>{agent.name}</option>
						{/each}
					</select>
				</div>
				<div class="w-24">
					<div class="text-xs text-muted-foreground mb-1">Priority</div>
					<select bind:value={editPriority}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
						<option value={0}>Normal</option>
						<option value={5}>High</option>
						<option value={10}>Urgent</option>
					</select>
				</div>
			</div>
			{#if availableDeps.length > 0}
				<div>
					<div class="text-xs text-muted-foreground mb-1">Depends on</div>
					<div class="flex flex-wrap gap-1.5 max-h-24 overflow-y-auto">
						{#each availableDeps as dep}
							<button type="button" onclick={() => toggleEditDep(dep.id)}
								class="rounded-md border px-2 py-1 text-xs transition-colors
									{editDeps.includes(dep.id)
										? 'border-primary bg-primary/10 text-primary'
										: 'border-border text-muted-foreground hover:border-primary/50'}">
								{dep.title}
							</button>
						{/each}
					</div>
				</div>
			{/if}
			<div class="flex gap-2">
				<button onclick={saveEdit}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
					Save
				</button>
				<button onclick={() => (editing = false)}
					class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">
					Cancel
				</button>
			</div>
		</div>
	{/if}

	{#if loading}
		<div class="flex-1 flex items-center justify-center text-muted-foreground text-sm">Loading...</div>
	{:else if task}
		<div class="flex flex-1 overflow-hidden">
			<!-- Left: conversation -->
			<div class="flex-1 flex flex-col overflow-hidden">
				<div bind:this={messagesEl} class="flex-1 overflow-y-auto p-4 space-y-3">
					<!-- Task description -->
					{#if task.description}
						<div class="rounded-lg border {statusBg(task.status)} p-3 text-sm">
							<div class="text-xs font-medium text-muted-foreground mb-1">Description</div>
							<div class="whitespace-pre-wrap">{task.description}</div>
						</div>
					{/if}

					<!-- Dependencies -->
					{#if task.blocked_by && task.blocked_by.length > 0}
						<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
							<div class="text-xs font-medium text-amber-500 mb-1">Blocked by</div>
							<div class="space-y-1">
								{#each task.blocked_by as blockerId}
									<a href="/tasks/{blockerId}" class="block text-xs text-amber-400 hover:underline">
										{blockerId}
									</a>
								{/each}
							</div>
						</div>
					{/if}
					{#if task.depends_on && task.depends_on.length > 0}
						<div class="rounded-lg border border-border/50 p-3 text-sm">
							<div class="text-xs font-medium text-muted-foreground mb-1">Dependencies</div>
							<div class="space-y-1">
								{#each task.depends_on as depId}
									<a href="/tasks/{depId}" class="block text-xs text-muted-foreground hover:underline">
										{#if task.blocked_by?.includes(depId)}⏳{:else}✅{/if} {depId}
									</a>
								{/each}
							</div>
						</div>
					{/if}

					<!-- Messages -->
					{#each messages as msg (msg.id)}
						{@const isSystem = msg.role === 'system'}
						{@const isAssistant = msg.role === 'assistant'}
						<div class="flex gap-3 {isSystem ? '' : isAssistant ? '' : 'flex-row-reverse'}">
							<div class="flex-shrink-0 h-7 w-7 rounded-full flex items-center justify-center text-xs font-bold
								{isSystem ? 'bg-muted text-muted-foreground' :
								 isAssistant ? 'bg-accent text-accent-foreground' :
								 'bg-primary text-primary-foreground'}">
								{#if isSystem}S{:else if isAssistant}A{:else}U{/if}
							</div>
							<div class="max-w-[80%]">
								<div class="flex items-center gap-2 mb-0.5">
									<span class="text-xs font-medium {isSystem ? 'text-muted-foreground' : ''}">{msg.role}</span>
									<span class="text-xs text-muted-foreground">{timeAgo(msg.timestamp)}</span>
								</div>
								<div class="rounded-lg px-3 py-2 text-sm prose prose-invert prose-sm max-w-none
									{isSystem ? 'bg-muted/50 text-muted-foreground text-xs italic' :
									 isAssistant ? 'bg-accent text-accent-foreground' :
									 'bg-primary text-primary-foreground'}">
									{@html renderContent(msg.content)}
								</div>
							</div>
						</div>
					{:else}
						{#if !task.description}
							<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
								<div class="text-center space-y-1">
									<div class="text-3xl">&#x1f4cb;</div>
									<div>No activity yet</div>
								</div>
							</div>
						{/if}
					{/each}

					<!-- Live indicator -->
					{#if task.status === 'in_progress'}
						<div class="flex items-center gap-2 text-xs text-muted-foreground">
							<span class="h-2 w-2 rounded-full bg-blue-400 animate-pulse"></span>
							Agent is working on this task...
						</div>
					{:else if task.status === 'waiting_for_input'}
						<div class="flex items-center gap-2 text-xs text-orange-400">
							<span class="h-2 w-2 rounded-full bg-orange-400 animate-pulse"></span>
							Waiting for your response...
						</div>
					{/if}
				</div>

				<!-- Message input -->
				{#if task.status === 'in_progress' || task.status === 'waiting_for_input'}
					<div class="border-t border-border p-4">
						{#if task.status === 'waiting_for_input'}
							<div class="text-xs text-orange-400 mb-2">The agent needs your input to continue</div>
						{/if}
						<div class="flex items-end gap-3">
							<div class="flex-1 rounded-xl border border-border bg-secondary/50 focus-within:border-primary/50 focus-within:ring-1 focus-within:ring-primary/30 transition-all">
								<textarea
									bind:value={messageInput}
									onkeydown={handleMessageKeydown}
									oncompositionstart={() => (composing = true)}
									oncompositionend={() => setTimeout(() => (composing = false), 0)}
									placeholder="Send a message to the agent..."
									rows={1}
									class="w-full resize-none rounded-xl bg-transparent px-4 py-3 text-sm text-foreground focus:outline-none placeholder:text-muted-foreground max-h-32"
									disabled={messageSending}
								></textarea>
							</div>
							<button
								onclick={sendTaskMessage}
								disabled={!messageInput.trim() || messageSending}
								class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors flex-shrink-0 shadow-lg shadow-primary/20"
							>
								<svg class="h-5 w-5" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
							</button>
						</div>
					</div>
				{/if}
			</div>

			<!-- Right: details sidebar -->
			<div class="w-72 border-l border-border p-4 overflow-y-auto space-y-4">
				<!-- Details -->
				<div class="space-y-2">
					<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Details</h3>
					<dl class="space-y-1.5 text-sm">
						<div class="flex justify-between">
							<dt class="text-muted-foreground">ID</dt>
							<dd class="font-mono text-xs truncate max-w-[140px]">{task.id}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Status</dt>
							<dd class="{statusColor(task.status)}">{task.status.replace('_', ' ')}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Priority</dt>
							<dd>{priorityLabel(task.priority)}</dd>
						</div>
						{#if task.agent_id}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Agent</dt>
								<dd><a href="/agents/{task.agent_id}" class="underline hover:text-foreground">{task.agent_id}</a></dd>
							</div>
						{/if}
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Created</dt>
							<dd class="text-xs">{timeAgo(task.created_at)}</dd>
						</div>
						{#if task.completed_at}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Completed</dt>
								<dd class="text-xs">{timeAgo(task.completed_at)}</dd>
							</div>
						{/if}
					</dl>
				</div>

				<!-- Subtasks -->
				{#if subtaskList.length > 0}
					<div class="space-y-2">
						<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">
							Steps ({subtaskList.filter(s => s.status === 'completed').length}/{subtaskList.length})
						</h3>
						<div class="space-y-1.5">
							{#each subtaskList as sub}
								<div class="flex items-start gap-2 rounded p-1.5 text-sm">
									<span class="mt-0.5 flex-shrink-0 h-4 w-4 rounded border flex items-center justify-center
										{sub.status === 'completed'
											? 'bg-emerald-500/20 border-emerald-500 text-emerald-400'
											: sub.status === 'in_progress'
											? 'border-blue-400 text-blue-400'
											: 'border-muted-foreground/30'}">
										{#if sub.status === 'completed'}
											<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
												<path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
											</svg>
										{:else if sub.status === 'in_progress'}
											<span class="h-1.5 w-1.5 rounded-full bg-blue-400 animate-pulse"></span>
										{/if}
									</span>
									<div class="flex-1 min-w-0">
										<span class="block truncate {sub.status === 'completed' ? 'line-through text-muted-foreground' : ''}">{sub.title}</span>
										{#if sub.description}
											<span class="block text-xs text-muted-foreground mt-0.5 line-clamp-2">{sub.description}</span>
										{/if}
									</div>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				<!-- Conversation link -->
				{#if task.context}
					{@const ctx = typeof task.context === 'string' ? (() => { try { return JSON.parse(task.context); } catch { return null; } })() : task.context}
					{#if ctx?.conversation_id}
						<div class="space-y-2">
							<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Linked Conversation</h3>
							<a href="/conversations/{ctx.conversation_id}" class="text-sm underline hover:text-foreground">
								Open conversation
							</a>
						</div>
					{/if}
				{/if}
			</div>
		</div>
	{/if}
</div>
