<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { fetchVisualizationDocument, type MessageVisualization } from '$lib/api';

	interface FollowUpRequest {
		requestId: string;
		prompt: string;
		title: string | null;
	}

	let {
		artifact,
		title,
		mode = 'normal',
		href,
		followUpTarget = 'this thread',
		onfollowup,
	}: {
		artifact: MessageVisualization | null;
		title: string;
		mode?: 'normal' | 'wide';
		href?: string;
		followUpTarget?: string;
		onfollowup?: (prompt: string, title?: string) => Promise<void>;
	} = $props();

	let frame = $state<HTMLIFrameElement>();
	let confirmButton = $state<HTMLButtonElement>();
	let blobUrl = $state('');
	let loadError = $state('');
	let frameHeight = $state(320);
	let expanded = $state(false);
	let pendingFollowUp = $state<FollowUpRequest | null>(null);
	let followUpSending = $state(false);
	let followUpError = $state('');
	let disposed = false;
	let themeObserver: MutationObserver | null = null;

	let ready = $derived(artifact?.status === 'ready' && Boolean(href));
	let fallback = $derived(loadError || (artifact?.status === 'unavailable' ? fallbackMessage(artifact.error_code) : !artifact ? 'This response referenced a visualization that was not captured.' : 'This visualization is unavailable.'));

	onMount(() => {
		window.addEventListener('message', handleFrameMessage);
		window.addEventListener('keydown', handleKeydown);
		themeObserver = new MutationObserver(sendTheme);
		themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
		if (ready && artifact && href) void loadDocument(href, artifact.retrieval_token);
	});

	onDestroy(() => {
		disposed = true;
		window.removeEventListener('message', handleFrameMessage);
		window.removeEventListener('keydown', handleKeydown);
		themeObserver?.disconnect();
		if (blobUrl) URL.revokeObjectURL(blobUrl);
	});

	async function loadDocument(url: string, token: string) {
		try {
			const blob = await fetchVisualizationDocument(url, token);
			if (disposed) return;
			blobUrl = URL.createObjectURL(blob);
		} catch (cause) {
			if (!disposed) loadError = cause instanceof Error ? cause.message : 'Could not load this visualization.';
		}
	}

	function fallbackMessage(code: string | null): string {
		const messages: Record<string, string> = {
			outside_permitted_roots: 'The generated file was outside this Agent’s permitted workspace.',
			missing: 'The generated file could not be found when the response was saved.',
			unreadable: 'The generated file could not be read when the response was saved.',
			non_html: 'The referenced file was not a valid HTML visualization.',
			oversize: 'The generated visualization exceeded the 1 MiB limit.',
			malformed_html: 'The generated file was not a supported HTML fragment.',
		};
		return messages[code ?? ''] ?? 'This visualization could not be captured.';
	}

	function currentTheme(): 'light' | 'dark' {
		return document.documentElement.classList.contains('dark') ? 'dark' : 'light';
	}

	function postToFrame(message: Record<string, unknown>) {
		if (!artifact || !frame?.contentWindow) return;
		frame.contentWindow.postMessage({ source: 'xpressclaw-host', artifactId: artifact.id, ...message }, '*');
	}

	function sendTheme() {
		postToFrame({ type: 'theme', theme: currentTheme() });
	}

	function plainRecord(value: unknown): value is Record<string, unknown> {
		return Object.prototype.toString.call(value) === '[object Object]';
	}

	function validText(value: unknown, max: number): value is string {
		return typeof value === 'string' && value.length <= max && !/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(value);
	}

	function exactKeys(value: Record<string, unknown>, allowed: string[]): boolean {
		return Object.keys(value).every((key) => allowed.includes(key));
	}

	function handleFrameMessage(event: MessageEvent) {
		if (!artifact || event.source !== frame?.contentWindow || !plainRecord(event.data)) return;
		const data = event.data;
		if (data.source !== 'xpressclaw-visualization' || data.artifactId !== artifact.id) return;
		if (data.type === 'resize') {
			// The expanded viewport is intentionally much taller than the inline
			// card. Keep it from replacing the remembered inline measurement.
			if (!expanded && typeof data.height === 'number' && Number.isFinite(data.height)) {
				frameHeight = Math.max(220, Math.min(720, Math.ceil(data.height)));
			}
			return;
		}
		if (data.type !== 'follow-up-request' || !exactKeys(data, ['source', 'type', 'artifactId', 'requestId', 'prompt', 'title'])) return;
		if (!validText(data.requestId, 100) || !/^[A-Za-z0-9-]+$/.test(data.requestId)) return;
		if (!validText(data.prompt, 20_000) || !data.prompt.trim()) {
			sendFollowUpResult(data.requestId, false, 'The visualization supplied an invalid follow-up prompt.');
			return;
		}
		if (data.title !== null && data.title !== undefined && (!validText(data.title, 250) || !data.title.trim())) {
			sendFollowUpResult(data.requestId, false, 'The visualization supplied an invalid follow-up title.');
			return;
		}
		if (pendingFollowUp) {
			sendFollowUpResult(data.requestId, false, 'Another follow-up is awaiting confirmation.');
			return;
		}
		pendingFollowUp = {
			requestId: data.requestId,
			prompt: data.prompt,
			title: typeof data.title === 'string' ? data.title : null,
		};
		followUpError = '';
		requestAnimationFrame(() => confirmButton?.focus());
	}

	function sendFollowUpResult(requestId: string, ok: boolean, error?: string) {
		postToFrame({ type: 'follow-up-result', requestId, ok, ...(error ? { error } : {}) });
	}

	async function confirmFollowUp() {
		if (!pendingFollowUp || followUpSending) return;
		if (!onfollowup) {
			followUpError = 'Follow-up messages are unavailable here.';
			return;
		}
		followUpSending = true;
		followUpError = '';
		const request = pendingFollowUp;
		try {
			await onfollowup(request.prompt, request.title ?? undefined);
			sendFollowUpResult(request.requestId, true);
			pendingFollowUp = null;
		} catch (cause) {
			followUpError = cause instanceof Error ? cause.message : 'Could not send the follow-up.';
		} finally {
			followUpSending = false;
		}
	}

	function cancelFollowUp() {
		if (!pendingFollowUp || followUpSending) return;
		sendFollowUpResult(pendingFollowUp.requestId, false, 'Follow-up cancelled.');
		pendingFollowUp = null;
		followUpError = '';
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key !== 'Escape') return;
		if (pendingFollowUp && !followUpSending) cancelFollowUp();
		else if (expanded) expanded = false;
	}
