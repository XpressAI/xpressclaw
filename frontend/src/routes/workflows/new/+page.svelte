<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { agents, workflows } from '$lib/api';
	import type { Agent } from '$lib/api';

	let agentList = $state<Agent[]>([]);
	let existingNames = $state(new Set<string>());
	let template = $state<'blank' | 'code-review' | 'goal-loop'>('code-review');
	let workflowName = $state('Code Review Loop');
	let loading = $state(true);
	let creating = $state(false);
	let error = $state('');

	onMount(async () => {
		try {
			const [existing, sessions] = await Promise.all([workflows.list(), agents.list()]);
			existingNames = new Set(existing.map((workflow) => workflow.name));
			agentList = sessions;
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

	function chooseTemplate(value: 'blank' | 'code-review' | 'goal-loop') {
		template = value;
		workflowName = uniqueName(value === 'code-review' ? 'Code Review Loop' : value === 'goal-loop' ? 'Goal Loop' : 'New Workflow');
	}

	function blankYaml(name: string): string {
		return `name: ${JSON.stringify(name.toLowerCase().replace(/\s+/g, '-'))}
description: A reusable native-agent workflow.
version: 1

inputs:
  goal:
    type: string
    description: The outcome this workflow should produce.
    required: true
  worker:
    type: agent
    description: Agent context that performs the work.
    required: true
    primary: true

flows:
  main:
    color: "#22c55e"
    steps:
      - id: work
        type: step
        label: Do the work
        agent: "@worker"
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
description: One agent implements, another reviews, and the loop continues until approval.
version: 1

inputs:
  goal:
    type: string
    description: The code change to implement and review.
    required: true
  implementer:
    type: agent
    description: Agent context that owns the implementation and pull request.
    required: true
    primary: true
  reviewer:
    type: agent
    description: Independent agent context that reviews the implementation.
    required: true
  wait_for_human_review:
    type: boolean
    description: Keep the workflow alive until GitHub review activity arrives.
    default: true

flows:
  main:
    color: "#4A90D9"
    steps:
      - id: implement
        type: step
        label: Implement on a draft PR
        agent: "@implementer"
        prompt: |
          Implement this goal in the current repository:

          @goal

          Run the relevant checks, commit and push the branch, and create or
          update a DRAFT GitHub pull request. Do not mark it ready yet. This
          draft URL is the stable handoff that lets a reviewer in another
          XpressClaw context inspect the exact diff without sharing a folder.
        outputs:
          pull_request_url:
            type: string
            description: URL of the draft pull request.
          summary:
            type: string
            description: What changed and how it was verified.

      - id: review
        type: step
        label: Independent review
        agent: "@reviewer"
        new_session: true
        prompt: |
          Independently review the actual pull request at:

          @implement.pull_request_url

          The intended goal is:

          @goal

          Use the project-scoped GitHub tools to inspect the complete diff and
          checks; do not rely on a shared working tree. Approve only when it is
          correct, complete, maintainable, and tested. Return a verdict of
          exactly "approved" or "changes_requested".
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
            goto: step mark_ready
          - match: changes_requested
            continue: true
          - match: default
            goto: step review

      - id: revise
        type: step
        label: Address independent review
        agent: "@implementer"
        prompt: |
          Address this independent review of @implement.pull_request_url:

          @review.feedback

          Reinspect the current branch, implement the fixes, run relevant checks,
          and push the updated commits to the same draft pull request.
        outputs:
          summary:
            type: string
            description: What changed in response to review and how it was verified.

      - id: repeat_review
        type: jump
        label: Review the revision
        target: step review

      - id: mark_ready
        type: step
        label: Mark PR ready
        agent: "@implementer"
        prompt: |
          The independent review approved the implementation. Verify final checks,
          push the branch if needed, and mark @implement.pull_request_url ready
          for human review. Return that exact pull request URL.
        outputs:
          pull_request_url:
            type: string
            description: URL of the ready pull request.
          checks:
            type: string
            description: Final verification summary.

      - id: human_review_gate
        type: when
        label: Wait for human review?
        switch: "@wait_for_human_review"
        arms:
          - match: "true"
            continue: true
          - match: "false"
            goto: flow done

      - id: wait_for_review
        type: wait
        label: Wait for GitHub review activity
        agent: "@implementer"
        event: github.pull_request.activity
        resource: "@mark_ready.pull_request_url"
        timeout: 14d
        on_timeout: flow timed_out

      - id: handle_review
        type: step
        label: Triage and address human review
        agent: "@implementer"
        prompt: |
          GitHub activity arrived for @mark_ready.pull_request_url:

          @wait_for_review

          Inspect the complete current PR conversation, reviews, checks, and diff.
          If approval means no work remains, return outcome "approved". If actionable
          feedback exists, address it, run the relevant checks, push the updates to
          the same PR, and return "changes_addressed". If this event needs no code
          change and review is still pending, return "keep_waiting".
        outputs:
          outcome:
            type: string
            description: approved, changes_addressed, or keep_waiting.
          summary:
            type: string
            description: What the review said and what action was taken.

      - id: human_review_result
        type: when
        label: Human review result
        switch: "@handle_review.outcome"
        arms:
          - match: approved
            goto: flow done
          - match: changes_addressed
            goto: step review
          - match: keep_waiting
            goto: step wait_for_review
          - match: default
            goto: step wait_for_review

  done:
    color: "#22c55e"
    steps: []

  timed_out:
    color: "#f59e0b"
    steps: []
`;
	}

	function goalLoopYaml(name: string): string {
		return `name: ${JSON.stringify(name.toLowerCase().replace(/\s+/g, '-'))}
description: One agent pursues a goal in bounded iterations and stops when it reports completion.
version: 1

inputs:
  goal:
    type: string
    description: The outcome the agent should pursue.
    required: true
  worker:
    type: agent
    description: Agent context that pursues the goal.
    required: true
    primary: true

flows:
  main:
    color: "#8b5cf6"
    steps:
      - id: pursue_goal
        type: step
        label: Make progress
        agent: "@worker"
        prompt: |
          Work autonomously toward this goal in the current workspace:

          @goal

          Inspect the current state, make one meaningful verified increment, and
          decide whether the goal is complete. Return status as exactly "complete"
          or "continue", plus a progress summary and the next action if needed.
        outputs:
          status:
            type: string
            description: Either complete or continue.
          progress:
            type: string
            description: What changed and how it was verified.
          next_action:
            type: string
            description: The next useful increment when status is continue.

      - id: goal_gate
        type: when
        label: Goal complete?
        switch: "@pursue_goal.status"
        arms:
          - match: complete
            continue: true
          - match: continue
            goto: step pursue_goal
`;
	}

	async function createWorkflow() {
		if (!workflowName.trim() || creating) return;
		creating = true;
		error = '';
		try {
			const yaml = template === 'code-review'
				? codeReviewYaml(workflowName.trim())
				: template === 'goal-loop'
					? goalLoopYaml(workflowName.trim())
					: blankYaml(workflowName.trim());
			const description = template === 'code-review'
				? 'Implementation and independent review loop using durable agents.'
				: template === 'goal-loop'
					? 'A bounded loop that makes verified progress until its goal is complete.'
					: 'A reusable agent workflow.';
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
		<a href="/automations" class="text-xs text-muted-foreground hover:text-foreground">← Automations</a>
		<h1 class="mt-2 text-2xl font-bold">New workflow</h1>
		<p class="mt-1 text-sm text-muted-foreground">Start with a working multi-agent pattern or a single editable step.</p>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading agents…</div>
	{:else if agentList.length === 0}
		<div class="rounded-xl border border-dashed border-border bg-card p-8 text-center">
			<h2 class="text-base font-semibold">Workflows need at least one agent</h2>
			<p class="mt-2 text-sm text-muted-foreground">Create the durable agents that will perform each step.</p>
			<a href="/setup?mode=add-session" class="mt-4 inline-flex rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">Create agent</a>
		</div>
	{:else}
		<div class="grid gap-3 sm:grid-cols-3">
			<button onclick={() => chooseTemplate('code-review')} class="rounded-xl border p-4 text-left {template === 'code-review' ? 'border-primary bg-primary/5' : 'border-border bg-card hover:border-primary/40'}">
				<div class="text-sm font-semibold">Implementation + review loop</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">One agent writes the code, another reviews it, and rejected changes loop back until approval before the PR is marked ready.</p>
			</button>
			<button onclick={() => chooseTemplate('goal-loop')} class="rounded-xl border p-4 text-left {template === 'goal-loop' ? 'border-primary bg-primary/5' : 'border-border bg-card hover:border-primary/40'}">
				<div class="text-sm font-semibold">Goal loop</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">An agent makes verified increments until it reports completion. Execution is capped at 10 cycles.</p>
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
			<div class="rounded-lg border border-border/70 bg-background/50 p-3">
				<div class="text-xs font-medium">Run-time agent roles</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					{#if template === 'code-review'}
						Each run chooses an <strong>implementer</strong> and an independent <strong>reviewer</strong>. The definition is reusable; when started in a Project, both roles must be Agents in that Project. A draft PR is their handoff, so they do not need to share a folder. After it is marked ready, the workflow sleeps until a GitHub review or comment arrives.
					{:else}
						Each run chooses the <strong>worker</strong> Agent, so this definition can be reused in any Project.
					{/if}
				</p>
			</div>
			{#if error}<p class="text-xs text-destructive">{error}</p>{/if}
			<div class="flex justify-end gap-2">
				<a href="/automations" class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</a>
				<button onclick={createWorkflow} disabled={!workflowName.trim() || creating} class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{creating ? 'Creating…' : 'Create workflow'}</button>
			</div>
		</div>
	{/if}
</div>
