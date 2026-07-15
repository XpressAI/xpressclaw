<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { agents, workflows } from '$lib/api';
	import type { Agent } from '$lib/api';

	let agentList = $state<Agent[]>([]);
	let existingNames = $state(new Set<string>());
	let template = $state<'blank' | 'code-review'>('code-review');
	let workflowName = $state('Code Review Loop');
	let builderId = $state('');
	let reviewerId = $state('');
	let loading = $state(true);
	let creating = $state(false);
	let error = $state('');

	onMount(async () => {
		try {
			const [existing, sessions] = await Promise.all([workflows.list(), agents.list()]);
			existingNames = new Set(existing.map((workflow) => workflow.name));
			agentList = sessions;
			builderId = sessions.find((agent) => agent.backend.toLowerCase().includes('codex'))?.id ?? sessions[0]?.id ?? '';
			reviewerId = sessions.find((agent) => agent.backend.toLowerCase().includes('claude'))?.id ?? sessions[1]?.id ?? sessions[0]?.id ?? '';
			workflowName = uniqueName(workflowName);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loading = false;
		}
	});

	function uniqueName(base: string): string {
		if (!existingNames.has(base)) return base;
		let suffix = 2;
		while (existingNames.has(`${base} ${suffix}`)) suffix += 1;
		return `${base} ${suffix}`;
	}

	function displayName(agent: Agent): string {
		return agent.config?.display_name || agent.name;
	}

	function chooseTemplate(value: 'blank' | 'code-review') {
		template = value;
		workflowName = uniqueName(value === 'code-review' ? 'Code Review Loop' : 'New Workflow');
	}

	function blankYaml(name: string): string {
		return `name: ${JSON.stringify(name.toLowerCase().replace(/\s+/g, '-'))}
description: A reusable native-agent workflow.
version: 1

variables:
  goal: "Describe the requested outcome here"

flows:
  main:
    color: "#22c55e"
    steps:
      - id: work
        type: step
        label: Do the work
        agent: ${JSON.stringify(builderId)}
        prompt: |
          Complete this goal autonomously in the configured workspace:

          @goal
        outputs:
          result:
            type: string
            description: Outcome and verification summary.
`;
	}

	function codeReviewYaml(name: string): string {
		return `name: ${JSON.stringify(name.toLowerCase().replace(/\s+/g, '-'))}
description: One session implements, another reviews, and the loop continues until approval.
version: 1

variables:
  goal: "Describe the requested code change here"

flows:
  main:
    color: "#4A90D9"
    steps:
      - id: implement
        type: step
        label: Implementation
        agent: ${JSON.stringify(builderId)}
        prompt: |
          Implement this goal in the current repository:

          @goal

          Address feedback from the previous review if present:

          @review.feedback

          Run the relevant checks and leave the working tree ready for review.
        outputs:
          summary:
            type: string
            description: What changed and how it was verified.

      - id: review
        type: step
        label: Independent review
        agent: ${JSON.stringify(reviewerId)}
        prompt: |
          Review the actual implementation and diff for this goal:

          @goal

          Approve only when it is correct, complete, maintainable, and tested.
          Return a verdict of exactly "approved" or "changes_requested".
        outputs:
          verdict:
            type: string
            description: Either approved or changes_requested.
          feedback:
            type: string
            description: Specific actionable feedback for the implementer.

      - id: review_gate
        type: when
        label: Review decision
        switch: "@review.verdict"
        arms:
          - match: approved
            continue: true
          - match: changes_requested
            goto: step implement

      - id: mark_ready
        type: step
        label: Mark PR ready
        agent: ${JSON.stringify(builderId)}
        prompt: |
          The independent review approved the implementation. Verify final checks,
          push the branch if needed, and mark its GitHub pull request ready for review.
        outputs:
          pull_request_url:
            type: string
            description: URL of the ready pull request.
          checks:
            type: string
            description: Final verification summary.
`;
	}

	async function createWorkflow() {
		if (!workflowName.trim() || !builderId || creating) return;
		creating = true;
		error = '';
		try {
			const yaml = template === 'code-review' ? codeReviewYaml(workflowName.trim()) : blankYaml(workflowName.trim());
			const description = template === 'code-review'
				? 'Implementation and independent review loop using native sessions.'
				: 'A reusable native-agent workflow.';
			const workflow = await workflows.create({ name: workflowName.trim(), description, yaml_content: yaml });
			goto(`/workflows/${workflow.id}`);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			creating = false;
		}
	}
