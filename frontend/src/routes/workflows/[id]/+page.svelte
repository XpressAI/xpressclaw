<script lang="ts">
	import { page } from '$app/stores';
	import WorkflowView from './WorkflowView.svelte';

	let workflowId = $derived($page.params.id!);
</script>

{#key workflowId}
	<WorkflowView {workflowId} />
{/key}
