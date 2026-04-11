<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { boot, shutdown, type WanixInstance } from './wanix-bridge';

	interface Props {
		agentId: string;
	}

	let { agentId }: Props = $props();

	let wanix = $state<WanixInstance | null>(null);
	let status = $state<'booting' | 'ready' | 'error'>('booting');
	let error = $state<string | null>(null);
	let terminalLines = $state<string[]>([]);
	let files = $state<Array<{ name: string; size: number; isDir: boolean }>>([]);

	// Terminal output element ref
	let terminalEl: HTMLPreElement;

	onMount(async () => {
		try {
			wanix = await boot();
			status = 'ready';

			// Create workspace directory for the agent
			try {
				await wanix.makeDir('/workspace');
			} catch {
				// already exists
			}

			// Refresh file list
			await refreshFiles();

			terminalLines = [...terminalLines, `[wanix] Agent "${agentId}" workspace ready.`];
		} catch (e) {
			status = 'error';
			error = e instanceof Error ? e.message : String(e);
		}
	});

	onDestroy(() => {
		// Don't shutdown — other components may use the same instance
	});

	async function refreshFiles() {
		if (!wanix) return;
		try {
			files = await wanix.readDir('/workspace');
		} catch {
			files = [];
		}
	}

	function formatSize(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	// Auto-scroll terminal
	$effect(() => {
		if (terminalEl && terminalLines.length > 0) {
			terminalEl.scrollTop = terminalEl.scrollHeight;
		}
	});
</script>

<div class="flex flex-col h-full gap-3">
	<!-- Status bar -->
	<div class="flex items-center gap-2 px-1">
		{#if status === 'booting'}
			<span class="inline-block w-2 h-2 rounded-full bg-yellow-500 animate-pulse"></span>
			<span class="text-xs text-muted-foreground">Booting Wanix...</span>
		{:else if status === 'ready'}
			<span class="inline-block w-2 h-2 rounded-full bg-green-500"></span>
			<span class="text-xs text-muted-foreground">Wanix ready</span>
		{:else}
			<span class="inline-block w-2 h-2 rounded-full bg-red-500"></span>
			<span class="text-xs text-destructive">{error}</span>
		{/if}
	</div>

	<!-- Terminal -->
	<div class="rounded-lg border border-border bg-black/80 flex-1 min-h-[200px]">
		<div class="flex items-center gap-2 px-3 py-1.5 border-b border-border/50">
			<span class="text-xs text-muted-foreground font-mono">Terminal</span>
		</div>
		<pre
			bind:this={terminalEl}
			class="p-3 text-xs font-mono text-green-400 overflow-y-auto max-h-[300px] whitespace-pre-wrap"
		>{terminalLines.join('\n')}</pre>
	</div>

	<!-- File tree -->
	<div class="rounded-lg border border-border bg-card flex-1 min-h-[150px]">
		<div class="flex items-center justify-between px-3 py-1.5 border-b border-border">
			<span class="text-xs font-semibold">/workspace</span>
			<button
				onclick={refreshFiles}
				class="text-xs text-muted-foreground hover:text-foreground"
			>
				Refresh
			</button>
		</div>
		<div class="p-2 overflow-y-auto max-h-[250px]">
			{#if files.length === 0}
				<p class="text-xs text-muted-foreground italic px-1">Empty workspace</p>
			{:else}
				{#each files as file}
					<div class="flex items-center gap-2 px-2 py-1 rounded hover:bg-accent text-xs font-mono">
						{#if file.isDir}
							<span class="text-blue-400">📁</span>
						{:else}
							<span class="text-muted-foreground">📄</span>
						{/if}
						<span class="flex-1 truncate">{file.name}</span>
						{#if !file.isDir}
							<span class="text-muted-foreground">{formatSize(file.size)}</span>
						{/if}
					</div>
				{/each}
			{/if}
		</div>
	</div>
</div>