</script>

<div
	data-inline-visualization
	data-visualization-status={ready && !loadError ? 'ready' : 'unavailable'}
	data-visualization-mode={mode}
	class={expanded ? 'fixed inset-0 z-[80] flex items-center justify-center p-2 sm:p-6' : `my-3 ${mode === 'wide' ? 'w-full' : 'max-w-3xl'}`}
>
	{#if expanded}
		<button type="button" class="absolute inset-0 bg-background/85 backdrop-blur-sm" aria-label="Close expanded visualization" onclick={() => (expanded = false)}></button>
	{/if}
	<section class="relative flex min-h-0 w-full flex-col overflow-hidden rounded-xl border border-border bg-card shadow-[var(--shadow-overlay)] {expanded ? 'h-full max-h-[min(900px,calc(100vh-1rem))] max-w-7xl' : ''}">
		<header class="flex shrink-0 items-center gap-2 border-b border-border/70 px-3 py-2">
			<svg class="h-4 w-4 shrink-0 text-primary" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M4 18V9m5 9V5m5 13v-7m5 7V3"/><path d="M2 21h20"/></svg>
			<h3 class="min-w-0 flex-1 truncate text-xs font-medium">{title}</h3>
			{#if ready && !loadError}<span class="ai-status-pill bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"><span aria-hidden="true">●</span> Interactive</span>{/if}
			<button type="button" class="ai-icon-button" aria-label={expanded ? 'Exit expanded visualization' : 'Expand visualization'} onclick={() => (expanded = !expanded)}>
				{#if expanded}<svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m8 3v5H3M16 3v5h5M8 21v-5H3m13 5v-5h5"/></svg>{:else}<svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M8 3H3v5m13-5h5v5M8 21H3v-5m13 5h5v-5"/></svg>{/if}
			</button>
		</header>

		{#if ready && !loadError}
			{#if blobUrl}
				<iframe
					bind:this={frame}
					src={blobUrl}
					title={title}
					sandbox="allow-scripts"
					referrerpolicy="no-referrer"
					class="min-h-0 w-full border-0 bg-transparent {expanded ? 'flex-1' : 'shrink-0'}"
					style:height={expanded ? undefined : `${frameHeight}px`}
					onload={sendTheme}
				></iframe>
			{:else}
				<div class="h-72 animate-pulse bg-muted/55" aria-label="Loading visualization"></div>
			{/if}
		{:else}
			<div class="flex min-h-40 items-center gap-3 px-4 py-5 text-sm">
				<span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground" aria-hidden="true">!</span>
				<div><p class="font-medium">Visualization unavailable</p><p class="mt-1 text-xs leading-5 text-muted-foreground">{fallback}</p></div>
			</div>
		{/if}
	</section>
</div>

{#if pendingFollowUp}
	<div class="fixed inset-0 z-[100] flex items-center justify-center p-4" role="presentation">
		<button type="button" class="absolute inset-0 bg-background/80 backdrop-blur-sm" aria-label="Cancel visualization follow-up" onclick={cancelFollowUp}></button>
		<div role="dialog" aria-modal="true" aria-labelledby="visualization-follow-up-title" class="relative w-full max-w-lg rounded-xl border border-border bg-card p-5 shadow-[var(--shadow-overlay)]">
			<h2 id="visualization-follow-up-title" class="text-base font-semibold">{pendingFollowUp.title || 'Send this follow-up?'}</h2>
			<p class="mt-2 text-xs leading-5 text-muted-foreground">This visualization wants to send the following message to {followUpTarget}:</p>
			<div class="mt-3 max-h-56 overflow-y-auto whitespace-pre-wrap rounded-lg bg-muted/65 p-3 text-sm">{pendingFollowUp.prompt}</div>
			{#if followUpError}<p class="mt-3 text-xs text-destructive" role="alert">{followUpError}</p>{/if}
			<div class="mt-5 flex justify-end gap-2">
				<button type="button" class="rounded-lg border border-border px-3 py-2 text-xs" disabled={followUpSending} onclick={cancelFollowUp}>Cancel</button>
				<button bind:this={confirmButton} type="button" class="rounded-lg bg-primary px-3 py-2 text-xs text-primary-foreground disabled:opacity-60" disabled={followUpSending} onclick={() => void confirmFollowUp()}>{followUpSending ? 'Sending…' : 'Send follow-up'}</button>
			</div>
		</div>
	</div>
{/if}
