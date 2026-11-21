<script lang="ts">
	import type { Agent } from '$lib/api';
	import TerminalPanel from '$lib/components/TerminalPanel.svelte';

	interface Props {
		agentId: string | null;
		initialSession: string | null;
		agents: Agent[];
		onUpdateNode: (patch: { agentId?: string | null; title?: string }) => void;
	}

	let { agentId, initialSession, agents, onUpdateNode }: Props = $props();

	let activeAgentId = $state('');

	$effect(() => {
		const next = agentId || agents[0]?.id || '';
		if (!activeAgentId && next) activeAgentId = next;
	});

	function selectAgent(nextId: string) {
		activeAgentId = nextId;
		const agent = agents.find((candidate) => candidate.id === nextId);
		onUpdateNode({
			agentId: nextId || null,
			title: agent ? `Terminal · ${agent.title || agent.name}` : 'Terminal',
		});
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background" data-canvas-terminal-card>
	{#if agents.length > 1 || !activeAgentId}
		<div class="flex items-center gap-2 border-b border-border bg-[hsl(var(--inset))] px-2.5 py-1 text-[11px]">
			<span class="text-muted-foreground">Agent</span>
			<select
				aria-label="Terminal agent"
				value={activeAgentId}
				onchange={(event) => selectAgent(event.currentTarget.value)}
				class="max-w-48 rounded border border-input bg-background px-1.5 py-0.5 text-[11px]"
			>
				{#if agents.length === 0}<option value="">No agents available</option>{/if}
				{#each agents as agent (agent.id)}
					<option value={agent.id}>{agent.title || agent.name}</option>
				{/each}
			</select>
		</div>
	{/if}
	<div class="min-h-0 flex-1">
		{#if activeAgentId}
			{#key `${activeAgentId}:${initialSession || 'xpressclaw'}`}
				<TerminalPanel agentId={activeAgentId} initialSession={initialSession || 'xpressclaw'} />
			{/key}
		{:else}
			<div class="flex h-full items-center justify-center p-6 text-xs text-muted-foreground">
				Select an agent to connect to its retained container terminal.
			</div>
		{/if}
	</div>
</div>
