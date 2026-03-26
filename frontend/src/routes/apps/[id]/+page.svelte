<script lang="ts">
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { apps, conversations } from '$lib/api';
	import type { App } from '$lib/api';
	import { goto } from '$app/navigation';

	let app = $state<App | null>(null);
	let error = $state<string | null>(null);

	$effect(() => {
		const id = $page.params.id;
		if (id) loadApp(id);
	});

	async function loadApp(id: string) {
		try {
			app = await apps.get(id);
			error = null;
		} catch (e) {
			error = `App not found: ${id}`;
		}
	}

	async function openAppChat() {
		if (!app) return;
		// If the app has a linked conversation, navigate to it
		if (app.conversation_id) {
			goto(`/conversations/${app.conversation_id}`);
			return;
		}
		// Otherwise create one with the creating agent
		try {
			const conv = await conversations.create({
				participant_ids: [app.agent_id]
			});
			// Update the app with the conversation ID
			// (future: backend endpoint for this)
			goto(`/conversations/${conv.id}?msg=${encodeURIComponent(`I'd like to discuss the "${app.title}" app.`)}`);
		} catch {}
	}
</script>

{#if error}
	<div class="flex h-full items-center justify-center">
		<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">
			{error}
		</div>
	</div>
{:else if !app}
	<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
		Loading...
	</div>
{:else}
	<div class="flex h-full flex-col">
		<!-- App Header -->
		<div class="flex items-center gap-3 border-b border-border px-5 py-2.5">
			<span class="text-lg">{app.icon ?? '📦'}</span>
			<div class="flex-1 min-w-0">
				<h1 class="text-sm font-semibold">{app.title}</h1>
				{#if app.description}
					<p class="text-xs text-muted-foreground truncate">{app.description}</p>
				{/if}
			</div>
			<div class="flex items-center gap-1">
				<!-- Chat with creating agent -->
				<button
					onclick={openAppChat}
					class="rounded-lg p-2 text-muted-foreground hover:bg-secondary hover:text-foreground transition-colors"
					title="Chat with {app.agent_id} about this app"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M8.625 12a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H8.25m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0H12m4.125 0a.375.375 0 11-.75 0 .375.375 0 01.75 0zm0 0h-.375M21 12c0 4.556-4.03 8.25-9 8.25a9.764 9.764 0 01-2.555-.337A5.972 5.972 0 015.41 20.97a5.969 5.969 0 01-.474-.065 4.48 4.48 0 00.978-2.025c.09-.457-.133-.901-.467-1.226C3.93 16.178 3 14.189 3 12c0-4.556 4.03-8.25 9-8.25s9 3.694 9 8.25z" /></svg>
				</button>
				<!-- Pop out -->
				<a
					href="/apps/{app.id}/"
					target="_blank"
					class="rounded-lg p-2 text-muted-foreground hover:bg-secondary hover:text-foreground transition-colors"
					title="Open in new window"
				>
					<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M13.5 6H5.25A2.25 2.25 0 003 8.25v10.5A2.25 2.25 0 005.25 21h10.5A2.25 2.25 0 0018 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25" /></svg>
				</a>
			</div>
		</div>

		<!-- App iframe -->
		<div class="flex-1">
			{#if app.status === 'running' && app.container_id}
				<iframe
					src="/apps/{app.id}/"
					class="w-full h-full border-0"
					sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
					title={app.title}
				></iframe>
			{:else}
				<div class="flex h-full items-center justify-center">
					<div class="text-center space-y-3">
						<span class="text-4xl">{app.icon ?? '📦'}</span>
						<h2 class="text-lg font-semibold">{app.title}</h2>
						<p class="text-sm text-muted-foreground">
							{#if app.status === 'stopped'}
								This app is not running.
							{:else if app.status === 'error'}
								This app encountered an error.
							{:else}
								This app is starting...
							{/if}
						</p>
						{#if app.description}
							<p class="text-xs text-muted-foreground">{app.description}</p>
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}
