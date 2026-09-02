<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { MessageVisualization } from '$lib/api';
	import { renderContent } from '$lib/formatMessage';
	import { splitMessageVisualizations } from '$lib/messageVisualizations';
	import VisualizationArtifact from '$lib/components/VisualizationArtifact.svelte';

	type MessageRole = 'user' | 'assistant' | 'system' | string;
	type SelectionAction = 'Explain' | 'Improve' | 'Shorten' | 'Tone' | 'Grammar';

	let {
		role,
		sender,
		timestampLabel,
		content,
		avatar,
		badge = '',
		transcriptTimestamp = '',
		openLinksInNewWindow = role === 'assistant',
		selectionActions = role === 'assistant',
		onselectionaction,
		visualizations = [],
		visualizationUrl,
		visualizationFollowUpTarget = 'this thread',
		onvisualizationfollowup,
		ondelete,
		deleting = false,
		children,
	}: {
		role: MessageRole;
		sender: string;
		timestampLabel: string;
		content: string;
		avatar: string;
		badge?: string;
		transcriptTimestamp?: string;
		openLinksInNewWindow?: boolean;
		selectionActions?: boolean;
		onselectionaction?: (action: SelectionAction, text: string) => void;
		visualizations?: MessageVisualization[];
		visualizationUrl?: (artifact: MessageVisualization) => string;
		visualizationFollowUpTarget?: string;
		onvisualizationfollowup?: (prompt: string, title?: string) => Promise<void>;
		ondelete?: () => void;
		deleting?: boolean;
		children?: Snippet;
	} = $props();

	let contentElement = $state<HTMLDivElement>();
	let selectedText = $state('');
	let actionLeft = $state(0);
	let actionTop = $state(0);
	let copied = $state(false);
	const actions: SelectionAction[] = ['Explain', 'Improve', 'Shorten', 'Tone', 'Grammar'];
	let fromUser = $derived(role === 'user');
	let isSystem = $derived(role === 'system');
	let contentBlocks = $derived(splitMessageVisualizations(content, role, visualizations));
	let hasWideVisualization = $derived(contentBlocks.some((block) => block.kind === 'visualization' && block.mode === 'wide'));

	function watchSelection(node: HTMLElement) {
		const handleSelection = () => requestAnimationFrame(captureSelection);
		node.addEventListener('pointerup', handleSelection);
		node.addEventListener('keyup', handleSelection);

		return {
			destroy() {
				node.removeEventListener('pointerup', handleSelection);
				node.removeEventListener('keyup', handleSelection);
			},
		};
	}

	function captureSelection() {
		if (!selectionActions || !contentElement) return clearSelectionActions();
		const selection = window.getSelection();
		if (!selection || selection.isCollapsed || selection.rangeCount === 0) return clearSelectionActions();
		const range = selection.getRangeAt(0);
		if (!contentElement.contains(range.commonAncestorContainer)) return clearSelectionActions();
		const text = selection.toString().trim();
		if (!text) return clearSelectionActions();

		const selectionRect = range.getBoundingClientRect();
		const contentRect = contentElement.getBoundingClientRect();
		const width = Math.min(330, contentRect.width);
		actionLeft = Math.max(8, Math.min(contentRect.width - width - 8, selectionRect.left - contentRect.left + selectionRect.width / 2 - width / 2));
		actionTop = selectionRect.top - contentRect.top - 40;
		if (actionTop < 0) actionTop = selectionRect.bottom - contentRect.top + 8;
		selectedText = text;
	}

	function clearSelectionActions() {
		selectedText = '';
	}

	function runSelectionAction(action: SelectionAction) {
		if (!selectedText) return;
		onselectionaction?.(action, selectedText);
		window.getSelection()?.removeAllRanges();
		clearSelectionActions();
	}

	async function copyMessage() {
		try {
			await navigator.clipboard.writeText(content);
			copied = true;
			window.setTimeout(() => (copied = false), 1_500);
		} catch {
			copied = false;
		}
	}
</script>

<article
	data-transcript-kind="message"
	data-message-role={role}
	data-transcript-timestamp={transcriptTimestamp || undefined}
	class="group/message flex gap-3 py-2 {fromUser ? 'flex-row-reverse' : ''}"
