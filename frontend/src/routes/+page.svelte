<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { setup, sessions, agents as agentsApi } from '$lib/api';
	import type { Agent } from '$lib/api';
	import { agentAvatar } from '$lib/utils';

	let status_text = $state('Connecting to server...');
	let loading = $state(true);
	let retries = 0;

	let message = $state('');
	let agentList = $state<Agent[]>([]);
	let selectedAgent = $state('');
	let sending = $state(false);
	let composing = $state(false);
	let sendError = $state('');

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
			if (agts.length > 0) selectedAgent = agts[0].id;
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

	onMount(checkReady);

	function greeting(): string {
		const hour = new Date().getHours();
		if (hour < 12) return 'Good morning';
		if (hour < 18) return 'Good afternoon';
		return 'Good evening';
	}

	let selectedAgentObj = $derived(agentList.find(a => a.id === selectedAgent));

	async function send() {
		if (!message.trim() || !selectedAgent || sending) return;
		sending = true;
		sendError = '';

		try {
			await sessions.sendMessage(selectedAgent, message.trim());
			goto(`/agents/${selectedAgent}`);
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
		} finally {
			sending = false;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !composing && e.keyCode !== 229) {
			e.preventDefault();
			send();
		}
	}
</script>

{#if loading}
	<div class="flex h-full flex-col items-center justify-center gap-3">
		<div class="h-8 w-8 animate-spin rounded-full border-2 border-muted-foreground border-t-primary"></div>
		<span class="text-sm text-muted-foreground">{status_text}</span>
	</div>
{:else}
	<div class="flex h-full flex-col items-center justify-center px-4">
		<div class="w-full max-w-2xl space-y-8">
			<!-- Greeting -->
			<div class="text-center">
				<h1 class="text-3xl font-semibold text-foreground">{greeting()}</h1>
				<p class="mt-2 text-sm text-muted-foreground">What would you like to get done?</p>
			</div>

			{#if agentList.length === 0}
				<div class="rounded-2xl border border-dashed border-border bg-card p-8 text-center">
					<h2 class="text-base font-semibold">Create a native session first</h2>
					<p class="mx-auto mt-2 max-w-md text-sm text-muted-foreground">Choose Codex, Claude Code, OpenCode, or a custom runner and connect it to a project workspace.</p>
					<a href="/setup?mode=add-agent" class="mt-5 inline-flex rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Create session</a>
				</div>
			{:else}
			<!-- Queue work directly into the selected logical session. -->
			<div class="rounded-2xl border border-border bg-card shadow-lg shadow-black/10">
				<textarea
					bind:value={message}
					onkeydown={handleKeydown}
					oncompositionstart={() => (composing = true)}
					oncompositionend={() => setTimeout(() => (composing = false), 0)}
					placeholder="Describe the outcome you want…"
					rows="3"
					disabled={sending}
					class="w-full resize-none rounded-t-2xl bg-transparent px-5 pt-5 pb-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
				></textarea>
				<div class="flex items-center justify-between px-4 pb-4">
					<div></div>
					<div class="flex items-center gap-3">
						<div class="flex items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
								{#if selectedAgentObj}
									<img src={agentAvatar(selectedAgentObj)} alt="" class="h-5 w-5 rounded-full object-cover" />
								{/if}
								<select
									bind:value={selectedAgent}
									class="bg-transparent text-xs text-foreground focus:outline-none cursor-pointer"
								>
									{#each agentList as agent}
										<option value={agent.id}>
											{agent.config?.display_name || agent.name}
										</option>
									{/each}
								</select>
						</div>
						<button
							onclick={send}
							disabled={!message.trim() || sending}
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
			<div class="text-center"><a href="/agents" class="text-xs text-muted-foreground hover:text-foreground hover:underline">Manage sessions</a></div>
			{/if}
		</div>
	</div>
{/if}
