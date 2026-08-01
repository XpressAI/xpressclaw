<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { setup, sessions, agents as agentsApi } from '$lib/api';
	import type { Agent, ImageAttachmentUpload } from '$lib/api';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';
	import { clearComposerDraft, loadComposerDraft, loadComposerTarget, saveComposerDraft, saveComposerTarget } from '$lib/composerDrafts';
	import { appendImageFiles, imageDataUrl, IMAGE_FILE_ACCEPT, MAX_IMAGE_ATTACHMENTS, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';
	import { harnessMark } from '$lib/utils';

	const messageDraftScope = 'new-work';
	let status_text = $state('Connecting to server...');
	let loading = $state(true);
	let retries = 0;

	let message = $state('');
	let messageDraftReady = $state(false);
	let agentList = $state<Agent[]>([]);
	let selectedAgent = $state('');
	let selectedAgentReady = $state(false);
	let sending = $state(false);
	let composing = $state(false);
	let sendError = $state('');
	let startFresh = $state(false);
	let imageAttachments = $state<ImageAttachmentUpload[]>([]);
	let imageInput = $state<HTMLInputElement>();
	let imagePreviews = $derived(imageAttachments.map((attachment) => ({
		name: attachment.name,
		src: imageDataUrl(attachment),
	})));

	async function checkReady() {
		try {
			status_text = 'Checking setup...';
			const status = await setup.status();
			if (!status.setup_complete) {
				goto('/setup', { replaceState: true });
				return;
			}

			const [agts] = await Promise.all([
				agentsApi.list().catch(() => [])
			]);
			agentList = agts;
			const savedAgent = loadComposerTarget(messageDraftScope);
			selectedAgent = agts.find((agent) => agent.id === savedAgent)?.id ?? agts[0]?.id ?? '';
			selectedAgentReady = true;
			loading = false;
		} catch {
			retries++;
			if (retries < 60) {
				status_text = 'Waiting for server...';
				setTimeout(checkReady, 500);
			} else {
				loading = false;
			}
		}
	}

	$effect(() => {
		if (messageDraftReady) saveComposerDraft(messageDraftScope, message);
	});

	$effect(() => {
		if (selectedAgentReady) saveComposerTarget(messageDraftScope, selectedAgent);
	});

	onMount(() => {
		message = loadComposerDraft(messageDraftScope);
		messageDraftReady = true;
		void checkReady();
	});

	function greeting(): string {
		const hour = new Date().getHours();
		if (hour < 12) return 'Good morning';
		if (hour < 18) return 'Good afternoon';
		return 'Good evening';
	}

	let selectedAgentObj = $derived(agentList.find(a => a.id === selectedAgent));

	async function send() {
		if ((!message.trim() && imageAttachments.length === 0) || !selectedAgent || sending) return;
		sending = true;
		sendError = '';

		try {
			const queued = await sessions.sendMessage(selectedAgent, message.trim(), {
				newSession: startFresh,
				attachments: imageAttachments,
			});
			message = '';
			clearComposerDraft(messageDraftScope);
			imageAttachments = [];
			await goto(`/tasks/${queued.task.id}`);
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
		} finally {
			sending = false;
		}
	}

	async function addImages(files: File[]) {
		try {
			imageAttachments = await appendImageFiles(imageAttachments, files);
			sendError = '';
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
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
			.catch((e) => (sendError = e instanceof Error ? e.message : String(e)));
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !composing && e.keyCode !== 229) {
			e.preventDefault();
			send();
		}
	}

	function statusDot(status: string): string {
		if (status === 'running') return 'bg-blue-500 animate-pulse';
		if (status === 'queued') return 'bg-amber-400';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		return 'bg-emerald-500';
	}
</script>

{#if loading}
	<div class="flex h-full flex-col items-center justify-center gap-3">
		<div class="h-8 w-8 animate-spin rounded-full border-2 border-muted-foreground border-t-primary"></div>
		<span class="text-sm text-muted-foreground">{status_text}</span>
	</div>
{:else}
	<div class="flex min-h-full flex-col items-center justify-center px-4 py-8 sm:px-6">
		<div class="w-full max-w-2xl space-y-6 sm:space-y-8">
			<!-- Greeting -->
			<div class="text-center">
				<h1 class="text-2xl font-semibold text-foreground sm:text-3xl">{greeting()}</h1>
				<p class="mt-2 text-sm text-muted-foreground">Send work to one of your agents.</p>
			</div>

			{#if agentList.length === 0}
				<div class="rounded-2xl border border-dashed border-border bg-card p-8 text-center">
					<h2 class="text-base font-semibold">Create an agent first</h2>
					<p class="mx-auto mt-2 max-w-md text-sm text-muted-foreground">Give a durable context a workspace and a Codex, Claude, OpenCode, or custom harness.</p>
					<a href="/setup?mode=add-session" class="mt-5 inline-flex rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Create agent</a>
				</div>
			{:else}
			<!-- Queue work directly into the selected logical session. -->
			<div class="rounded-2xl border border-border bg-card shadow-lg shadow-black/10 focus-within:border-primary/40">
				<textarea
					bind:value={message}
					onkeydown={handleKeydown}
					oncompositionstart={() => (composing = true)}
					oncompositionend={() => setTimeout(() => (composing = false), 0)}
					onpaste={handlePaste}
					placeholder="Describe the outcome you want…"
					rows="3"
					disabled={sending}
					class="w-full resize-none rounded-t-2xl bg-transparent px-5 pt-5 pb-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
				></textarea>
				<ImageAttachmentPreviews attachments={imagePreviews} onremove={(index) => (imageAttachments = imageAttachments.filter((_, itemIndex) => itemIndex !== index))} />
				<div class="flex flex-col gap-3 px-4 pb-4 sm:flex-row sm:items-center sm:justify-between">
					<label class="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground" title="Do not inherit context from this agent's current conversation">
						<input type="checkbox" bind:checked={startFresh} class="h-3.5 w-3.5 rounded border-border accent-primary" />
						Start a fresh conversation
					</label>
					<div class="flex min-w-0 items-center justify-end gap-2 sm:gap-3">
						<input bind:this={imageInput} type="file" accept={IMAGE_FILE_ACCEPT} multiple onchange={handleImageInput} class="hidden" />
						<button type="button" onclick={() => imageInput?.click()} disabled={sending || imageAttachments.length >= MAX_IMAGE_ATTACHMENTS} aria-label="Attach images" title="Attach images (you can also paste)"
							class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-30">
							<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>
						</button>
						<div class="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
								{#if selectedAgentObj}
									<span class="flex h-5 w-5 items-center justify-center rounded bg-muted text-[10px] font-semibold">{harnessMark(selectedAgentObj.backend)}</span>
									<span class="h-2 w-2 rounded-full {statusDot(selectedAgentObj.status)}"></span>
								{/if}
								<select
									bind:value={selectedAgent}
									class="min-w-0 max-w-40 cursor-pointer bg-transparent text-xs text-foreground focus:outline-none"
								>
									{#each agentList as agent}
										<option value={agent.id}>
											{agent.title || agent.name}
										</option>
									{/each}
								</select>
						</div>
						<button
							onclick={send}
							disabled={(!message.trim() && imageAttachments.length === 0) || sending}
							class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors shadow-lg shadow-primary/20"
						>
							{#if sending}
								<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary-foreground border-t-transparent"></span>
							{:else}
								<svg class="h-4 w-4" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
							{/if}
						</button>
					</div>
				</div>
			</div>
			{#if sendError}<p class="text-center text-sm text-destructive">{sendError}</p>{/if}
			<div class="text-center"><a href="/agents" class="text-xs text-muted-foreground hover:text-foreground hover:underline">Manage agents</a></div>
			{/if}
		</div>
	</div>
{/if}