>
	<div class="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold
		{isSystem ? 'bg-muted text-muted-foreground' : fromUser ? 'bg-primary text-primary-foreground' : 'bg-accent text-accent-foreground'}">
		{avatar}
	</div>
	<div class="min-w-0 {hasWideVisualization ? 'w-full max-w-[min(64rem,96%)]' : 'max-w-[min(48rem,86%)]'} {fromUser ? 'items-end' : ''}">
		<div class="mb-1 flex flex-wrap items-center gap-2 text-[11px] text-muted-foreground {fromUser ? 'justify-end' : ''}">
			<span class="font-medium {isSystem ? 'text-muted-foreground' : 'text-foreground'}">{sender}</span>
			{#if badge}<span class="ai-status-pill h-5 bg-accent px-1.5 text-[9px] font-semibold uppercase tracking-wide text-accent-foreground">{badge}</span>{/if}
			<span>{timestampLabel}</span>
			{#if ondelete}
				<button
					type="button"
					onclick={ondelete}
					disabled={deleting}
					aria-label={deleting ? 'Deleting message' : 'Delete message'}
					class="rounded px-1.5 py-0.5 text-[10px] opacity-100 hover:bg-destructive/10 hover:text-destructive disabled:opacity-50 sm:opacity-0 sm:group-hover/message:opacity-100 focus-visible:opacity-100"
				>{deleting ? 'Deleting…' : 'Delete'}</button>
			{/if}
		</div>
		<div
			bind:this={contentElement}
			use:watchSelection
			class="relative rounded-lg rounded-[10px] px-3.5 py-2.5 text-sm
				{isSystem ? 'bg-muted/55 text-xs italic text-muted-foreground shadow-[var(--shadow-hairline)]' :
				fromUser ? 'rounded-tr-[4px] bg-primary text-primary-foreground shadow-sm' :
				'rounded-tl-[4px] bg-card pr-10 text-card-foreground shadow-[var(--shadow-card)]'}"
		>
			{#each contentBlocks as block, index (`${block.kind}:${index}`)}
				{#if block.kind === 'text'}
					<div class="prose-chat max-w-none break-words {fromUser ? 'prose-chat-user' : ''}">
						{@html renderContent(block.content, { openLinksInNewWindow, renderStructuredAgentMarkup: role === 'assistant' })}
					</div>
				{:else}
					<VisualizationArtifact
						artifact={block.artifact}
						title={block.title}
						mode={block.mode}
						href={block.artifact && visualizationUrl ? visualizationUrl(block.artifact) : undefined}
						followUpTarget={visualizationFollowUpTarget}
						onfollowup={onvisualizationfollowup}
					/>
				{/if}
			{/each}
			{#if children}{@render children()}{/if}

			{#if !isSystem && !fromUser}
				<button
					type="button"
					onclick={copyMessage}
					aria-label={copied ? 'Message copied' : 'Copy message'}
					class="ai-icon-button absolute right-1.5 top-1.5 bg-card opacity-100 shadow-[var(--shadow-control)] transition-opacity sm:opacity-0 sm:group-hover/message:opacity-100 focus-visible:opacity-100"
				>
					{#if copied}
						<svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path stroke-linecap="round" stroke-linejoin="round" d="m5 12 4 4L19 6" /></svg>
					{:else}
						<svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="12" height="12" rx="2.5"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
					{/if}
				</button>
			{/if}

			{#if selectedText}
				<div
					data-selection-actions
					class="absolute z-30 flex max-w-[330px] overflow-hidden rounded-full bg-card p-0.5 shadow-[var(--shadow-overlay)]"
					style:left={`${actionLeft}px`}
					style:top={`${actionTop}px`}
				>
					{#each actions as action}
						<button
							type="button"
							onpointerdown={(event) => event.preventDefault()}
							onclick={() => runSelectionAction(action)}
							class="rounded-full px-2 py-1 text-[10.5px] font-medium text-muted-foreground transition-colors hover:bg-[hsl(var(--hover))] hover:text-foreground"
						>{action}</button>
					{/each}
				</div>
			{/if}
		</div>
	</div>
</article>
