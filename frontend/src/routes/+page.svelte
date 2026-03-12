<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { conversations } from '$lib/api';

	onMount(async () => {
		try {
			const convs = await conversations.list(1);
			if (convs.length > 0) {
				goto(`/conversations/${convs[0].id}`, { replaceState: true });
			} else {
				goto('/dashboard', { replaceState: true });
			}
		} catch {
			goto('/dashboard', { replaceState: true });
		}
	});
</script>

<div class="flex h-full items-center justify-center text-muted-foreground text-sm">
	Loading...
</div>