</script>

<div class="mx-auto w-full max-w-3xl space-y-6 p-6">
	<div>
		<a href="/workflows" class="text-xs text-muted-foreground hover:text-foreground">← Workflows</a>
		<h1 class="mt-2 text-2xl font-bold">New workflow</h1>
		<p class="mt-1 text-sm text-muted-foreground">Start with a working multi-session pattern or a single editable step.</p>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading sessions…</div>
	{:else if agentList.length === 0}
		<div class="rounded-xl border border-dashed border-border bg-card p-8 text-center">
			<h2 class="text-base font-semibold">Workflows need at least one session</h2>
			<p class="mt-2 text-sm text-muted-foreground">Create the native sessions that will perform each step.</p>
			<a href="/setup?mode=add-session" class="mt-4 inline-flex rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">Create session</a>
		</div>
	{:else}
		<div class="grid gap-3 sm:grid-cols-2">
			<button onclick={() => chooseTemplate('code-review')} class="rounded-xl border p-4 text-left {template === 'code-review' ? 'border-primary bg-primary/5' : 'border-border bg-card hover:border-primary/40'}">
				<div class="text-sm font-semibold">Implementation + review loop</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">One session writes the code, another reviews it, and rejected changes loop back until approval before the PR is marked ready.</p>
			</button>
			<button onclick={() => chooseTemplate('blank')} class="rounded-xl border p-4 text-left {template === 'blank' ? 'border-primary bg-primary/5' : 'border-border bg-card hover:border-primary/40'}">
				<div class="text-sm font-semibold">Single-step workflow</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">A minimal executable workflow you can extend in the visual editor.</p>
			</button>
		</div>

		<div class="space-y-4 rounded-xl border border-border bg-card p-5">
			<div>
				<label for="workflow-name" class="mb-1 block text-xs font-medium text-muted-foreground">Name</label>
				<input id="workflow-name" bind:value={workflowName} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring" />
			</div>
			<div class="grid gap-4 sm:grid-cols-2">
				<div>
					<label for="builder-session" class="mb-1 block text-xs font-medium text-muted-foreground">{template === 'code-review' ? 'Implementation session' : 'Session'}</label>
					<select id="builder-session" bind:value={builderId} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
						{#each agentList as agent}<option value={agent.id}>{displayName(agent)} · {agent.backend}</option>{/each}
					</select>
				</div>
				{#if template === 'code-review'}
					<div>
						<label for="reviewer-session" class="mb-1 block text-xs font-medium text-muted-foreground">Review session</label>
						<select id="reviewer-session" bind:value={reviewerId} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
							{#each agentList as agent}<option value={agent.id}>{displayName(agent)} · {agent.backend}</option>{/each}
						</select>
					</div>
				{/if}
			</div>
			{#if template === 'code-review' && builderId === reviewerId}
				<p class="text-xs text-amber-600">Using separate implementation and review sessions gives you a genuinely independent review.</p>
			{:else if template === 'code-review'}
				<p class="text-xs text-muted-foreground">Both sessions should use the same project workspace so the reviewer sees the implementer's actual diff.</p>
			{/if}
			{#if error}<p class="text-xs text-destructive">{error}</p>{/if}
			<div class="flex justify-end gap-2">
				<a href="/workflows" class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</a>
				<button onclick={createWorkflow} disabled={!workflowName.trim() || !builderId || (template === 'code-review' && !reviewerId) || creating} class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{creating ? 'Creating…' : 'Create workflow'}</button>
			</div>
		</div>
	{/if}
</div>
