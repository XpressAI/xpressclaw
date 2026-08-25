<script lang="ts">
	import { onMount, tick } from 'svelte';
	import {
		dashboard,
		type DashboardActiveWork,
		type DashboardEvent,
		type DashboardRange,
		type DashboardSnapshot,
	} from '$lib/api';
	import DashboardChart from '$lib/components/dashboard/DashboardChart.svelte';
	import { serverTimestampMs } from '$lib/serverTime';
	import { timeAgo } from '$lib/utils';

	type ChartMode = 'context' | 'tools' | 'code';
	type LiveState = 'connecting' | 'live' | 'reconnecting' | 'offline';
	const MAX_FEED_EVENTS = 400;

	let snapshot = $state<DashboardSnapshot | null>(null);
	let feed = $state<DashboardEvent[]>([]);
	let projectId = $state('');
	let range = $state<DashboardRange>('24h');
	let chartMode = $state<ChartMode>('context');
	let loading = $state(true);
	let loadingOlder = $state(false);
	let hasMoreOlder = $state(false);
	let historyLimitReached = $state(false);
	let olderCursor = $state<number | null>(null);
	let error = $state('');
	let liveState = $state<LiveState>('connecting');
	let mounted = $state(false);
	let now = $state(Date.now());
	let feedScroller = $state<HTMLDivElement>();
	let atLiveEdge = $state(true);
	let announcement = $state('');
	let newEventIds = $state<Set<string>>(new Set());
	let eventSource: EventSource | null = null;
	let refreshTimer: ReturnType<typeof setTimeout> | null = null;
	let loadGeneration = 0;
	let paginationGeneration = 0;
	let clockTimer: ReturnType<typeof setInterval> | null = null;
	const animationTimers = new Set<ReturnType<typeof setTimeout>>();

	let counters = $derived(snapshot?.counters ?? {
		working_agents: 0,
		active_work: 0,
		needs_attention: 0,
		tool_calls: 0,
	});
	let projects = $derived(snapshot?.projects ?? []);
	let selectedProjectName = $derived(projects.find((project) => project.id === projectId)?.name ?? 'All Projects');
	let metricDescription = $derived(chartMode === 'context'
		? 'ACP context-window occupancy across active response turns. This is not billing-grade token usage.'
		: chartMode === 'tools'
			? 'Canonical tool-call starts. Progress updates are not counted twice.'
			: 'Lines changed relative to each turn baseline. Pre-existing dirty changes are excluded.');

	$effect(() => {
		projectId;
		range;
		if (!mounted) return;
		void loadDashboard();
	});

	onMount(() => {
		mounted = true;
		const updateClock = () => (now = Date.now());
		const updateClockState = () => {
			if (clockTimer) clearInterval(clockTimer);
			clockTimer = null;
			if (!document.hidden) {
				updateClock();
				clockTimer = setInterval(updateClock, 1_000);
			}
		};
		const wentOffline = () => (liveState = 'offline');
		const cameOnline = () => {
			liveState = 'reconnecting';
			if (!eventSource && snapshot) connectStream(snapshot.cursor);
		};
		updateClockState();
		document.addEventListener('visibilitychange', updateClockState);
		window.addEventListener('offline', wentOffline);
		window.addEventListener('online', cameOnline);
		return () => {
			mounted = false;
			eventSource?.close();
			if (clockTimer) clearInterval(clockTimer);
			if (refreshTimer) clearTimeout(refreshTimer);
			for (const timer of animationTimers) clearTimeout(timer);
			document.removeEventListener('visibilitychange', updateClockState);
			window.removeEventListener('offline', wentOffline);
			window.removeEventListener('online', cameOnline);
		};
	});

	async function loadDashboard() {
		const generation = ++loadGeneration;
		paginationGeneration += 1;
		loadingOlder = false;
		hasMoreOlder = false;
		historyLimitReached = false;
		olderCursor = null;
		eventSource?.close();
		eventSource = null;
		loading = true;
		error = '';
		liveState = navigator.onLine ? 'connecting' : 'offline';
		try {
			const loaded = await dashboard.snapshot(projectId, range);
			if (!mounted || generation !== loadGeneration) return;
			snapshot = loaded;
			feed = uniqueEvents(loaded.feed.events).slice(0, 160);
			olderCursor = loaded.feed.next_before ?? feed.at(-1)?.cursor ?? null;
			hasMoreOlder = loaded.feed.has_more;
			loading = false;
			connectStream(loaded.cursor);
		} catch (caught) {
			if (generation !== loadGeneration) return;
			loading = false;
			error = caught instanceof Error ? caught.message : 'Control center could not be loaded.';
			liveState = navigator.onLine ? 'reconnecting' : 'offline';
		}
	}

	function connectStream(after: number) {
		eventSource?.close();
		if (!mounted || !navigator.onLine) {
			liveState = 'offline';
			return;
		}
		liveState = 'connecting';
		const source = new EventSource(dashboard.streamUrl(projectId, range, after));
		eventSource = source;
		source.onopen = () => {
			if (eventSource === source) liveState = 'live';
		};
		source.addEventListener('dashboard', (event) => {
			if (eventSource !== source) return;
			try {
				insertLiveEvent(JSON.parse((event as MessageEvent).data) as DashboardEvent);
			} catch {
				liveState = 'reconnecting';
			}
		});
		source.addEventListener('reset', () => {
			if (eventSource === source) void loadDashboard();
		});
		source.addEventListener('stream_error', () => {
			if (eventSource === source) liveState = 'reconnecting';
		});
		source.onerror = () => {
			if (eventSource === source) liveState = navigator.onLine ? 'reconnecting' : 'offline';
		};
	}

	async function insertLiveEvent(event: DashboardEvent) {
		const scroller = feedScroller;
		const follow = atLiveEdge;
		const previousHeight = scroller?.scrollHeight ?? 0;
		const previousTop = scroller?.scrollTop ?? 0;
		const combined = uniqueEvents([event, ...feed]);
		feed = combined.slice(0, MAX_FEED_EVENTS);
		if (combined.length > MAX_FEED_EVENTS || (combined.length >= MAX_FEED_EVENTS && hasMoreOlder)) {
			hasMoreOlder = false;
			historyLimitReached = true;
		}
		newEventIds = new Set(newEventIds).add(event.event_id);
		announcement = `${event.source_label} updated ${event.target_title}`;
		const timer = setTimeout(() => {
			const next = new Set(newEventIds);
			next.delete(event.event_id);
			newEventIds = next;
			animationTimers.delete(timer);
		}, 1_600);
		animationTimers.add(timer);
		await tick();
		if (scroller) {
			if (follow) scroller.scrollTo({ top: 0, behavior: reducedMotion() ? 'auto' : 'smooth' });
			else scroller.scrollTop = previousTop + scroller.scrollHeight - previousHeight;
		}
		scheduleSummaryRefresh();
	}

	function scheduleSummaryRefresh() {
		if (refreshTimer) return;
		refreshTimer = setTimeout(async () => {
			refreshTimer = null;
			const refreshProjectId = projectId;
			const refreshRange = range;
			try {
				const refreshed = await dashboard.snapshot(refreshProjectId, refreshRange, 20);
				if (!mounted || refreshProjectId !== projectId || refreshRange !== range) return;
				snapshot = { ...refreshed, feed: snapshot?.feed ?? refreshed.feed };
			} catch {
				// EventSource owns connection state; a later event or reconnect retries.
			}
		}, 900);
	}

	async function loadOlder() {
		const before = olderCursor;
		if (!hasMoreOlder || before === null || loadingOlder) return;
		const generation = loadGeneration;
		const request = ++paginationGeneration;
		const requestProjectId = projectId;
		const requestRange = range;
		loadingOlder = true;
		try {
			const page = await dashboard.feed(requestProjectId, requestRange, before);
			if (
				!mounted || request !== paginationGeneration || generation !== loadGeneration
				|| requestProjectId !== projectId || requestRange !== range
			) return;
			const combined = uniqueEvents([...feed, ...page.events]);
			const reachedLimit = combined.length > MAX_FEED_EVENTS
				|| (combined.length >= MAX_FEED_EVENTS && page.has_more);
			feed = combined.slice(0, MAX_FEED_EVENTS);
			olderCursor = page.next_before ?? page.events.at(-1)?.cursor ?? olderCursor;
			historyLimitReached = reachedLimit;
			hasMoreOlder = page.has_more && !reachedLimit;
			if (snapshot) snapshot = { ...snapshot, feed: { ...page, has_more: hasMoreOlder } };
		} catch (caught) {
			if (request !== paginationGeneration || generation !== loadGeneration) return;
			error = caught instanceof Error ? caught.message : 'Older activity could not be loaded.';
		} finally {
			if (request === paginationGeneration) loadingOlder = false;
		}
	}

	function uniqueEvents(events: DashboardEvent[]): DashboardEvent[] {
		const latest = new Map<string, DashboardEvent>();
		for (const event of events) {
			const current = latest.get(event.event_id);
			if (!current || event.cursor > current.cursor) latest.set(event.event_id, event);
		}
		return [...latest.values()].sort((left, right) => right.cursor - left.cursor);
	}

	function updateLiveEdge() {
		atLiveEdge = (feedScroller?.scrollTop ?? 0) < 36;
	}

	function duration(work: DashboardActiveWork): string {
		const anchor = serverTimestampMs(work.phase === 'working' ? work.started_at : work.queued_at);
		if (anchor === null) return '';
		const seconds = Math.max(0, Math.floor((now - anchor) / 1_000));
		if (seconds < 60) return `${seconds}s`;
		const minutes = Math.floor(seconds / 60);
		if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
		return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
	}

	function eventLabel(event: DashboardEvent): string {
		const labels: Record<string, string> = {
			agent_response: 'Response',
			task_message: 'Task message',
			conversation_message: 'Conversation message',
			tool_call: 'Tool',
			progress: 'Progress',
			completion: 'Completed',
			failure: 'Failed',
			waiting_for_input: 'Needs input',
			cancellation: 'Cancelled',
			status_change: 'Status',
		};
		return labels[event.event_kind] ?? 'Activity';
	}

	function eventGlyph(event: DashboardEvent): string {
		if (event.needs_attention) return '!';
		if (event.event_kind === 'tool_call') return '⌁';
		if (event.event_kind === 'completion') return '✓';
		if (event.event_kind === 'failure') return '×';
		if (event.target_type === 'conversation') return '◌';
		return '↗';
	}

	function reducedMotion(): boolean {
		return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
	}

	function retry() {
		void loadDashboard();
	}
