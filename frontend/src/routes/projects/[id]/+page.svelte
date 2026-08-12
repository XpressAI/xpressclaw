<script lang="ts">
	import { page } from '$app/stores';
	import ProjectOverview from './ProjectOverview.svelte';

	let projectId = $derived($page.params.id!);
</script>

{#key projectId}<ProjectOverview {projectId} />{/key}
