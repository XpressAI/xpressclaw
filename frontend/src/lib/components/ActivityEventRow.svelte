<script lang="ts">
	import type { SessionEvent } from '$lib/api';
	import { renderContent } from '$lib/formatMessage';
	import { timeAgo } from '$lib/utils';

	let { event }: { event: SessionEvent } = $props();
	let expanded = $state(false);

	interface ToolDiff {
		path: string;
		oldText: string;
		newText: string;
	}

	let itemType = $derived(String(event.payload?.item_type ?? ''));
	let richText = $derived(itemType === 'agent_message' || event.event_type === 'agent_thought');
	let isTool = $derived(event.event_type === 'tool_call');
	let toolDiffs = $derived(extractToolDiffs(event));
	let toolContent = $derived(toolContentText(event));
	let toolOutput = $derived(toolOutputText(event));
	let toolInput = $derived(toolInputText(event));
	let details = $derived(detailText(event));

	function eventLabel(eventType: string): string {
		const labels: Record<string, string> = {
			attempt_queued: 'Queued',
			attempt_preparing: 'Preparing',
			attempt_running: 'Running',
			attempt_failed: 'Failed',
			attempt_cancelled: 'Cancelled',
			attempt_interrupted: 'Interrupted',
			runner_progress: itemType === 'agent_message' ? 'Update' : 'Progress',
			agent_thought: 'Thought',
			tool_call: 'Tool',
			session_fork: 'Session',
			session_fork_fallback: 'Session',
			session_config_options: 'Session',
			available_commands: 'Commands',
			session_info: 'Session',
		};
		return labels[eventType] ?? eventType.replaceAll('_', ' ');
	}

	function labelTone(eventType: string): string {
		if (eventType.includes('failed')) return 'text-red-400/80';
		if (eventType === 'attempt_interrupted') return 'text-amber-400/80';
		if (eventType === 'attempt_running') return 'text-emerald-400/75';
		if (eventType === 'agent_thought') return 'text-violet-300/70';
		return 'text-foreground/45';
	}

	function detailText(item: SessionEvent): string {
		const ignored = new Set(['sessionUpdate', 'status', 'error', 'item_type']);
		if (item.event_type === 'tool_call') {
			for (const key of ['toolCallId', 'title', 'kind', 'content', 'rawInput', 'rawOutput', '_meta', 'updatedAt']) {
				ignored.add(key);
			}
		}
		const detail = Object.fromEntries(
			Object.entries(item.payload ?? {}).filter(([key, value]) => !ignored.has(key) && value !== null)
		);
		if (Object.keys(detail).length === 0) return '';
		try {
			return JSON.stringify(detail, null, 2);
		} catch {
			return String(detail);
		}
	}

	function isRecord(value: unknown): value is Record<string, unknown> {
		return typeof value === 'object' && value !== null && !Array.isArray(value);
	}

	function extractToolDiffs(item: SessionEvent): ToolDiff[] {
		if (!Array.isArray(item.payload?.content)) return [];
		return item.payload.content.flatMap((content): ToolDiff[] => {
			if (!isRecord(content) || content.type !== 'diff') return [];
			const oldText = typeof content.oldText === 'string' ? content.oldText : '';
			const newText = typeof content.newText === 'string' ? content.newText : '';
			if (!oldText && !newText) return [];
			return [{
				path: typeof content.path === 'string' ? content.path.replace(/^\/workspace\//, '') : 'Changed content',
				oldText,
				newText,
			}];
		});
	}

	function prettyValue(value: unknown): string {
		if (typeof value === 'string') return value;
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return String(value);
		}
	}

	function contentText(value: unknown): string {
		if (typeof value === 'string') return value;
		if (!isRecord(value)) return '';
		if (typeof value.text === 'string') return value.text;
		if ('content' in value) return contentText(value.content);
		return '';
	}

	function toolContentText(item: SessionEvent): string {
		if (!Array.isArray(item.payload?.content)) return '';
		return item.payload.content
			.filter(content => !isRecord(content) || content.type !== 'diff')
			.map(contentText)
			.filter(Boolean)
			.join('\n\n');
	}

	function toolOutputText(item: SessionEvent): string {
		const output = item.payload?.rawOutput;
		if (output === undefined || output === null) return '';
		if (isRecord(output)) {
			for (const key of ['formatted_output', 'formattedOutput', 'output', 'text']) {
				if (typeof output[key] === 'string') return output[key];
			}
		}
		return prettyValue(output);
	}

	function toolInputText(item: SessionEvent): string {
		const input = item.payload?.rawInput;
		return input === undefined || input === null ? '' : prettyValue(input);
	}
