<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { setup, conversations } from '$lib/api';

	let status_text = $state('Connecting to server...');
	let retries = 0;

	async function checkAndRedirect() {
		try {
			status_text = 'Checking setup...';
			const status = await setup.status();
			if (!status.setup_complete) {
				goto('/setup', { replaceState: true });
				return;
			}

			status_text = 'Loading...';
			const convs = await conversations.list(1);
			if (convs.length > 0) {
				goto(`/conversations/${convs[0].id}`, { replaceState: true });
			} else {
				goto('/dashboard', { replaceState: true });
			}
		} catch {
			retries++;
			if (retries < 60) {
				status_text = 'Waiting for server...';
				setTimeout(checkAndRedirect, 500);
			} else {
				goto('/dashboard', { replaceState: true });
			}
		}
	}

	onMount(checkAndRedirect);
</script>

<div class="flex h-full flex-col items-center justify-center gap-3">
	<div class="h-8 w-8 animate-spin rounded-full border-2 border-muted-foreground border-t-primary"></div>
	<span class="text-sm text-muted-foreground">{status_text}</span>
</div>
