<script lang="ts">
	import { page } from '$app/stores';
	import TaskView from './TaskView.svelte';

	let taskId = $derived($page.params.id!);
</script>

{#key taskId}
	<TaskView {taskId} />
{/key}
