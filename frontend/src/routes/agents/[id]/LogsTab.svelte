<script lang="ts">
	import { onMount, onDestroy } from 'svelte';

	let { agentId }: { agentId: string } = $props();

	type Line = {
		agent_id: string;
		stream: 'stdout' | 'stderr';
		line: string;
		ts_ms: number;
	};

	let lines = $state<Line[]>([]);
	let connected = $state(false);
	let autoScroll = $state(true);
	let termEl: HTMLDivElement | undefined;
	let source: EventSource | null = null;

	const MAX_LINES = 2000;

	function connect() {
		if (source) source.close();
		source = new EventSource(`/api/agents/${agentId}/terminal`);
		source.addEventListener('line', (ev) => {
			try {
				const parsed = JSON.parse(ev.data) as Line;
				lines = [...lines.slice(-MAX_LINES + 1), parsed];
				if (autoScroll) queueMicrotask(scrollToBottom);
			} catch {
				// ignore malformed
			}
		});
		source.onopen = () => {
			connected = true;
		};
		source.onerror = () => {
			connected = false;
			// Browser will retry automatically.
		};
	}

	function scrollToBottom() {
		if (termEl) termEl.scrollTop = termEl.scrollHeight;
	}

	function clearTerm() {
		lines = [];
	}

	function fmtTime(ms: number) {
		const d = new Date(ms);
		return d.toLocaleTimeString(undefined, { hour12: false }) + '.' + String(d.getMilliseconds()).padStart(3, '0');
	}

	onMount(() => {
		connect();
	});

	onDestroy(() => {
		if (source) source.close();
	});
</script>

<div class="flex flex-col h-[calc(100vh-12rem)] gap-2">
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-2 text-xs">
			<span class="h-2 w-2 rounded-full {connected ? 'bg-green-500' : 'bg-zinc-500'}"></span>
			<span class="font-mono text-muted-foreground">
				{connected ? 'streaming' : 'disconnected'} — {lines.length} line{lines.length === 1 ? '' : 's'}
			</span>
		</div>
		<div class="flex items-center gap-3">
			<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
				<input type="checkbox" bind:checked={autoScroll} class="rounded" />
				Follow
			</label>
			<button
				onclick={clearTerm}
				class="rounded-md border border-border px-2.5 py-1 text-xs hover:bg-accent transition-colors"
			>
				Clear
			</button>
		</div>
	</div>

	<div
		bind:this={termEl}
		class="flex-1 overflow-auto rounded-lg border border-border bg-[#0b0b0c] p-3 font-mono text-[12px] leading-[1.35] text-[#d4d4d4]"
	>
		{#if lines.length === 0}
			<div class="text-zinc-500 italic">
				Waiting for output… The pi-agent container is spawned on the first conversation or task.
			</div>
		{:else}
			{#each lines as l}
				<div class="whitespace-pre-wrap break-words">
					<span class="text-zinc-600">{fmtTime(l.ts_ms)}</span>
					<span class={l.stream === 'stderr' ? 'text-amber-400 ml-2' : 'text-zinc-500 ml-2'}>
						{l.stream === 'stderr' ? 'E' : 'O'}
					</span>
					<span class="ml-2 {l.stream === 'stderr' ? 'text-amber-100' : ''}">{l.line}</span>
				</div>
			{/each}
		{/if}
	</div>
</div>
