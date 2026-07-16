<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { tasks, agents } from '$lib/api';
	import type { Task, TaskMessage, Agent, WorkAttempt, SessionEvent } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import { renderContent } from '$lib/formatMessage';

	let task = $state<Task | null>(null);
	let messages = $state<TaskMessage[]>([]);
	let attempts = $state<WorkAttempt[]>([]);
	let activityEvents = $state<SessionEvent[]>([]);
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
	let lastActivityEventId = 0;
	let initialLoad = true;

	let availableDeps = $derived(
		allTasks.filter(t => t.id !== task?.id && t.status !== 'completed' && t.status !== 'cancelled')
	);
	let primaryActivityEvents = $derived(
		activityEvents.filter(event => {
			const mirrorsTaskReply = event.payload?.item_type === 'agent_message' && messages.some(message =>
				message.role === 'assistant' && (
					message.content === event.summary ||
					(event.summary.length >= 200 && message.content.startsWith(event.summary.slice(0, 180)))
				)
			);
			return !['artifact_created', 'attempt_completed'].includes(event.event_type) &&
				(event.event_type !== 'runner_progress' || event.payload?.item_type === 'agent_message') &&
				!mirrorsTaskReply;
		})
	);
	let technicalActivityEvents = $derived(
		activityEvents.filter(event =>
			event.event_type === 'runner_progress' && event.payload?.item_type !== 'agent_message'
		)
	);
	let activeAttempt = $derived(
		attempts.find(attempt => ['queued', 'preparing', 'running', 'review'].includes(attempt.status)) ?? null
	);
	let latestAttemptResult = $derived(attempts.find(attempt => attempt.result)?.result ?? null);
	let latestResult = $derived(
		latestAttemptResult && !messages.some(message =>
			message.role === 'assistant' && message.content === latestAttemptResult
		)
			? latestAttemptResult
			: null
	);
	let latestError = $derived(attempts.find(attempt => attempt.error_message)?.error_message ?? null);
	let finalAssistantMessage = $derived(messages.at(-1)?.role === 'assistant' ? messages.at(-1)! : null);
	let conversationMessages = $derived(finalAssistantMessage ? messages.slice(0, -1) : messages);
	let messagePlaceholder = $derived(
		!task?.agent_id
			? 'Assign a project to chat about this task'
			: task.status === 'waiting_for_input'
				? 'Reply to the worker...'
				: ['completed', 'blocked', 'cancelled'].includes(task.status)
					? 'Ask a follow-up or request a correction...'
					: 'Send additional context...'
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
			const [loadedTask, loadedAgents, loadedMessages, activity] = await Promise.all([
				tasks.get(id),
				agents.list().catch(() => []),
				tasks.messages(id),
				tasks.activity(id),
			]);
			task = loadedTask;
			agentList = loadedAgents;
			messages = loadedMessages;
			attempts = activity.attempts;
			activityEvents = activity.events;
			prevMessageCount = messages.length;
			lastActivityEventId = activityEvents.at(-1)?.id ?? 0;
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

	/** Poll semantic task activity without exposing the runner's terminal. */
	async function poll() {
		try {
			const id = $page.params.id!;
			const [newTask, newMessages, newActivity] = await Promise.all([
				tasks.get(id),
				tasks.messages(id),
				tasks.activity(id, lastActivityEventId || undefined),
			]);
			// Update task status/details in-place
			task = newTask;
			attempts = newActivity.attempts;
			let shouldScroll = false;
			// Only update messages and scroll if count changed
			if (newMessages.length !== prevMessageCount) {
				messages = newMessages;
				prevMessageCount = newMessages.length;
				shouldScroll = true;
			}
			if (newActivity.events.length > 0) {
				const known = new Set(activityEvents.map(event => event.id));
				activityEvents = [
					...activityEvents,
					...newActivity.events.filter(event => !known.has(event.id)),
				];
				lastActivityEventId = activityEvents.at(-1)?.id ?? lastActivityEventId;
				shouldScroll = true;
			}
			if (shouldScroll) scrollToBottom();
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

	function sessionLabel(id: string): string {
		const session = agentList.find(agent => agent.id === id);
		return session?.title || session?.name || id;
	}

	function taskLabel(id: string): string {
		return allTasks.find((candidate) => candidate.id === id)?.title ?? id;
	}

	function startsFreshConversation(): boolean {
		if (!task?.context || typeof task.context !== 'object') return false;
		return (task.context as Record<string, unknown>).session_mode === 'new';
	}

	function activityDot(eventType: string): string {
		if (eventType === 'attempt_failed') return 'bg-red-400';
		if (eventType === 'attempt_cancelled') return 'bg-muted-foreground';
		if (eventType === 'runner_progress') return 'bg-blue-400';
		if (eventType === 'attempt_running') return 'bg-emerald-400';
		return 'bg-amber-400';
	}
</script>

<div class="flex min-h-0 h-full flex-col">
	<!-- Header -->
	<div class="shrink-0 border-b border-border p-3 sm:p-4">
		<div class="flex items-center gap-2 text-sm text-muted-foreground mb-2">
			<a href="/tasks" class="hover:text-foreground">Tasks</a>
			<span>/</span>
			<span class="text-foreground truncate">{task?.title ?? '...'}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">{error}</div>
		{:else if task}
			<div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
				<div class="min-w-0">
					<h1 class="text-lg font-bold sm:text-xl">{task.title}</h1>
					<div class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs sm:text-sm">
						<span class="flex items-center gap-1.5">
							<span class="h-2 w-2 rounded-full {task.status === 'in_progress' ? 'animate-pulse' : ''}
								{task.status === 'completed' ? 'bg-emerald-400' :
								 task.status === 'in_progress' ? 'bg-blue-400' :
								 task.status === 'pending' ? 'bg-amber-400' :
								 task.status === 'waiting_for_input' ? 'bg-orange-400' :
								 task.status === 'blocked' ? 'bg-red-400' :
								 'bg-muted-foreground'}"></span>
							<span class="{statusColor(task.status)}">{task.status.replaceAll('_', ' ')}</span>
						</span>
						{#if task.agent_id}
							<span class="text-muted-foreground">{sessionLabel(task.agent_id)}</span>
						{/if}
						<span class="text-muted-foreground">{priorityLabel(task.priority)}</span>
						<span class="text-xs text-muted-foreground">{timeAgo(task.created_at)}</span>
					</div>
				</div>
				<div class="flex shrink-0 gap-2 overflow-x-auto">
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
					{#if ['in_progress', 'pending', 'waiting_for_input', 'blocked'].includes(task.status)}
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
		<div class="shrink-0 space-y-3 overflow-y-auto border-b border-border bg-card/50 p-3 sm:p-4">
			<input type="text" bind:value={editTitle} placeholder="Task title..."
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" />
			<textarea bind:value={editDesc} placeholder="Description..." rows="2"
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring resize-none"></textarea>
			<div class="flex flex-col gap-3 sm:flex-row">
				<div class="flex-1">
					<div class="text-xs text-muted-foreground mb-1">Project</div>
					<select bind:value={editAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
						<option value="">Unassigned</option>
						{#each agentList as agent}
							<option value={agent.id}>{agent.title || agent.name}</option>
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
		<div class="flex min-h-0 flex-1 overflow-hidden">
			<!-- Left: conversation -->
			<div class="flex-1 flex flex-col overflow-hidden">
				<div bind:this={messagesEl} class="flex-1 space-y-3 overflow-y-auto p-3 sm:p-4">
					<div class="flex flex-wrap items-center gap-2 text-[11px] text-muted-foreground">
						<span class="rounded-full border border-border bg-secondary/40 px-2 py-1">{startsFreshConversation() ? 'Fresh conversation' : 'Continues project conversation'}</span>
						{#if task.depends_on && task.depends_on.length > 0}<span>Continues its dependency</span>{/if}
					</div>
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
										{taskLabel(blockerId)}
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
										{#if task.blocked_by?.includes(depId)}⏳{:else}✅{/if} {taskLabel(depId)}
									</a>
								{/each}
							</div>
						</div>
					{/if}

					<!-- Messages -->
					{#each conversationMessages as msg (msg.id)}
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
					{/each}

					<!-- Native attempt activity -->
					{#if primaryActivityEvents.length > 0}
						<section class="space-y-2">
							<div class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Activity</div>
							{#each primaryActivityEvents as event (event.id)}
								{@const itemType = String(event.payload?.item_type ?? '')}
								<div class="flex gap-3 rounded-lg border border-border/60 bg-card/40 p-3">
									<div class="mt-1.5 h-2 w-2 flex-shrink-0 rounded-full {activityDot(event.event_type)}"></div>
									<div class="min-w-0 flex-1">
										<div class="mb-1 flex items-center justify-between gap-3">
											<span class="text-xs font-medium text-muted-foreground">
												{itemType === 'agent_message' ? 'Worker update' : event.event_type.replaceAll('_', ' ')}
											</span>
											<span class="flex-shrink-0 text-xs text-muted-foreground">{timeAgo(event.created_at)}</span>
										</div>
										{#if itemType === 'agent_message'}
											<div class="prose prose-invert prose-sm max-w-none text-sm">{@html renderContent(event.summary)}</div>
										{:else}
											<div class="break-words font-mono text-xs text-foreground/90">{event.summary}</div>
										{/if}
									</div>
								</div>
							{/each}
						</section>
					{/if}

					{#if technicalActivityEvents.length > 0}
						<section class="rounded-lg border border-border/60 bg-card/30">
							<div class="border-b border-border/60 px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
								Technical steps ({technicalActivityEvents.length})
							</div>
							<div class="space-y-2 p-3">
								{#each technicalActivityEvents as event (event.id)}
									<div class="flex gap-2 text-xs">
										<span class="mt-1 h-1.5 w-1.5 flex-shrink-0 rounded-full bg-blue-400"></span>
										<span class="min-w-0 flex-1 break-words font-mono text-foreground/80">{event.summary}</span>
										<span class="flex-shrink-0 text-muted-foreground">{timeAgo(event.created_at)}</span>
									</div>
								{/each}
							</div>
						</section>
					{/if}

					{#if subtaskList.length > 0}
						<section class="rounded-lg border border-border/60 bg-card/30 p-3 lg:hidden">
							<div class="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">Steps ({subtaskList.filter((step) => step.status === 'completed').length}/{subtaskList.length})</div>
							<div class="space-y-2">
								{#each subtaskList as subtask}
									<div class="flex items-start gap-2 text-sm"><span class="mt-0.5 {subtask.status === 'completed' ? 'text-emerald-400' : subtask.status === 'in_progress' ? 'text-blue-400' : 'text-muted-foreground'}">{subtask.status === 'completed' ? '✓' : subtask.status === 'in_progress' ? '●' : '○'}</span><span class={subtask.status === 'completed' ? 'text-muted-foreground line-through' : ''}>{subtask.title}</span></div>
								{/each}
							</div>
						</section>
					{/if}

					{#if latestResult}
						<section class="rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-4">
							<div class="mb-2 flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-emerald-400">
								<span class="h-2 w-2 rounded-full bg-emerald-400"></span>
								Result
							</div>
							<div class="prose prose-invert prose-sm max-w-none">{@html renderContent(latestResult)}</div>
						</section>
					{:else if latestError}
						<section class="rounded-lg border border-red-500/30 bg-red-500/5 p-4">
							<div class="mb-2 text-xs font-medium uppercase tracking-wide text-red-400">Attempt failed</div>
							<div class="whitespace-pre-wrap text-sm text-red-200">{latestError}</div>
						</section>
					{/if}

					{#if finalAssistantMessage}
						<div class="flex gap-3">
							<div class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-full bg-accent text-xs font-bold text-accent-foreground">A</div>
							<div class="max-w-[88%]">
								<div class="mb-0.5 flex items-center gap-2"><span class="text-xs font-medium">assistant</span><span class="text-xs text-muted-foreground">{timeAgo(finalAssistantMessage.timestamp)}</span></div>
								<div class="prose prose-invert prose-sm max-w-none rounded-lg bg-accent px-3 py-2 text-sm text-accent-foreground">{@html renderContent(finalAssistantMessage.content)}</div>
							</div>
						</div>
					{/if}

					{#if messages.length === 0 && primaryActivityEvents.length === 0 && technicalActivityEvents.length === 0 && !latestResult && !latestError && !task.description}
						<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
							<div class="space-y-1 text-center">
								<div class="text-3xl">&#x1f4cb;</div>
								<div>No activity yet</div>
							</div>
						</div>
					{/if}

					<!-- Live indicator -->
					{#if activeAttempt?.status === 'running' || activeAttempt?.status === 'preparing' || activeAttempt?.status === 'review'}
						<div class="flex items-center gap-2 text-xs text-muted-foreground">
							<span class="h-2 w-2 rounded-full bg-blue-400 animate-pulse"></span>
							A native worker is handling this task...
						</div>
					{:else if activeAttempt?.status === 'queued'}
						<div class="flex items-center gap-2 text-xs text-muted-foreground">
							<span class="h-2 w-2 rounded-full bg-amber-400 animate-pulse"></span>
							The next worker turn is queued...
						</div>
					{:else if task.status === 'waiting_for_input'}
						<div class="flex items-center gap-2 text-xs text-orange-400">
							<span class="h-2 w-2 rounded-full bg-orange-400 animate-pulse"></span>
							Waiting for your response...
						</div>
					{:else if task.status === 'in_progress' && subtaskList.some(subtask => subtask.status !== 'completed')}
						<div class="flex items-center gap-2 text-xs text-amber-400">
							<span class="h-2 w-2 rounded-full bg-amber-400"></span>
							This task still has unfinished steps.
						</div>
					{/if}
				</div>

				<!-- Message input -->
				<div class="shrink-0 border-t border-border bg-background p-3 sm:p-4">
					{#if task.status === 'waiting_for_input'}
						<div class="text-xs text-orange-400 mb-2">The native worker needs additional input</div>
					{:else if !task.agent_id}
						<div class="text-xs text-muted-foreground mb-2">Assign a project before sending a message</div>
					{/if}
					<div class="flex items-end gap-3">
							<div class="flex-1 rounded-xl border border-border bg-secondary/50 focus-within:border-primary/50 focus-within:ring-1 focus-within:ring-primary/30 transition-all">
								<textarea
									bind:value={messageInput}
									onkeydown={handleMessageKeydown}
									oncompositionstart={() => (composing = true)}
									oncompositionend={() => setTimeout(() => (composing = false), 0)}
									placeholder={messagePlaceholder}
									rows={1}
									class="w-full resize-none rounded-xl bg-transparent px-4 py-3 text-sm text-foreground focus:outline-none placeholder:text-muted-foreground max-h-32"
									disabled={messageSending || !task.agent_id}
								></textarea>
							</div>
							<button
								onclick={sendTaskMessage}
								aria-label="Send message"
								disabled={!messageInput.trim() || messageSending || !task.agent_id}
								class="flex h-11 w-11 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors flex-shrink-0 shadow-lg shadow-primary/20"
							>
								<svg class="h-5 w-5" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
							</button>
					</div>
				</div>
			</div>

			<!-- Right: details sidebar -->
			<div class="hidden w-72 shrink-0 space-y-4 overflow-y-auto border-l border-border p-4 lg:block">
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
							<dd class="{statusColor(task.status)}">{task.status.replaceAll('_', ' ')}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Priority</dt>
							<dd>{priorityLabel(task.priority)}</dd>
						</div>
						{#if task.agent_id}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Project</dt>
								<dd><a href="/agents/{task.agent_id}" class="underline hover:text-foreground">{sessionLabel(task.agent_id)}</a></dd>
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

				{#if task.agent_id}
					<div class="space-y-2">
						<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Project</h3>
						<a href="/agents/{task.agent_id}" class="text-sm underline hover:text-foreground">Open project</a>
					</div>
				{/if}
			</div>
		</div>
	{/if}
</div>
