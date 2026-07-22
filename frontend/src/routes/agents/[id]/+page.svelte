<script lang="ts">
	import { page } from '$app/stores';
	import { projectSection } from '$lib/workspace';
	import ProjectView from './ProjectView.svelte';

	let agentId = $derived($page.params.id!);
	let section = $derived(projectSection(`${$page.url.pathname}${$page.url.search}`));
</script>

{#key agentId}
	<ProjectView {agentId} {section} />
{/key}
