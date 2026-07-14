<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { sessions } from '$lib/api';
	import type { SessionOverview, SessionEvent, WorkAttempt } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let { agentId }: { agentId: string } = $props();
	let overview = $state<SessionOverview | null>(null);
	let error = $state<string | null>(null);
	let message = $state('');
	let sending = $state(false);
	let cancelling = $state<string | null>(null);
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	let visibleArtifacts = $derived(
		(overview?.artifacts ?? []).filter((artifact) => artifact.artifact_type !== 'runner_output')
	);

	onMount(() => {
		load();
		pollTimer = setInterval(load, 2000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function load() {
		try {
			overview = await sessions.get(agentId);
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function send() {
		const content = message.trim();
		if (!content || sending) return;
		sending = true;
		try {
			await sessions.sendMessage(agentId, content);
			message = '';
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			sending = false;
		}
	}

	async function cancel(attempt: WorkAttempt) {
		if (cancelling) return;
		cancelling = attempt.id;
		try {
			await sessions.cancelAttempt(agentId, attempt.id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			cancelling = null;
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
			event.preventDefault();
			send();
		}
	}

	function statusTone(status: string): string {
		if (status === 'completed' || status === 'idle') return 'text-emerald-500 bg-emerald-500/10 border-emerald-500/20';
		if (status === 'failed' || status === 'blocked') return 'text-destructive bg-destructive/10 border-destructive/20';
		if (status === 'waiting_for_input') return 'text-amber-500 bg-amber-500/10 border-amber-500/20';
		if (status === 'running' || status === 'preparing') return 'text-blue-500 bg-blue-500/10 border-blue-500/20';
		return 'text-muted-foreground bg-secondary border-border';
	}

	function eventMarker(event: SessionEvent): { label: string; tone: string } {
		if (event.source_type === 'user') return { label: 'You', tone: 'bg-primary text-primary-foreground' };
		if (event.event_type === 'attempt_failed') return { label: '!', tone: 'bg-destructive text-destructive-foreground' };
		if (event.event_type === 'attempt_completed') return { label: '✓', tone: 'bg-emerald-500 text-white' };
		if (event.source_type === 'schedule') return { label: '↻', tone: 'bg-violet-500 text-white' };
		if (event.source_type === 'connector') return { label: '↗', tone: 'bg-cyan-600 text-white' };
		return { label: '•', tone: 'bg-secondary text-secondary-foreground' };
	}

	function compactId(id: string): string {
		return id.length > 10 ? id.slice(0, 8) : id;
	}
</script>

<div class="mx-auto max-w-7xl space-y-5 pb-8">
	{#if error}
		<div class="rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>
	{/if}

	{#if overview}
		<div class="grid gap-3 sm:grid-cols-3">
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Session</div>
				<div class="mt-2 flex items-center gap-2">
					<span class="h-2 w-2 rounded-full {overview.session.status === 'running' ? 'bg-blue-500 animate-pulse' : 'bg-emerald-500'}"></span>
					<span class="text-base font-semibold capitalize">{overview.session.status.replaceAll('_', ' ')}</span>
				</div>
				<p class="mt-1 line-clamp-2 text-xs text-muted-foreground">
					{overview.session.latest_summary ?? 'Ready for work'}
				</p>
			</div>
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="text-xs font-medium uppercase tracking-wide text-muted-foreground">In progress</div>
				<div class="mt-2 text-2xl font-semibold">{overview.active_attempts.length}</div>
				<p class="mt-1 text-xs text-muted-foreground">Background native workers</p>
			</div>
			<div class="rounded-xl border border-border bg-card p-4">
				<div class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Queued</div>
				<div class="mt-2 text-2xl font-semibold">{overview.queued_attempts.length}</div>
				<p class="mt-1 text-xs text-muted-foreground">Messages and automated tasks</p>
			</div>
		</div>

		<div class="rounded-xl border border-border bg-card p-4 shadow-sm">
			<div class="mb-3 flex items-center justify-between gap-4">
				<div>
					<h2 class="text-sm font-semibold">Send work or ask a question</h2>
					<p class="mt-0.5 text-xs text-muted-foreground">The session accepts messages while native workers continue in the background.</p>
				</div>
				<span class="hidden rounded-full border border-emerald-500/20 bg-emerald-500/10 px-2.5 py-1 text-xs text-emerald-500 sm:inline">Available</span>
			</div>
			<textarea
				bind:value={message}
				onkeydown={handleKeydown}
				rows="3"
				placeholder="Describe the outcome you want…"
				class="w-full resize-y rounded-lg border border-input bg-background px-3 py-2.5 text-sm outline-none transition-colors placeholder:text-muted-foreground focus:border-primary focus:ring-1 focus:ring-primary"
			></textarea>
			<div class="mt-2 flex items-center justify-between">
				<span class="text-[11px] text-muted-foreground">⌘/Ctrl + Enter to send</span>
				<button
					onclick={send}
					disabled={sending || !message.trim()}
					class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
				>
					{sending ? 'Queuing…' : 'Send'}
				</button>
			</div>
		</div>

		<div class="grid gap-5 lg:grid-cols-[minmax(0,1.7fr)_minmax(300px,1fr)]">
			<section class="min-w-0 rounded-xl border border-border bg-card">
				<div class="border-b border-border px-4 py-3">
					<h2 class="text-sm font-semibold">Activity</h2>
					<p class="text-xs text-muted-foreground">One timeline across people, schedules, workflows, and native workers</p>
				</div>
				{#if overview.recent_events.length === 0}
					<div class="px-4 py-12 text-center text-sm text-muted-foreground">No activity yet. Send the first message above.</div>
				{:else}
					<div class="divide-y divide-border">
						{#each overview.recent_events as event (event.id)}
							{@const marker = eventMarker(event)}
							<div class="flex gap-3 px-4 py-3.5">
								<div class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold {marker.tone}">{marker.label}</div>
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-x-2 gap-y-1">
										<span class="text-xs font-medium capitalize">{event.source_type.replaceAll('_', ' ')}</span>
										<span class="text-[11px] text-muted-foreground">{event.event_type.replaceAll('_', ' ')}</span>
										<span class="ml-auto text-[11px] text-muted-foreground">{timeAgo(event.created_at)}</span>
									</div>
									<p class="mt-1 whitespace-pre-wrap break-words text-sm leading-relaxed">{event.summary}</p>
									{#if event.attempt_id || event.task_id}
										<div class="mt-1.5 flex gap-3 text-[11px] text-muted-foreground">
											{#if event.attempt_id}<span>attempt {compactId(event.attempt_id)}</span>{/if}
											{#if event.task_id}<a class="hover:text-foreground hover:underline" href="/tasks/{event.task_id}">view task</a>{/if}
										</div>
									{/if}
								</div>
							</div>
						{/each}
					</div>
				{/if}
			</section>

			<div class="min-w-0 space-y-5">
				<section class="rounded-xl border border-border bg-card">
					<div class="border-b border-border px-4 py-3">
						<h2 class="text-sm font-semibold">Work attempts</h2>
						<p class="text-xs text-muted-foreground">Isolated native CLI invocations</p>
					</div>
					{#if overview.recent_attempts.length === 0}
						<div class="px-4 py-8 text-center text-xs text-muted-foreground">Nothing queued</div>
					{:else}
						<div class="divide-y divide-border">
							{#each overview.recent_attempts.slice(0, 8) as attempt (attempt.id)}
								<div class="space-y-2 px-4 py-3">
									<div class="flex items-center gap-2">
										<span class="rounded-full border px-2 py-0.5 text-[10px] font-medium capitalize {statusTone(attempt.status)}">{attempt.status.replaceAll('_', ' ')}</span>
										<span class="text-xs font-medium capitalize">{attempt.runner}</span>
										<span class="ml-auto text-[10px] text-muted-foreground">{timeAgo(attempt.created_at)}</span>
									</div>
									<p class="line-clamp-2 text-xs text-muted-foreground">{attempt.prompt}</p>
									<div class="flex items-center gap-3 text-[11px]">
										{#if attempt.task_id}<a href="/tasks/{attempt.task_id}" class="text-muted-foreground hover:text-foreground hover:underline">Task</a>{/if}
										{#if ['queued', 'preparing', 'running', 'review', 'waiting_for_input'].includes(attempt.status)}
											<button onclick={() => cancel(attempt)} disabled={cancelling === attempt.id} class="ml-auto text-destructive hover:underline disabled:opacity-50">
												{cancelling === attempt.id ? 'Cancelling…' : 'Cancel'}
											</button>
										{/if}
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</section>

				<section class="rounded-xl border border-border bg-card">
					<div class="border-b border-border px-4 py-3">
						<h2 class="text-sm font-semibold">Artifacts</h2>
						<p class="text-xs text-muted-foreground">Results, patches, reports, and review decisions</p>
					</div>
					{#if visibleArtifacts.length === 0}
						<div class="px-4 py-8 text-center text-xs text-muted-foreground">Completed work will appear here</div>
					{:else}
						<div class="divide-y divide-border">
							{#each visibleArtifacts.slice(0, 6) as artifact (artifact.id)}
								<div class="px-4 py-3">
									<div class="flex items-center gap-2">
										<span class="rounded bg-secondary px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">{artifact.artifact_type}</span>
										<span class="truncate text-xs font-medium">{artifact.title}</span>
									</div>
									{#if artifact.content}<p class="mt-2 line-clamp-5 whitespace-pre-wrap text-xs leading-relaxed text-muted-foreground">{artifact.content}</p>{/if}
									{#if artifact.uri}<a href={artifact.uri} class="mt-2 block truncate text-xs text-primary hover:underline">{artifact.uri}</a>{/if}
								</div>
							{/each}
						</div>
					{/if}
				</section>
			</div>
		</div>
	{:else if !error}
		<div class="flex min-h-64 items-center justify-center text-sm text-muted-foreground">Loading session…</div>
	{/if}
</div>
