<script lang="ts">
	import { page } from '$app/stores';
	import ProjectView from './ProjectView.svelte';

	let agentId = $derived($page.params.id!);
</script>

{#key agentId}
	<ProjectView {agentId} />
{/key}
