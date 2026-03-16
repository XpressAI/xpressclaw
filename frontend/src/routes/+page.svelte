<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { setup, conversations, agents as agentsApi } from '$lib/api';
	import type { Agent } from '$lib/api';

	let status_text = $state('Connecting to server...');
	let loading = $state(true);
	let retries = 0;

	let message = $state('');
	let agentList = $state<Agent[]>([]);
	let selectedAgent = $state('');
	let sending = $state(false);

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

	async function send() {
		if (!message.trim() || sending) return;
		sending = true;

		try {
			// Create conversation with selected agent
			const conv = await conversations.create({
				participant_ids: selectedAgent ? [selectedAgent] : []
			});

			// Send the first message
			await conversations.sendMessage(conv.id, message.trim(), 'You');

			// Navigate to conversation view
			goto(`/conversations/${conv.id}`);
		} catch (e) {
			sending = false;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
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
			<h1 class="text-center text-3xl font-semibold text-foreground">
				{greeting()}
			</h1>

			<!-- Input box -->
			<div class="rounded-xl border border-border bg-card shadow-sm">
				<textarea
					bind:value={message}
					onkeydown={handleKeydown}
					placeholder="How can I help you today?"
					rows="3"
					disabled={sending}
					class="w-full resize-none rounded-t-xl bg-transparent px-4 pt-4 pb-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
				></textarea>
				<div class="flex items-center justify-between px-3 pb-3">
					<div></div>
					<div class="flex items-center gap-2">
						{#if agentList.length > 0}
							<select
								bind:value={selectedAgent}
								class="rounded-md border border-border bg-background px-2 py-1 text-xs text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
							>
								{#each agentList as agent}
									<option value={agent.id}>
										{agent.name}
									</option>
								{/each}
							</select>
						{/if}
						<button
							onclick={send}
							disabled={!message.trim() || sending}
							class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
						>
							{#if sending}
								<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary-foreground border-t-transparent"></span>
							{:else}
								<svg class="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 12h14M12 5l7 7-7 7"/></svg>
							{/if}
						</button>
					</div>
				</div>
			</div>
		</div>
	</div>
{/if}
