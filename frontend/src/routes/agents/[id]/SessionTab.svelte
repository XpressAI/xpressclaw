<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { sessions, tasks } from '$lib/api';
	import type { ImageAttachmentUpload, SessionOverview, RunnerReadiness, Task } from '$lib/api';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';
	import { clearComposerDraft, loadComposerDraft, saveComposerDraft } from '$lib/composerDrafts';
	import { appendImageFiles, imageDataUrl, IMAGE_FILE_ACCEPT, MAX_IMAGE_ATTACHMENTS, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';
	import { timeAgo } from '$lib/utils';

	let { agentId }: { agentId: string } = $props();
	const messageDraftScope = () => `session.${agentId}`;
	let overview = $state<SessionOverview | null>(null);
	let readiness = $state<RunnerReadiness | null>(null);
	let taskList = $state<Task[]>([]);
	let error = $state<string | null>(null);
	let message = $state('');
	let messageDraftReady = $state(false);
	let sending = $state(false);
	let preparing = $state(false);
	let startFresh = $state(false);
	let imageAttachments = $state<ImageAttachmentUpload[]>([]);
	let imageInput = $state<HTMLInputElement>();
	let imagePreviews = $derived(imageAttachments.map((attachment) => ({
		name: attachment.name,
		src: imageDataUrl(attachment),
	})));
	let pollTimer: ReturnType<typeof setInterval> | null = null;

	const RECENT_WORK_LIMIT = 5;
	const ATTENTION_STATUSES = ['waiting_for_input', 'blocked'];
	let attentionTasks = $derived(taskList.filter((task) => ATTENTION_STATUSES.includes(task.status)));
	let activeTasks = $derived(taskList.filter((task) => ['pending', 'in_progress'].includes(task.status)));
	let recentWorkTasks = $state<Task[]>([]);

	$effect(() => {
		if (messageDraftReady) saveComposerDraft(messageDraftScope(), message);
	});

	onMount(() => {
		message = loadComposerDraft(messageDraftScope());
		messageDraftReady = true;
		load();
		loadReadiness();
		pollTimer = setInterval(load, 2500);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function load() {
		try {
			const [nextOverview, result, recentWork] = await Promise.all([
				sessions.get(agentId),
				tasks.list(undefined, agentId),
				tasks.list(undefined, agentId, {
					limit: RECENT_WORK_LIMIT,
					sort: 'recent',
					excludeStatuses: ATTENTION_STATUSES,
				}),
			]);
			overview = nextOverview;
			taskList = result.tasks;
			recentWorkTasks = recentWork.tasks;
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function loadReadiness() {
		try {
			readiness = await sessions.readiness(agentId);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function prepareRunner() {
		if (preparing) return;
		preparing = true;
		try {
			readiness = await sessions.prepare(agentId);
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			preparing = false;
		}
	}

	async function send() {
		const content = message.trim();
		if ((!content && imageAttachments.length === 0) || sending) return;
		sending = true;
		try {
			const queued = await sessions.sendMessage(agentId, content, {
				newSession: startFresh,
				attachments: imageAttachments,
			});
			message = '';
			clearComposerDraft(messageDraftScope());
			imageAttachments = [];
			startFresh = false;
			await goto(`/tasks/${queued.task.id}`);
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			sending = false;
		}
	}

	async function addImages(files: File[]) {
		try {
			imageAttachments = await appendImageFiles(imageAttachments, files);
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	function handleImageInput(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		void addImages(Array.from(input.files ?? [])).finally(() => (input.value = ''));
	}

	function handlePaste(event: ClipboardEvent) {
		if (!shouldHandleImagePaste(event)) return;
		event.preventDefault();
		void pastedImageFiles(event)
			.then(addImages)
			.catch((e) => (error = e instanceof Error ? e.message : String(e)));
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
			event.preventDefault();
			send();
		}
	}

	function statusMeta(status: string): { label: string; dot: string; text: string } {
		if (status === 'running') return { label: 'Working', dot: 'bg-blue-500 animate-pulse', text: 'text-blue-500' };
		if (status === 'queued') return { label: 'Queued', dot: 'bg-amber-400', text: 'text-amber-500' };
		if (status === 'waiting_for_input') return { label: 'Waiting for you', dot: 'bg-orange-500 animate-pulse', text: 'text-orange-500' };
		if (status === 'blocked' || status === 'failed') return { label: 'Needs attention', dot: 'bg-red-500', text: 'text-red-500' };
		if (status === 'completed') return { label: 'Completed', dot: 'bg-emerald-500', text: 'text-emerald-500' };
		if (status === 'cancelled') return { label: 'Cancelled', dot: 'bg-muted-foreground', text: 'text-muted-foreground' };
		return { label: 'Ready', dot: 'bg-emerald-500', text: 'text-emerald-500' };
	}

	function taskTone(status: string): string {
		if (status === 'in_progress') return 'border-blue-500/25 bg-blue-500/5';
		if (status === 'pending') return 'border-amber-500/25 bg-amber-500/5';
		if (status === 'waiting_for_input') return 'border-orange-500/30 bg-orange-500/5';
		if (status === 'blocked') return 'border-red-500/25 bg-red-500/5';
		return 'border-border bg-card';
	}
</script>

<div class="mx-auto w-full max-w-5xl space-y-5 pb-8">
	{#if error}
		<div class="rounded-xl border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>
	{/if}

	{#if readiness && !readiness.ready}
		<div class="rounded-xl border border-amber-500/30 bg-amber-500/5 p-4">
			<div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
				<div>
					<h2 class="text-sm font-semibold text-amber-600">This project is not ready to run</h2>
					<ul class="mt-2 space-y-1 text-xs text-muted-foreground">
						{#each readiness.issues as issue}<li>• {issue}</li>{/each}
					</ul>
					{#if !readiness.auth_present}
						<p class="mt-2 text-xs text-foreground">Run <code>{readiness.kind} login</code> on this computer, then refresh.</p>
					{/if}
				</div>
				{#if readiness.docker_available && !readiness.image_present}
					<button onclick={prepareRunner} disabled={preparing} class="shrink-0 rounded-lg bg-primary px-3 py-2 text-xs font-medium text-primary-foreground disabled:opacity-50">
						{preparing ? 'Preparing…' : 'Prepare runner'}
					</button>
				{/if}
			</div>
		</div>
	{/if}

	{#if overview}
		{@const projectStatus = statusMeta(overview.session.status)}
		<section class="overflow-hidden rounded-2xl border border-border bg-card shadow-sm">
			<div class="flex flex-col gap-3 border-b border-border px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
				<div class="flex items-center gap-2 text-sm font-medium {projectStatus.text}">
					<span class="h-2.5 w-2.5 rounded-full {projectStatus.dot}"></span>
					{projectStatus.label}
				</div>
				<div class="text-xs text-muted-foreground">
					{#if activeTasks.length > 0}{activeTasks.length} active{:else}No active tasks{/if}
					{#if overview.queued_attempts.length > 0} · {overview.queued_attempts.length} queued{/if}
				</div>
			</div>
			<textarea bind:value={message} onkeydown={handleKeydown} onpaste={handlePaste} rows="4" placeholder="Send work or ask a question…" class="w-full resize-y bg-transparent px-5 pb-3 pt-5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"></textarea>
			<ImageAttachmentPreviews attachments={imagePreviews} onremove={(index) => (imageAttachments = imageAttachments.filter((_, itemIndex) => itemIndex !== index))} />
			<div class="flex flex-col gap-3 px-4 pb-4 sm:flex-row sm:items-center sm:justify-between">
				<label class="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground" title="By default, this task continues the project's active agent conversation">
					<input type="checkbox" bind:checked={startFresh} class="h-3.5 w-3.5 rounded border-border accent-primary" />
					Start a fresh conversation
				</label>
				<div class="flex items-center justify-between gap-3 sm:justify-end">
					<input bind:this={imageInput} type="file" accept={IMAGE_FILE_ACCEPT} multiple onchange={handleImageInput} class="hidden" />
					<button type="button" onclick={() => imageInput?.click()} disabled={sending || imageAttachments.length >= MAX_IMAGE_ATTACHMENTS} aria-label="Attach images" title="Attach images (you can also paste)"
						class="flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-30">
						<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>
					</button>
					<span class="text-[11px] text-muted-foreground">⌘/Ctrl + Enter</span>
					<button onclick={send} disabled={sending || (!message.trim() && imageAttachments.length === 0)} class="rounded-lg bg-primary px-5 py-2 text-sm font-medium text-primary-foreground disabled:opacity-40">
						{sending ? 'Sending…' : 'Send'}
					</button>
				</div>
			</div>
			<div class="border-t border-border/70 bg-secondary/20 px-4 py-2.5 text-[11px] text-muted-foreground">
				Tasks continue this project’s active {readiness?.kind ?? 'agent'} conversation. Dependent tasks continue the conversation they depend on.
			</div>
		</section>

		{#if attentionTasks.length > 0}
			<section class="space-y-2">
				<div class="flex items-center justify-between px-1">
					<h2 class="text-sm font-semibold text-orange-500">Needs you</h2>
					<span class="text-xs text-muted-foreground">{attentionTasks.length}</span>
				</div>
				{#each attentionTasks as task (task.id)}
					<a href="/tasks/{task.id}" class="flex items-center gap-3 rounded-xl border p-4 transition-colors hover:border-primary/40 {taskTone(task.status)}">
						<span class="h-2.5 w-2.5 shrink-0 rounded-full bg-orange-500 animate-pulse"></span>
						<div class="min-w-0 flex-1">
							<div class="truncate text-sm font-medium">{task.title}</div>
							<div class="mt-0.5 text-xs text-muted-foreground">{task.status === 'waiting_for_input' ? 'Waiting for your reply' : 'Blocked'} · {timeAgo(task.updated_at)}</div>
						</div>
						<span class="text-muted-foreground">→</span>
					</a>
				{/each}
			</section>
		{/if}

		<section class="overflow-hidden rounded-xl border border-border bg-card">
			<div class="flex items-center justify-between border-b border-border px-4 py-3">
				<div>
					<h2 class="text-sm font-semibold">Work</h2>
					<p class="mt-0.5 text-xs text-muted-foreground">Open a task to see its conversation and every technical step.</p>
				</div>
				<a href="/tasks" class="text-xs text-muted-foreground hover:text-foreground">All tasks</a>
			</div>
			{#if recentWorkTasks.length === 0}
				<div class="px-4 py-12 text-center text-sm text-muted-foreground">{attentionTasks.length > 0 ? 'No other work in this project.' : 'No work yet. Send the first task above.'}</div>
			{:else}
				<div data-project-work-list class="divide-y divide-border">
					{#each recentWorkTasks as task (task.id)}
						{@const meta = statusMeta(task.status)}
						<a data-project-work-item href="/tasks/{task.id}" class="group flex items-center gap-3 px-4 py-3.5 hover:bg-accent/40">
							<span class="h-2.5 w-2.5 shrink-0 rounded-full {meta.dot}"></span>
							<div class="min-w-0 flex-1">
								<div class="truncate text-sm font-medium group-hover:text-primary">{task.title}</div>
								<div class="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground"><span>{meta.label}</span><span>·</span><span>{timeAgo(task.updated_at)}</span></div>
							</div>
							<span class="text-muted-foreground">→</span>
						</a>
					{/each}
				</div>
			{/if}
		</section>
	{:else if !error}
		<div class="flex min-h-64 items-center justify-center text-sm text-muted-foreground">Loading project…</div>
	{/if}
</div>
