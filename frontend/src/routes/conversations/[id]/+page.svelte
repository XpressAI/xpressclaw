<script lang="ts">
	import { page } from '$app/stores';
	import ConversationView from './ConversationView.svelte';

	let conversationId = $derived($page.params.id!);
</script>

{#key conversationId}<ConversationView {conversationId} />{/key}