</script>

<div class="group/event">
	<button
		type="button"
		onclick={() => (expanded = !expanded)}
		aria-expanded={expanded}
		class="grid w-full grid-cols-[4.5rem_minmax(0,1fr)_auto_auto] items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-secondary/25 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring sm:grid-cols-[5.25rem_minmax(0,1fr)_auto_auto]"
	>
		<span class="truncate text-[11px] font-medium capitalize {labelTone(event.event_type)}">
			{eventLabel(event.event_type)}
		</span>
		<span class="truncate text-xs text-muted-foreground/80 group-hover/event:text-muted-foreground" title={event.summary}>
			{event.summary}
		</span>
		<span class="whitespace-nowrap text-[10px] text-muted-foreground/45">{timeAgo(event.created_at)}</span>
		<svg class="h-3 w-3 text-muted-foreground/35 transition-transform {expanded ? 'rotate-180' : ''}"
			fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24" aria-hidden="true">
			<path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" />
		</svg>
	</button>

	{#if expanded}
		<div class="mb-1 ml-[5.25rem] mr-2 border-l border-border/40 py-1.5 pl-3 text-xs sm:ml-[6rem]">
			{#if richText}
				<div class="prose prose-invert prose-sm max-w-none text-xs text-foreground/80">
					{@html renderContent(event.summary)}
				</div>
			{:else if !isTool}
				<div class="whitespace-pre-wrap text-foreground/75">{event.summary}</div>
			{/if}
			{#if isTool && toolDiffs.length > 0}
				<div class="space-y-3" data-tool-diffs>
					{#each toolDiffs as diff}
						<section class="overflow-hidden rounded-md border border-border/50">
							<div class="border-b border-border/50 bg-secondary/25 px-3 py-1.5 font-mono text-[11px] text-foreground/80">{diff.path}</div>
							<div class="grid gap-px bg-border/40 lg:grid-cols-2">
								<div class="min-w-0 bg-background/95">
									<div class="border-b border-red-500/15 bg-red-500/5 px-3 py-1 text-[10px] font-medium uppercase tracking-wide text-red-300/75">Before</div>
									<pre class="max-h-96 overflow-auto whitespace-pre p-3 font-mono text-[11px] leading-relaxed text-red-100/75">{diff.oldText || '(empty)'}</pre>
								</div>
								<div class="min-w-0 bg-background/95">
									<div class="border-b border-emerald-500/15 bg-emerald-500/5 px-3 py-1 text-[10px] font-medium uppercase tracking-wide text-emerald-300/75">After</div>
									<pre class="max-h-96 overflow-auto whitespace-pre p-3 font-mono text-[11px] leading-relaxed text-emerald-100/75">{diff.newText || '(empty)'}</pre>
								</div>
							</div>
						</section>
					{/each}
				</div>
			{/if}
			{#if isTool && toolContent}
				<div class="mt-2 whitespace-pre-wrap rounded-md bg-black/10 px-3 py-2 text-foreground/75">{toolContent}</div>
			{/if}
			{#if isTool && toolOutput}
				<div class="mt-2">
					<div class="mb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground/55">Output</div>
					<pre class="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/15 px-3 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground/80">{toolOutput}</pre>
				</div>
			{/if}
			{#if isTool && toolInput}
				<details class="mt-2">
					<summary class="cursor-pointer select-none text-[10px] font-medium uppercase tracking-wide text-muted-foreground/55">Input</summary>
					<pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/15 px-3 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground/75">{toolInput}</pre>
				</details>
			{/if}
			{#if details}
				<pre class="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/15 px-3 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground/75">{details}</pre>
			{/if}
			<div class="mt-1.5 text-[10px] text-muted-foreground/40">
				{event.source_type}{event.source_id ? ` · ${event.source_id}` : ''}{typeof event.payload?.kind === 'string' ? ` · ${event.payload.kind}` : ''}{typeof event.payload?.status === 'string' ? ` · ${event.payload.status}` : ''}
			</div>
		</div>
	{/if}
</div>
