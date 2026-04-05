<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { agents } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let { agentId }: { agentId: string } = $props();

	let logs = $state('');
	let loading = $state(true);
	let autoRefresh = $state(true);
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let logsEl: HTMLPreElement;

	async function fetchLogs() {
		try {
			const result = await agents.logs(agentId, 200);
			logs = result.logs || '';
			loading = false;
			scrollToBottom();
		} catch (e) {
			logs = `Error fetching logs: ${e}`;
			loading = false;
		}
	}

	function scrollToBottom() {
		setTimeout(() => {
			if (logsEl) logsEl.scrollTop = logsEl.scrollHeight;
		}, 50);
	}

	onMount(() => {
		fetchLogs();
		pollTimer = setInterval(() => {
			if (autoRefresh) fetchLogs();
		}, 5000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<div class="space-y-3">
	<div class="flex items-center justify-between">
		<h2 class="text-sm font-semibold">Container Logs</h2>
		<div class="flex items-center gap-3">
			<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
				<input type="checkbox" bind:checked={autoRefresh} class="rounded" />
				Auto-refresh
			</label>
			<button onclick={fetchLogs}
				class="rounded-md border border-border px-2.5 py-1 text-xs hover:bg-accent transition-colors">
				Refresh
			</button>
		</div>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading logs...</div>
	{:else if !logs}
		<div class="text-sm text-muted-foreground">No logs available. Is the agent running?</div>
	{:else}
		<pre bind:this={logsEl}
			class="rounded-lg border border-border bg-muted/30 p-4 text-xs font-mono overflow-auto max-h-[calc(100vh-16rem)] whitespace-pre-wrap break-words"
		>{logs}</pre>
	{/if}
</div>
