<script lang="ts">
	import type { SessionEvent } from '$lib/api';
	import { renderContent } from '$lib/formatMessage';
	import { timeAgo } from '$lib/utils';

	let { event }: { event: SessionEvent } = $props();
	let expanded = $state(false);

	let itemType = $derived(String(event.payload?.item_type ?? ''));
	let richText = $derived(itemType === 'agent_message' || event.event_type === 'agent_thought');
	let details = $derived(detailText(event));

	function eventLabel(eventType: string): string {
		const labels: Record<string, string> = {
			attempt_queued: 'Queued',
			attempt_preparing: 'Preparing',
			attempt_running: 'Running',
			attempt_failed: 'Failed',
			attempt_cancelled: 'Cancelled',
			runner_progress: itemType === 'agent_message' ? 'Update' : 'Progress',
			agent_thought: 'Thought',
			tool_call: 'Tool',
			tool_call_update: 'Tool',
			session_config_options: 'Session',
			available_commands: 'Commands',
			usage: 'Usage',
			session_info: 'Session',
		};
		return labels[eventType] ?? eventType.replaceAll('_', ' ');
	}

	function labelTone(eventType: string): string {
		if (eventType.includes('failed')) return 'text-red-400/80';
		if (eventType === 'attempt_running') return 'text-emerald-400/75';
		if (eventType === 'agent_thought') return 'text-violet-300/70';
		return 'text-foreground/45';
	}

	function detailText(item: SessionEvent): string {
		const ignored = new Set(['sessionUpdate', 'status', 'error', 'item_type']);
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
			{:else}
				<div class="whitespace-pre-wrap text-foreground/75">{event.summary}</div>
			{/if}
			{#if details}
				<pre class="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/15 px-3 py-2 font-mono text-[11px] leading-relaxed text-muted-foreground/75">{details}</pre>
			{/if}
			<div class="mt-1.5 text-[10px] text-muted-foreground/40">
				{event.source_type}{event.source_id ? ` · ${event.source_id}` : ''}
			</div>
		</div>
	{/if}
</div>
