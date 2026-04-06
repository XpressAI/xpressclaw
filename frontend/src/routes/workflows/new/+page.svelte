<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { workflows } from '$lib/api';

	const DEFAULT_YAML = `name: new-workflow
description: ""
version: 1

nodes:
  - id: step1
    label: "Process Input"
    agent: ""
    prompt: |
      Process the incoming request.
    position: { x: 300, y: 50 }

  - id: step2
    label: "Review Output"
    agent: ""
    prompt: |
      Review the output from the previous step.
    position: { x: 300, y: 250 }

  - id: notify
    label: "Send Notification"
    type: sink
    sinks:
      - connector: ""
        channel: ""
        template: "Workflow completed!"
    position: { x: 300, y: 450 }

edges:
  - from: step1
    to: step2
    condition: completed
  - from: step2
    to: notify
    condition: completed
`;

	let error = $state('');

	onMount(async () => {
		// Find a unique name by checking existing workflows
		try {
			const existing = await workflows.list();
			const names = new Set(existing.map(w => w.name));
			let name = 'New Workflow';
			let n = 2;
			while (names.has(name)) {
				name = `New Workflow ${n}`;
				n++;
			}
			const yamlWithName = DEFAULT_YAML.replace('name: new-workflow', `name: ${name.toLowerCase().replace(/\s+/g, '-')}`);
			const wf = await workflows.create({
				name,
				description: '',
				yaml_content: yamlWithName
			});
			goto(`/workflows/${wf.id}`, { replaceState: true });
		} catch (e) {
			error = String(e);
		}
	});
</script>

<div class="flex h-full items-center justify-center">
	{#if error}
		<div class="rounded-lg border border-border bg-card p-8 text-center space-y-3 max-w-sm">
			<div class="text-sm text-destructive">{error}</div>
			<a
				href="/workflows"
				class="inline-flex rounded-md border border-border px-3 py-1.5 text-sm hover:bg-accent transition-colors"
			>
				Back to Workflows
			</a>
		</div>
	{:else}
		<div class="text-sm text-muted-foreground flex items-center gap-2">
			<svg class="h-4 w-4 animate-spin" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
				<path stroke-linecap="round" stroke-linejoin="round" d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0l3.181 3.183a8.25 8.25 0 0013.803-3.7M4.031 9.865a8.25 8.25 0 0113.803-3.7l3.181 3.182M0 0" />
			</svg>
			Creating workflow...
		</div>
	{/if}
</div>
