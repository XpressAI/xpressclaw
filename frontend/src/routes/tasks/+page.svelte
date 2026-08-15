<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { agents, tasks } from '$lib/api';
	import type { Agent, Task, TaskCounts } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import AgentLoading from '$lib/components/AgentLoading.svelte';

	const PAGE_SIZE = 20;
	const FILTER_STATUSES = {
		attention: ['waiting_for_input', 'blocked'],
		active: ['pending', 'in_progress', 'waiting_for_input', 'blocked'],
		all: [],
		done: ['completed', 'cancelled'],
	} as const;

	let taskList = $state<Task[]>([]);
	let dependencyTasks = $state<Task[]>([]);
	let agentList = $state<Agent[]>([]);
	let counts = $state<TaskCounts | null>(null);
	let loading = $state(true);
	let page = $state(0);
	let scrollContainer = $state<HTMLDivElement>();
	let loadRequest = 0;
	let showCreate = $state(false);
	let newTitle = $state('');
	let newDesc = $state('');
	let newAgentId = $state('');
	let newPriority = $state(0);
	let newDependsOn = $state<string[]>([]);
	let newSession = $state(false);
	let filter = $state<'attention' | 'active' | 'all' | 'done'>('active');
	let searchText = $state('');
	let searchQuery = $state('');
	let searchComposing = $state(false);
	let searchTimer: ReturnType<typeof setTimeout> | undefined;
	let formError = $state('');
	let creating = $state(false);

	let totalTasks = $derived(countForFilter(filter, counts));
	let totalPages = $derived(Math.max(1, Math.ceil(totalTasks / PAGE_SIZE)));

	onMount(async () => {
		await Promise.all([load(), loadAgents(), loadDependencies()]);
	});
	onDestroy(() => clearTimeout(searchTimer));

	async function loadAgents() {
		agentList = await agents.list().catch(() => []);
		if (!newAgentId && agentList.length > 0) newAgentId = agentList[0].id;
	}

	async function loadDependencies() {
		const result = await tasks.list(undefined, undefined, {
			limit: 100,
			statuses: [...FILTER_STATUSES.active],
		}).catch(() => null);
		dependencyTasks = result?.tasks ?? [];
	}

	async function load() {
		const request = ++loadRequest;
		loading = true;
		try {
			const result = await tasks.list(undefined, undefined, {
				limit: PAGE_SIZE,
				offset: page * PAGE_SIZE,
				statuses: [...FILTER_STATUSES[filter]],
				sort: searchQuery ? 'recent' : undefined,
				search: searchQuery || undefined,
			});
			if (request !== loadRequest) return;
			counts = result.counts;
			if (result.tasks.length === 0 && page > 0 && page * PAGE_SIZE >= countForFilter(filter, result.counts)) {
				page -= 1;
				await load();
				return;
			}
			taskList = result.tasks;
		} catch {
			if (request === loadRequest) taskList = [];
		} finally {
			if (request === loadRequest) loading = false;
		}
	}

	function statusCount(key: keyof TaskCounts): number {
		if (!counts) return 0;
		return counts[key];
	}

	function countForFilter(selectedFilter: typeof filter, taskCounts: TaskCounts | null): number {
		if (!taskCounts) return 0;
		if (selectedFilter === 'attention') return taskCounts.waiting_for_input + taskCounts.blocked;
		if (selectedFilter === 'active') {
			return taskCounts.pending + taskCounts.in_progress + taskCounts.waiting_for_input + taskCounts.blocked;
		}
		if (selectedFilter === 'done') return taskCounts.completed + taskCounts.cancelled;
		return Object.values(taskCounts).reduce((total, count) => total + count, 0);
	}

	async function selectFilter(nextFilter: typeof filter) {
		if (filter === nextFilter) return;
		filter = nextFilter;
		page = 0;
		taskList = [];
		await load();
		scrollContainer?.scrollTo({ top: 0 });
	}

	async function goToPage(nextPage: number) {
		if (loading || nextPage < 0 || nextPage >= totalPages || nextPage === page) return;
		page = nextPage;
		taskList = [];
		await load();
		scrollContainer?.scrollTo({ top: 0 });
	}

	function handleSearchInput(event: Event) {
		searchText = (event.currentTarget as HTMLInputElement).value;
		if (searchComposing || (event as InputEvent).isComposing) return;
		scheduleSearch();
	}

	function scheduleSearch() {
		clearTimeout(searchTimer);
		searchTimer = setTimeout(applySearch, 250);
	}

	function handleSearchKeydown(event: KeyboardEvent) {
		if (event.key !== 'Enter' || event.isComposing || searchComposing || event.keyCode === 229) return;
		event.preventDefault();
		clearTimeout(searchTimer);
		applySearch();
	}

	function handleSearchCompositionStart() {
		searchComposing = true;
		clearTimeout(searchTimer);
	}

	function handleSearchCompositionEnd(event: CompositionEvent) {
		const input = event.currentTarget as HTMLInputElement;
		clearTimeout(searchTimer);
		// Some browsers end composition before dispatching the final input event.
		// Deferring one tick reads the committed value and also keeps the Enter
		// used to accept an IME candidate from submitting the search.
		searchTimer = setTimeout(() => {
			searchComposing = false;
			searchText = input.value;
			scheduleSearch();
		}, 0);
	}

	function applySearch() {
		const nextSearch = searchText.trim();
		if (nextSearch === searchQuery && page === 0) return;
		searchQuery = nextSearch;
		page = 0;
		taskList = [];
		void load().then(() => scrollContainer?.scrollTo({ top: 0 }));
	}

	function clearSearch() {
		clearTimeout(searchTimer);
		searchComposing = false;
		searchText = '';
		searchQuery = '';
		page = 0;
		taskList = [];
		void load().then(() => scrollContainer?.scrollTo({ top: 0 }));
	}

	/** Existing incomplete tasks that can be selected as dependencies. */
	let availableDeps = $derived(dependencyTasks);

	function toggleDep(id: string) {
		if (newDependsOn.includes(id)) {
			newDependsOn = newDependsOn.filter(d => d !== id);
		} else {
			newDependsOn = [...newDependsOn, id];
		}
	}

	async function createTask() {
		if (!newTitle.trim() || !newAgentId || creating) return;
		creating = true;
		formError = '';
		try {
			// Batch creation records dependencies before the dispatcher can
			// claim the task, even when the batch contains only one item.
			await tasks.createBatch({ tasks: [{
				ref: 'task',
				title: newTitle.trim(),
				description: newDesc.trim() || undefined,
				agent_id: newAgentId,
				priority: newPriority || undefined,
				new_session: newDependsOn.length === 0 && newSession,
				depends_on: newDependsOn.length > 0 ? newDependsOn : undefined
			}] });
			newTitle = '';
			newDesc = '';
			newPriority = 0;
			newDependsOn = [];
			newSession = false;
			showCreate = false;
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		} finally {
			creating = false;
		}
	}

	async function openCreate() {
		if (agentList.length === 0) return;
		formError = '';
		if (!newAgentId) newAgentId = agentList[0].id;
		showCreate = !showCreate;
		if (showCreate) await loadDependencies();
	}

	async function cancelTask(id: string) {
		await tasks.updateStatus(id, 'cancelled');
		await Promise.all([load(), loadDependencies()]);
	}

	async function deleteTask(id: string) {
		if (!confirm('Delete this task?')) return;
		await tasks.delete(id);
		await Promise.all([load(), loadDependencies()]);
	}

	function agentName(agentId: string | null): string | null {
		if (!agentId) return null;
		const agent = agentList.find((a) => a.id === agentId);
		return agent?.title || agent?.name || agentId;
	}

	function statusMeta(status: string): { label: string; dot: string; tone: string; pill: string; glyph: string } {
		if (status === 'in_progress') return { label: 'Working', dot: 'bg-blue-400 animate-pulse', tone: 'text-blue-500 dark:text-blue-300', pill: 'bg-blue-500/10', glyph: '2' };
		if (status === 'awaiting_review') return { label: 'Awaiting review', dot: 'bg-violet-400', tone: 'text-violet-600 dark:text-violet-300', pill: 'bg-violet-500/10', glyph: 'R' };
		if (status === 'waiting_for_subtasks') return { label: 'Waiting on subtasks', dot: 'bg-amber-400', tone: 'text-amber-600 dark:text-amber-300', pill: 'bg-amber-500/10', glyph: '↳' };
		if (status === 'idle') return { label: 'Not running', dot: 'bg-muted-foreground', tone: 'text-muted-foreground', pill: 'bg-muted', glyph: '–' };
		if (status === 'pending') return { label: 'Queued', dot: 'bg-amber-400', tone: 'text-amber-600 dark:text-amber-300', pill: 'bg-amber-500/10', glyph: '1' };
		if (status === 'waiting_for_input') return { label: 'Waiting for you', dot: 'bg-orange-400 animate-pulse', tone: 'text-orange-600 dark:text-orange-300', pill: 'bg-orange-500/10', glyph: '?' };
		if (status === 'blocked') return { label: 'Blocked', dot: 'bg-red-400', tone: 'text-red-600 dark:text-red-300', pill: 'bg-red-500/10', glyph: '!' };
		if (status === 'completed') return { label: 'Completed', dot: 'bg-emerald-400', tone: 'text-emerald-700 dark:text-emerald-300', pill: 'bg-emerald-500/10', glyph: '✓' };
		return { label: 'Cancelled', dot: 'bg-muted-foreground', tone: 'text-muted-foreground', pill: 'bg-muted', glyph: '×' };
	}