</script>

<svelte:head><title>Control center · XpressClaw</title></svelte:head>

<div class="control-center relative h-full min-h-0 overflow-y-auto bg-background" data-dashboard-page>
	<div class="control-grid pointer-events-none absolute inset-0" aria-hidden="true"></div>
	<div class="control-glow pointer-events-none absolute left-1/2 top-0 h-72 w-[70rem] max-w-full -translate-x-1/2" aria-hidden="true"></div>

	<div class="relative mx-auto flex min-h-full w-full max-w-[1560px] flex-col gap-4 px-4 py-5 sm:px-6 lg:gap-5 lg:px-8 lg:py-7">
		<header class="flex flex-col gap-4 xl:flex-row xl:items-end xl:justify-between">
			<div>
				<div class="mb-2 flex items-center gap-2.5">
					<span class="live-orbit" aria-hidden="true"><span></span></span>
					<span class="text-[10px] font-semibold uppercase tracking-[0.22em] text-primary">Instance overview</span>
				</div>
				<h1 class="text-2xl font-semibold tracking-[-0.035em] sm:text-3xl">Control center</h1>
				<p class="mt-1 max-w-xl text-sm text-muted-foreground">Live work, responses, and attention signals across your XpressClaw Projects.</p>
			</div>
			<div class="flex flex-wrap items-center gap-2.5">
				<div data-dashboard-connection role="status" aria-live="polite" class="live-badge {liveState}">
					<span class="status-beacon" aria-hidden="true"></span>
					<span>{liveState === 'live' ? 'Live' : liveState === 'offline' ? 'Offline' : liveState === 'reconnecting' ? 'Reconnecting' : 'Connecting'}</span>
				</div>
				<label class="sr-only" for="dashboard-project">Project scope</label>
				<select id="dashboard-project" bind:value={projectId} class="h-9 max-w-[13rem] rounded-lg border border-border bg-card px-3 text-xs font-medium shadow-sm outline-none focus:ring-2 focus:ring-ring/40">
					<option value="">All Projects</option>
					{#each projects as project (project.id)}<option value={project.id}>{project.name}</option>{/each}
				</select>
				<div class="flex h-9 items-center rounded-lg border border-border bg-card p-1 shadow-sm" aria-label="Time range">
					{#each ['1h', '24h', '7d'] as option}
						<button type="button" onclick={() => (range = option as DashboardRange)} aria-pressed={range === option} class="h-7 rounded-md px-2.5 text-[11px] font-semibold transition {range === option ? 'bg-foreground text-background shadow-sm' : 'text-muted-foreground hover:bg-accent hover:text-foreground'}">{option}</button>
					{/each}
				</div>
			</div>
		</header>

		{#if error}
			<div role="alert" class="flex items-center gap-3 rounded-xl border border-red-500/25 bg-[hsl(var(--danger-tint))] px-4 py-3 text-sm">
				<span class="flex h-7 w-7 items-center justify-center rounded-full bg-red-500/10 font-bold text-red-500">!</span>
				<span class="min-w-0 flex-1">{error}</span>
				<button type="button" onclick={retry} class="rounded-lg border border-border bg-card px-3 py-1.5 text-xs font-semibold hover:bg-accent">Retry</button>
				<button type="button" onclick={() => (error = '')} class="flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Dismiss error">×</button>
			</div>
		{/if}

		{#if loading && !snapshot}
			<div class="grid grid-cols-2 gap-3 lg:grid-cols-4" aria-label="Loading dashboard summary">
				{#each Array(4) as _}<div class="dashboard-card h-28 animate-pulse"><div class="m-4 h-3 w-24 rounded bg-muted"></div><div class="mx-4 mt-6 h-8 w-14 rounded bg-muted"></div></div>{/each}
			</div>
			<div class="grid gap-4 xl:grid-cols-[minmax(0,1.6fr)_minmax(20rem,.8fr)]">
				<div class="dashboard-card h-[22rem] animate-pulse bg-card/70"></div>
				<div class="dashboard-card h-[22rem] animate-pulse bg-card/70"></div>
			</div>
		{:else}
			<section class="grid grid-cols-2 gap-3 lg:grid-cols-4" aria-label="Live summary">
				<div class="dashboard-card kpi-card" data-kpi="working-agents">
					<div class="kpi-top"><span>Working Agents</span><span class="kpi-icon working" aria-hidden="true">◎</span></div>
					<div class="kpi-value">{counters.working_agents}</div>
					<div class="kpi-note"><span class="mini-beacon"></span>{counters.working_agents === 1 ? 'Agent responding now' : 'Agents responding now'}</div>
				</div>
				<div class="dashboard-card kpi-card" data-kpi="active-work">
					<div class="kpi-top"><span>Active work</span><span class="kpi-icon" aria-hidden="true">↗</span></div>
					<div class="kpi-value">{counters.active_work}</div>
					<div class="kpi-note">Queued and active turns</div>
				</div>
				<div class="dashboard-card kpi-card {counters.needs_attention > 0 ? 'attention' : ''}" data-kpi="needs-attention">
					<div class="kpi-top"><span>Needs you</span><span class="kpi-icon attention" aria-hidden="true">!</span></div>
					<div class="kpi-value">{counters.needs_attention}</div>
					<div class="kpi-note">Waiting or blocked</div>
				</div>
				<div class="dashboard-card kpi-card" data-kpi="tool-calls">
					<div class="kpi-top"><span>Tool calls</span><span class="kpi-icon" aria-hidden="true">⌁</span></div>
					<div class="kpi-value">{counters.tool_calls.toLocaleString()}</div>
					<div class="kpi-note">Canonical starts · {range}</div>
				</div>
			</section>

			{#if snapshot && snapshot.attention.length > 0}
				<section class="attention-rail" aria-labelledby="attention-heading">
					<div class="flex items-center gap-2 px-1"><span class="attention-mark">!</span><h2 id="attention-heading" class="text-xs font-semibold uppercase tracking-[0.14em] text-[hsl(var(--warning))]">Needs your attention</h2></div>
					<div class="mt-2 flex gap-2 overflow-x-auto pb-1 scrollbar-hide">
						{#each snapshot.attention as item (item.id)}
							<a href={item.href} class="attention-chip group">
								<span class="attention-pulse" aria-hidden="true"></span>
								<span class="min-w-0"><span class="block truncate text-xs font-semibold">{item.target_title}</span><span class="block truncate text-[10px] text-muted-foreground">{item.project_name ?? 'No Project'} · {item.summary}</span></span>
								<span class="ml-auto text-muted-foreground transition group-hover:translate-x-0.5 group-hover:text-foreground" aria-hidden="true">→</span>
							</a>
						{/each}
					</div>
				</section>
			{/if}

			<div class="grid min-h-0 gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(19rem,.75fr)]">
				<section class="dashboard-card order-3 overflow-hidden xl:order-none" aria-labelledby="activity-chart-heading">
					<div class="flex flex-col gap-3 border-b border-border/70 px-4 py-3.5 sm:flex-row sm:items-center sm:justify-between">
						<div><h2 id="activity-chart-heading" class="text-sm font-semibold">Activity signal</h2><p class="mt-0.5 text-[11px] text-muted-foreground">{selectedProjectName} · {metricDescription}</p></div>
						<div class="flex items-center rounded-lg bg-muted/70 p-1" aria-label="Chart metric">
							{#each [{ id: 'context', label: 'Context' }, { id: 'tools', label: 'Tools' }, { id: 'code', label: 'Code' }] as metric}
								<button type="button" onclick={() => (chartMode = metric.id as ChartMode)} aria-pressed={chartMode === metric.id} class="rounded-md px-2.5 py-1.5 text-[11px] font-semibold transition {chartMode === metric.id ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'}">{metric.label}</button>
							{/each}
						</div>
					</div>
					<div class="px-2 pb-1 pt-3 sm:px-4"><DashboardChart points={snapshot?.series ?? []} mode={chartMode} /></div>
				</section>

				<section class="dashboard-card order-1 flex min-h-[20rem] flex-col overflow-hidden xl:order-none" aria-labelledby="active-now-heading" data-active-now>
					<div class="flex items-center justify-between border-b border-border/70 px-4 py-3.5">
						<div><h2 id="active-now-heading" class="text-sm font-semibold">Active now</h2><p class="mt-0.5 text-[11px] text-muted-foreground">Current Agent response cycles</p></div>
					<span class="rounded-full bg-primary/10 px-2 py-1 text-[10px] font-semibold text-primary">{snapshot?.active_work.length ?? 0} live</span>
					</div>
					<div class="workspace-scroll-y min-h-0 flex-1 p-2">
						{#if snapshot && snapshot.active_work.length > 0}
							{#each snapshot.active_work as work (work.work_id)}
								<a href={work.href} class="active-row group">
									<div class="agent-mark"><span>{work.agent_name.slice(0, 2).toUpperCase()}</span><i class={work.phase}></i></div>
									<div class="min-w-0 flex-1">
										<div class="flex items-center gap-2"><span class="truncate text-xs font-semibold">{work.agent_name}</span><span class="shrink-0 text-[10px] text-muted-foreground">{work.project_name ?? 'No Project'}</span></div>
										<div class="mt-1 truncate text-xs text-foreground/85">{work.target_title}</div>
										<div class="mt-1 flex min-w-0 items-center gap-1.5 text-[10px] text-muted-foreground"><span class="truncate">{work.activity}</span><span aria-hidden="true">·</span><span class="shrink-0 font-mono">{work.phase === 'queued' ? 'queued' : 'working'} {duration(work)}</span></div>
									</div>
									<span class="mt-2 text-muted-foreground transition group-hover:translate-x-0.5 group-hover:text-primary" aria-hidden="true">→</span>
								</a>
							{/each}
						{:else}
							<div class="flex min-h-[16rem] flex-col items-center justify-center px-6 text-center">
								<div class="quiet-radar" aria-hidden="true"><span></span></div>
								<h3 class="mt-4 text-sm font-semibold">The instance is quiet</h3>
								<p class="mt-1 max-w-xs text-xs leading-5 text-muted-foreground">No Agents are queued or responding in this scope. New work will appear here immediately.</p>
							</div>
						{/if}
					</div>
				</section>

			<section class="dashboard-card order-2 flex min-h-[30rem] flex-1 flex-col overflow-hidden xl:order-none xl:col-span-2" aria-labelledby="live-feed-heading">
				<div class="flex items-center justify-between border-b border-border/70 px-4 py-3.5">
					<div><div class="flex items-center gap-2"><h2 id="live-feed-heading" class="text-sm font-semibold">Live feed</h2><span class="mini-beacon" aria-hidden="true"></span></div><p class="mt-0.5 text-[11px] text-muted-foreground">Newest activity first · safe summaries only</p></div>
					{#if !atLiveEdge}<button type="button" onclick={() => feedScroller?.scrollTo({ top: 0, behavior: reducedMotion() ? 'auto' : 'smooth' })} class="rounded-lg border border-border bg-card px-2.5 py-1.5 text-[10px] font-semibold shadow-sm hover:bg-accent">Jump to live</button>{/if}
				</div>
				<div bind:this={feedScroller} onscroll={updateLiveEdge} data-live-feed class="workspace-scroll-y min-h-0 flex-1 overscroll-contain">
					{#if feed.length > 0}
						<div class="divide-y divide-border/60">
							{#each feed as event (event.event_id)}
								<a href={event.href} data-feed-event={event.event_id} data-new-event={newEventIds.has(event.event_id)} class="feed-row group {event.needs_attention ? 'needs-attention' : ''} {newEventIds.has(event.event_id) ? 'arriving' : ''}">
									<div class="event-glyph {event.severity}" aria-hidden="true">{eventGlyph(event)}</div>
									<div class="min-w-0 flex-1">
										<div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
											<span class="text-xs font-semibold">{event.source_label}</span>
											<span class="event-kind {event.needs_attention ? 'attention' : ''}">{eventLabel(event)}</span>
											<span class="truncate text-[10px] text-muted-foreground">{event.project_name ?? 'No Project'}</span>
											<time class="ml-auto shrink-0 text-[10px] text-muted-foreground" datetime={event.occurred_at} title={new Date(serverTimestampMs(event.occurred_at) ?? 0).toLocaleString()}>{timeAgo(event.occurred_at)}</time>
										</div>
										<div class="mt-1 flex min-w-0 items-baseline gap-2"><span class="truncate text-xs font-medium text-foreground/90">{event.target_title}</span><span class="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground">{event.target_type}</span></div>
										<p class="mt-1 line-clamp-2 text-xs leading-5 text-muted-foreground">{event.preview}</p>
									</div>
									<span class="mt-3 text-muted-foreground opacity-0 transition group-hover:translate-x-0.5 group-hover:text-primary group-hover:opacity-100" aria-hidden="true">→</span>
								</a>
							{/each}
						</div>
						{#if hasMoreOlder}
							<div class="flex justify-center p-4"><button type="button" onclick={loadOlder} disabled={loadingOlder} class="rounded-lg border border-border bg-card px-4 py-2 text-xs font-semibold shadow-sm hover:bg-accent disabled:opacity-50">{loadingOlder ? 'Loading…' : 'Load earlier activity'}</button></div>
						{:else if historyLimitReached}
							<div class="border-t border-border/50 p-4 text-center text-[11px] text-muted-foreground" role="status" data-feed-limit>Showing the latest {MAX_FEED_EVENTS} events in this view. Narrow the Project or time range to explore a different slice.</div>
						{/if}
					{:else}
						<div class="flex min-h-[26rem] flex-col items-center justify-center px-6 text-center">
							<div class="empty-signal" aria-hidden="true"><i></i><i></i><i></i></div>
							<h3 class="mt-5 text-sm font-semibold">No activity in this window</h3>
							<p class="mt-1 max-w-sm text-xs leading-5 text-muted-foreground">Messages, safe Agent progress, tool starts, completions, failures, and requests for input will stream into this feed.</p>
							<a href="/" class="mt-4 rounded-lg bg-primary px-3.5 py-2 text-xs font-semibold text-primary-foreground shadow-sm hover:bg-primary/90">Start new work</a>
						</div>
					{/if}
				</div>
			</section>
			</div>
		{/if}
	</div>
	<span class="sr-only" aria-live="polite">{announcement}</span>
</div>

<style>
	.control-center { isolation: isolate; }
	.control-grid {
		background-image: linear-gradient(hsl(var(--border) / .32) 1px, transparent 1px), linear-gradient(90deg, hsl(var(--border) / .32) 1px, transparent 1px);
		background-size: 42px 42px;
		mask-image: linear-gradient(to bottom, black 0, transparent 33rem);
		opacity: .52;
	}
	.control-glow { background: radial-gradient(ellipse at top, hsl(var(--primary) / .12), transparent 68%); filter: blur(4px); }
	.dashboard-card { border: 1px solid hsl(var(--border) / .88); border-radius: .875rem; background: hsl(var(--card) / .94); box-shadow: 0 1px 1px rgb(15 23 42 / .025), 0 8px 24px rgb(15 23 42 / .035); }
	.kpi-card { position: relative; min-height: 7rem; overflow: hidden; padding: .9rem 1rem; }
	.kpi-card::after { position: absolute; inset: auto -20% -65% 35%; height: 7rem; border-radius: 999px; background: radial-gradient(circle, hsl(var(--primary) / .09), transparent 68%); content: ''; }
	.kpi-card.attention { border-color: hsl(var(--warning) / .3); background: linear-gradient(135deg, hsl(var(--card)), hsl(var(--warning-tint))); }
	.kpi-top { display: flex; align-items: center; justify-content: space-between; color: hsl(var(--muted-foreground)); font-size: .68rem; font-weight: 600; letter-spacing: .025em; }
	.kpi-icon { display: grid; height: 1.65rem; width: 1.65rem; place-items: center; border-radius: .5rem; background: hsl(var(--muted)); color: hsl(var(--foreground-secondary)); font-size: .75rem; }
	.kpi-icon.working { background: hsl(var(--primary) / .11); color: hsl(var(--primary)); }
	.kpi-icon.attention { background: hsl(var(--warning) / .12); color: hsl(var(--warning)); }
	.kpi-value { position: relative; z-index: 1; margin-top: .45rem; font-size: 1.75rem; font-weight: 650; letter-spacing: -.045em; }
	.kpi-note { position: relative; z-index: 1; margin-top: .1rem; display: flex; align-items: center; gap: .35rem; color: hsl(var(--muted-foreground)); font-size: .65rem; }
	.live-badge { display: inline-flex; height: 2.25rem; align-items: center; gap: .45rem; border: 1px solid hsl(var(--border)); border-radius: 999px; background: hsl(var(--card)); padding: 0 .7rem; color: hsl(var(--muted-foreground)); font-size: .68rem; font-weight: 600; box-shadow: 0 1px 2px rgb(0 0 0 / .04); }
	.live-badge.live { border-color: hsl(var(--success) / .25); color: hsl(var(--success)); }
	.live-badge.offline { border-color: hsl(var(--danger) / .25); color: hsl(var(--danger)); }
	.live-badge.reconnecting { border-color: hsl(var(--warning) / .25); color: hsl(var(--warning)); }
	.status-beacon, .mini-beacon { display: inline-block; height: .42rem; width: .42rem; flex: none; border-radius: 999px; background: currentColor; }
	.live .status-beacon, .mini-beacon { color: hsl(var(--success)); box-shadow: 0 0 0 4px hsl(var(--success) / .1), 0 0 12px hsl(var(--success) / .35); animation: beacon 2.1s ease-in-out infinite; }
	.live-orbit { position: relative; display: grid; height: 1.1rem; width: 1.1rem; place-items: center; border: 1px solid hsl(var(--primary) / .35); border-radius: 50%; }
	.live-orbit::before { position: absolute; inset: 2px; border: 1px dashed hsl(var(--primary) / .5); border-radius: 50%; content: ''; animation: orbit 9s linear infinite; }
	.live-orbit span { height: .28rem; width: .28rem; border-radius: 50%; background: hsl(var(--primary)); box-shadow: 0 0 9px hsl(var(--primary) / .7); }
	.attention-mark { display: grid; height: 1.35rem; width: 1.35rem; place-items: center; border-radius: 50%; background: hsl(var(--warning) / .12); color: hsl(var(--warning)); font-size: .7rem; font-weight: 800; }
	.attention-chip { display: flex; min-width: min(22rem, 82vw); max-width: 28rem; align-items: center; gap: .65rem; border: 1px solid hsl(var(--warning) / .25); border-radius: .7rem; background: linear-gradient(115deg, hsl(var(--card)), hsl(var(--warning-tint))); padding: .65rem .75rem; box-shadow: 0 2px 9px hsl(var(--warning) / .04); }
	.attention-pulse { height: .5rem; width: .5rem; flex: none; border-radius: 50%; background: hsl(var(--warning)); box-shadow: 0 0 0 4px hsl(var(--warning) / .1); animation: beacon 1.8s ease-in-out infinite; }
	.active-row { display: flex; align-items: flex-start; gap: .7rem; border-radius: .7rem; padding: .7rem; transition: background 140ms ease, transform 140ms ease; }
	.active-row:hover, .active-row:focus-visible { background: hsl(var(--hover)); transform: translateX(1px); outline: none; }
	.agent-mark { position: relative; display: grid; height: 2rem; width: 2rem; flex: none; place-items: center; border: 1px solid hsl(var(--primary) / .2); border-radius: .65rem; background: linear-gradient(145deg, hsl(var(--primary) / .13), hsl(var(--card))); color: hsl(var(--primary)); font-size: .58rem; font-weight: 800; }
	.agent-mark i { position: absolute; bottom: -.1rem; right: -.1rem; height: .5rem; width: .5rem; border: 2px solid hsl(var(--card)); border-radius: 50%; background: hsl(var(--warning)); }
	.agent-mark i.working { background: hsl(var(--success)); box-shadow: 0 0 8px hsl(var(--success) / .55); }
	.feed-row { position: relative; display: flex; min-height: 6.5rem; align-items: flex-start; gap: .85rem; padding: .9rem 1rem; transition: background 140ms ease; }
	.feed-row:hover, .feed-row:focus-visible { background: hsl(var(--hover) / .82); outline: none; }
	.feed-row.needs-attention { background: linear-gradient(90deg, hsl(var(--warning-tint)), transparent 70%); box-shadow: inset 3px 0 hsl(var(--warning)); }
	.feed-row.arriving { animation: feed-arrive .55s cubic-bezier(.2,.8,.2,1) both, feed-flash 1.5s ease-out both; }
	.event-glyph { display: grid; height: 2rem; width: 2rem; flex: none; place-items: center; border: 1px solid hsl(var(--border)); border-radius: .65rem; background: hsl(var(--muted)); color: hsl(var(--foreground-secondary)); font-size: .75rem; font-weight: 700; }
	.event-glyph.success { border-color: hsl(var(--success) / .22); background: hsl(var(--success-tint)); color: hsl(var(--success)); }
	.event-glyph.warning { border-color: hsl(var(--warning) / .24); background: hsl(var(--warning-tint)); color: hsl(var(--warning)); }
	.event-glyph.error { border-color: hsl(var(--danger) / .24); background: hsl(var(--danger-tint)); color: hsl(var(--danger)); }
	.event-kind { border-radius: 999px; background: hsl(var(--muted)); padding: .12rem .42rem; color: hsl(var(--muted-foreground)); font-size: .58rem; font-weight: 650; text-transform: uppercase; letter-spacing: .05em; }
	.event-kind.attention { background: hsl(var(--warning) / .12); color: hsl(var(--warning)); }
	.quiet-radar { position: relative; height: 3.25rem; width: 3.25rem; border: 1px solid hsl(var(--border)); border-radius: 50%; background: radial-gradient(circle, hsl(var(--primary) / .15) 0 5%, transparent 6% 35%, hsl(var(--border) / .65) 36% 38%, transparent 39%); }
	.quiet-radar::after { position: absolute; inset: 50% 50% 50% 50%; width: 45%; height: 1px; transform-origin: left; background: linear-gradient(90deg, hsl(var(--primary) / .55), transparent); content: ''; animation: radar 5s linear infinite; }
	.empty-signal { display: flex; height: 3rem; align-items: flex-end; gap: .3rem; }
	.empty-signal i { display: block; width: .28rem; border-radius: 99px; background: hsl(var(--primary) / .3); }
	.empty-signal i:nth-child(1) { height: 35%; } .empty-signal i:nth-child(2) { height: 80%; } .empty-signal i:nth-child(3) { height: 52%; }
	@keyframes beacon { 0%,100% { opacity: 1; } 50% { opacity: .52; } }
	@keyframes orbit { to { transform: rotate(360deg); } }
	@keyframes radar { to { transform: rotate(360deg); } }
	@keyframes feed-arrive { from { opacity: 0; transform: translateY(-7px); } to { opacity: 1; transform: translateY(0); } }
	@keyframes feed-flash { from { background-color: hsl(var(--primary) / .1); } to { background-color: transparent; } }
	:global(.dark) .dashboard-card { box-shadow: 0 1px 1px rgb(0 0 0 / .25), 0 10px 28px rgb(0 0 0 / .12); }
	@media (max-width: 639px) { .feed-row { min-height: 0; padding: .8rem; } .kpi-card { min-height: 6.5rem; padding: .8rem; } .kpi-value { font-size: 1.5rem; } }
	@media (prefers-reduced-motion: reduce) { .live-orbit::before, .attention-pulse, .mini-beacon, .quiet-radar::after, .feed-row.arriving { animation: none !important; } .active-row, .feed-row { transition: none; } }
</style>
