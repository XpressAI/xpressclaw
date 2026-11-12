<script lang="ts">
	import { onMount } from 'svelte';
	import { sessions } from '$lib/api';
	import type { AgentAuthentication } from '$lib/api';
	import { harnessName } from '$lib/utils';
	import TerminalPanel from './TerminalPanel.svelte';
	import AgentAuthenticationFlow from './AgentAuthenticationFlow.svelte';
	let { agentId, onclose, onauthenticated, onretry }: {
		agentId: string; onclose: () => void; onauthenticated?: () => void; onretry?: () => Promise<void>;
	} = $props();
	let dialog = $state<HTMLDialogElement>();
	let loading = $state(true);
	let error = $state('');
	let authentication = $state<AgentAuthentication | null>(null);
	let method = $state<string | null>(null);
	let selectedMethod = $derived(authentication?.methods.find(option => option.id === method));
	let authenticated = $state(false);
	let retrying = $state(false);
	async function prepare() {
		loading = true; error = ''; method = null;
		try {
			authentication = await sessions.prepareAuthentication(agentId);
			const interactive = authentication.methods.filter(option => option.kind === 'terminal');
			if (interactive.length === 1) method = interactive[0].id;
			else if (authentication.methods.length === 1) method = authentication.methods[0].id;
			else if (!authentication.methods.length) error = 'This runner does not provide an interactive sign-in method.';
		} catch (cause) { error = cause instanceof Error ? cause.message : String(cause); }
		finally { loading = false; }
	}
	onMount(() => { dialog?.showModal(); void prepare(); });
	function complete() { authenticated = true; onauthenticated?.(); }
	async function retry() {
		if (!onretry || retrying) return;
		retrying = true; error = '';
		try { await onretry(); onclose(); }
		catch (cause) { error = cause instanceof Error ? cause.message : String(cause); }
		finally { retrying = false; }
	}
</script>

<dialog bind:this={dialog} onclose={onclose} class="m-auto w-[min(58rem,calc(100vw-2rem))] rounded-xl border border-border bg-card p-0 text-foreground shadow-xl backdrop:bg-black/50" aria-label="Sign in to Agent">
	<div class="flex items-center justify-between border-b border-border px-5 py-4">
		<h2 class="text-lg font-semibold">Sign in{authentication ? ` to ${harnessName(authentication.kind)}` : ' to Agent'}</h2>
		<button type="button" onclick={onclose} aria-label="Close sign-in" class="rounded px-2 py-1 hover:bg-accent">✕</button>
	</div>
	<div class="p-5">
		{#if authenticated}
			<p role="status" class="text-emerald-600 dark:text-emerald-400">Signed in successfully.</p>
		{:else if loading}
			<p role="status" class="text-sm text-muted-foreground">Preparing the Agent's sign-in…</p>
		{:else if method && selectedMethod?.kind === 'terminal'}
			<p class="mb-3 text-sm text-muted-foreground">Complete the provider's sign-in below. Links open in your browser. XpressClaw reconnects the Agent when sign-in succeeds.</p>
			<div class="h-[min(26rem,60vh)] overflow-hidden rounded border border-border">
				{#key method}<TerminalPanel {agentId} authenticationMethod={method} onAuthenticated={complete} />{/key}
			</div>
		{:else if selectedMethod}
			{#key selectedMethod.id}<AgentAuthenticationFlow {agentId} method={selectedMethod} onauthenticated={complete} />{/key}
		{:else if authentication}
			<div class="space-y-2">
				{#each authentication.methods as option}
					<button type="button" onclick={() => method = option.id} class="block w-full rounded border border-border p-3 text-left hover:bg-accent">
						<span class="font-medium">{option.name}</span>
						{#if option.description}<span class="mt-1 block text-sm text-muted-foreground">{option.description}</span>{/if}
					</button>
				{/each}
			</div>
		{/if}
		{#if error}<p role="alert" class="mt-3 text-sm text-destructive">{error}</p>{/if}
	</div>
	<div class="flex justify-end gap-2 border-t border-border px-5 py-3">
		{#if method && !authenticated && (authentication?.methods.length ?? 0) > 1}<button type="button" onclick={() => method = null} class="mr-auto rounded border border-border px-3 py-1.5">Other sign-in methods</button>{/if}
		{#if error && !authenticated}<button type="button" onclick={prepare} class="rounded border border-border px-3 py-1.5">Try again</button>{/if}
		<button type="button" onclick={onclose} class="rounded border border-border px-3 py-1.5">{authenticated ? 'Done' : 'Cancel'}</button>
		{#if authenticated && onretry}<button type="button" onclick={retry} disabled={retrying} class="rounded bg-primary px-3 py-1.5 text-primary-foreground disabled:opacity-50">{retrying ? 'Retrying…' : 'Retry task'}</button>{/if}
	</div>
</dialog>