</script>

<div bind:this={scrollContainer} data-tasks-scroll class="workspace-scroll-y h-full">
	<div class="space-y-6 p-4 sm:p-6">
	<div class="flex items-center justify-between gap-3">
		<div>
			<h1 class="text-2xl font-bold">Tasks</h1>
			{#if counts && searchQuery}
				<p class="text-sm text-muted-foreground mt-1">
					{countForFilter('all', counts)} matching {countForFilter('all', counts) === 1 ? 'task' : 'tasks'}
				</p>
			{:else if counts}
				<p class="text-sm text-muted-foreground mt-1">
					{statusCount('pending')} pending, {statusCount('in_progress')} in progress, {statusCount('completed')} completed
				</p>
			{/if}
		</div>
		<button
			onclick={openCreate}
			disabled={agentList.length === 0}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors disabled:cursor-not-allowed disabled:opacity-50"
		>
			New Task
		</button>
	</div>

	{#if agentList.length === 0}
		<div class="rounded-lg border border-dashed border-border bg-card p-5 text-sm text-muted-foreground">
			Tasks need an agent so they know which workspace and harness to use. <a href="/setup?mode=add-session" class="font-medium text-primary hover:underline">Create an agent</a> first.
		</div>
	{/if}

	{#if showCreate}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<input
				type="text"
				placeholder="Task title..."
				bind:value={newTitle}
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<textarea
				placeholder="Description (optional)..."
				bind:value={newDesc}
				rows="2"
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring resize-none"
			></textarea>
			<div class="flex flex-col gap-3 sm:flex-row">
				<div class="flex-1">
					<label class="block text-xs text-muted-foreground mb-1">Agent
					<select
						bind:value={newAgentId}
						class="mt-1 w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						{#each agentList as agent}
							<option value={agent.id}>{agent.title || agent.name}</option>
						{/each}
					</select>
					</label>
				</div>
				<div class="w-24">
					<label class="block text-xs text-muted-foreground mb-1">Priority
					<select
						bind:value={newPriority}
						class="mt-1 w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						<option value={0}>Normal</option>
						<option value={5}>High</option>
						<option value={10}>Urgent</option>
					</select>
					</label>
				</div>
			</div>
			{#if availableDeps.length > 0}
				<div>
					<div class="block text-xs text-muted-foreground mb-1">Depends on (optional)</div>
					<div class="flex flex-wrap gap-1.5 max-h-24 overflow-y-auto">
						{#each availableDeps as dep}
							<button
								type="button"
								onclick={() => toggleDep(dep.id)}
								class="rounded-md border px-2 py-1 text-xs transition-colors
									{newDependsOn.includes(dep.id)
										? 'border-primary bg-primary/10 text-primary'
										: 'border-border text-muted-foreground hover:border-primary/50'}"
							>
								{dep.title}
							</button>
						{/each}
					</div>
					{#if newDependsOn.length > 0}
						<div class="text-xs text-muted-foreground mt-1">
							This task will wait, then branch from the conversation it depends on.
						</div>
					{/if}
				</div>
			{/if}
			<label class="flex items-start gap-2 text-xs text-muted-foreground {newDependsOn.length > 0 ? 'opacity-50' : ''}">
				<input type="checkbox" bind:checked={newSession} disabled={newDependsOn.length > 0} class="mt-0.5 h-3.5 w-3.5 accent-primary" />
				<span><strong class="font-medium text-foreground">Start a fresh conversation</strong><br />Otherwise this branches from the agent’s active conversation when supported.</span>
			</label>
			{#if formError}<p class="text-xs text-destructive">{formError}</p>{/if}
			<div class="flex gap-2">
				<button
					onclick={createTask}
					disabled={!newTitle.trim() || !newAgentId || creating}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
				>
					{creating ? 'Queuing…' : 'Create and queue'}
				</button>
				<button
					onclick={() => (showCreate = false)}
					class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent"
				>
					Cancel
				</button>
			</div>
		</div>
	{/if}

	<div class="ai-control relative overflow-hidden">
		<svg class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24" aria-hidden="true">
			<circle cx="11" cy="11" r="7" />
			<path d="m20 20-3.5-3.5" />
		</svg>
		<input
			type="search"
			value={searchText}
			oninput={handleSearchInput}
			onkeydown={handleSearchKeydown}
			oncompositionstart={handleSearchCompositionStart}
			oncompositionend={handleSearchCompositionEnd}
			maxlength="200"
			aria-label="Search tasks"
			placeholder="Search task titles, descriptions, and conversations…"
			class="w-full bg-transparent py-2.5 pl-9 pr-10 text-sm placeholder:text-muted-foreground focus:outline-none"
		/>
		{#if searchText}
			<button
				type="button"
				onclick={clearSearch}
				aria-label="Clear task search"
				class="absolute right-2 top-1/2 -translate-y-1/2 rounded-md px-2 py-1 text-lg leading-none text-muted-foreground hover:bg-accent hover:text-foreground"
			>×</button>
		{/if}
	</div>

	<div class="flex w-fit max-w-full gap-1 overflow-x-auto rounded-full bg-[hsl(var(--field))] p-0.5">
		{#each [
			{ id: 'attention', label: 'Needs you', count: statusCount('waiting_for_input') + statusCount('blocked') },
			{ id: 'active', label: 'Active', count: countForFilter('active', counts) },
			{ id: 'all', label: 'All', count: countForFilter('all', counts) },
			{ id: 'done', label: 'Done', count: countForFilter('done', counts) }
		] as item}
			<button onclick={() => selectFilter(item.id as typeof filter)} class="shrink-0 rounded-full px-3 py-1.5 text-xs font-medium transition-all {filter === item.id ? 'bg-card text-foreground shadow-[var(--shadow-control)]' : 'text-muted-foreground hover:text-foreground'}">
				{item.label} <span class="ml-1 opacity-70">{item.count}</span>
			</button>
		{/each}
	</div>

	<div data-task-list class="space-y-2">
		{#if loading && taskList.length === 0}
			<div class="flex justify-center px-4 py-16"><AgentLoading label="Loading tasks" /></div>
		{:else}
			{#each taskList as task (task.id)}
				{@const displayStatus = task.activity_status ?? task.status}
				{@const meta = statusMeta(displayStatus)}
				<a data-task-row data-task-activity-status={displayStatus} href="/tasks/{task.id}" class="group ai-card flex min-h-14 items-start gap-3 px-3 py-3 transition-colors hover:bg-[hsl(var(--hover))]">
					<span class="relative mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold {meta.tone} shadow-[inset_0_0_0_1.5px_hsl(var(--border-strong))]">
						{meta.glyph}
						{#if displayStatus === 'in_progress'}<span class="absolute inset-0 rounded-full border-2 border-transparent border-t-blue-400 animate-spin"></span>{/if}
					</span>
					<div class="min-w-0 flex-1">
						<div class="flex items-start justify-between gap-3">
							<h2 class="min-w-0 truncate text-sm font-medium">{task.title}</h2>
							<span class="ai-status-pill shrink-0 {meta.pill} {meta.tone}">{meta.label}</span>
						</div>
						{#if task.description}<p class="mt-1 line-clamp-2 text-xs text-muted-foreground">{task.description}</p>{/if}
						<div class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground">
							<span>{agentName(task.agent_id) ?? 'Unassigned'}</span><span>·</span><span>{timeAgo(task.updated_at)}</span>
							{#if task.blocked_by && task.blocked_by.length > 0}<span class="text-amber-500">· Waiting on {task.blocked_by.length}</span>{/if}
							{#if task.priority >= 5}<span class="text-orange-400">· High priority</span>{/if}
						</div>
					</div>
					<div class="flex shrink-0 items-center gap-2">
						{#if !['completed', 'cancelled'].includes(task.status)}<button onclick={(event) => { event.preventDefault(); event.stopPropagation(); cancelTask(task.id); }} class="hidden text-xs text-muted-foreground hover:text-destructive sm:block">Cancel</button>{/if}
						<button onclick={(event) => { event.preventDefault(); event.stopPropagation(); deleteTask(task.id); }} aria-label="Delete task" class="hidden text-lg leading-none text-muted-foreground hover:text-destructive sm:block">×</button>
					</div>
				</a>
			{:else}
				<div class="ai-card px-4 py-16 text-center text-sm text-muted-foreground">
					{searchQuery ? `No tasks match “${searchQuery}”.` : 'No tasks in this view.'}
				</div>
			{/each}
		{/if}
	</div>

	{#if totalPages > 1}
		<nav aria-label="Task pages" class="flex items-center justify-between gap-3 pb-2">
			<button
				type="button"
				onclick={() => goToPage(page - 1)}
				disabled={page === 0 || loading}
				class="rounded-md border border-border px-3 py-2 text-xs font-medium hover:bg-accent disabled:cursor-not-allowed disabled:opacity-40"
			>
				Previous
			</button>
			<span class="text-xs text-muted-foreground">Page {page + 1} of {totalPages}</span>
			<button
				type="button"
				onclick={() => goToPage(page + 1)}
				disabled={page + 1 >= totalPages || loading}
				class="rounded-md border border-border px-3 py-2 text-xs font-medium hover:bg-accent disabled:cursor-not-allowed disabled:opacity-40"
			>
				Next
			</button>
		</nav>
	{/if}
	</div>
</div>
