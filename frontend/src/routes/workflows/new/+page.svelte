<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { agents, workflows } from '$lib/api';
	import type { Agent } from '$lib/api';
	import {
		BLANK_WORKFLOW_TEMPLATE,
		WORKFLOW_TEMPLATES,
		renderWorkflowTemplate,
		uniqueWorkflowName,
		workflowTemplate,
		type WorkflowTemplateDefinition,
	} from '$lib/workflows/templates';

	type SelectedTemplateId = WorkflowTemplateDefinition['id'];

	let agentList = $state<Agent[]>([]);
	let existingNames = $state(new Set<string>());
	let selectedTemplateId = $state<SelectedTemplateId>('goal-loop');
	let workflowName = $state('Goal Loop');
	let scheduleAgentId = $state('');
	let scheduleCron = $state('');
	let loading = $state(true);
	let creating = $state(false);
	let error = $state('');
	let selectedTemplate = $derived(workflowTemplate(selectedTemplateId));
	let canCreate = $derived(Boolean(
		workflowName.trim()
		&& !creating
		&& (!selectedTemplate.schedule || (scheduleAgentId && scheduleCron.trim())),
	));

	onMount(async () => {
		try {
			const [existing, sessions] = await Promise.all([workflows.list(), agents.list()]);
			existingNames = new Set(existing.map((workflow) => workflow.name));
			agentList = sessions;
			scheduleAgentId = sessions[0]?.id ?? '';
			workflowName = uniqueWorkflowName(selectedTemplate.defaultName, existingNames);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loading = false;
		}
	});

	function chooseTemplate(id: SelectedTemplateId) {
		const definition = workflowTemplate(id);
		selectedTemplateId = id;
		workflowName = uniqueWorkflowName(definition.defaultName, existingNames);
		scheduleCron = definition.schedule?.defaultCron ?? '';
		error = '';
	}

	function agentLabel(agent: Agent): string {
		return agent.title || agent.name;
	}

	async function createWorkflow() {
		if (!canCreate) return;
		creating = true;
		error = '';
		try {
			const name = workflowName.trim();
			const yaml = renderWorkflowTemplate(selectedTemplate, name, {
				scheduleAgentId,
				scheduleCron,
			});
			const workflow = await workflows.create({
				name,
				description: selectedTemplate.apiDescription,
				yaml_content: yaml,
			});
			goto(`/workflows/${workflow.id}`);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			creating = false;
		}
	}
</script>

<div class="mx-auto w-full max-w-5xl space-y-5 p-4 sm:p-6">
	<div class="flex flex-wrap items-end justify-between gap-3">
		<div>
			<a href="/automations" class="text-xs text-muted-foreground hover:text-foreground">← Automations</a>
			<h1 class="mt-2 text-2xl font-bold">New workflow</h1>
			<p class="mt-1 text-sm text-muted-foreground">Choose a practical multi-agent pattern, then tailor it in the visual editor.</p>
		</div>
		<button
			type="button"
			data-start-blank
			aria-pressed={selectedTemplateId === 'blank'}
			onclick={() => chooseTemplate('blank')}
			class="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors {selectedTemplateId === 'blank' ? 'border-primary bg-primary/10 text-foreground' : 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground'}"
		>
			+ Start blank
		</button>
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
		<div data-workflow-template-gallery class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
			{#each WORKFLOW_TEMPLATES as candidate}
				<button
					type="button"
					data-workflow-template-card={candidate.id}
					aria-pressed={selectedTemplateId === candidate.id}
					onclick={() => chooseTemplate(candidate.id)}
					class="min-h-28 rounded-xl border p-3 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring {selectedTemplateId === candidate.id ? 'border-primary bg-primary/10 shadow-sm' : 'border-border bg-card hover:border-primary/40 hover:bg-accent/30'}"
				>
					<div class="flex items-start justify-between gap-2">
						<div class="text-sm font-semibold leading-snug">{candidate.title}</div>
						{#if selectedTemplateId === candidate.id}
							<span class="shrink-0 rounded-full bg-primary px-2 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-primary-foreground">Selected</span>
						{:else if candidate.schedule}
							<span class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[9px] font-medium uppercase tracking-wide text-muted-foreground">Scheduled</span>
						{/if}
					</div>
					<p class="mt-1.5 text-xs leading-relaxed text-muted-foreground">{candidate.description}</p>
				</button>
			{/each}
		</div>

		<div class="space-y-4 rounded-xl border border-border bg-card p-4 sm:p-5">
			<div class="flex flex-wrap items-start justify-between gap-2">
				<div>
					<div class="text-sm font-semibold">{selectedTemplate.title}</div>
					<p class="mt-0.5 text-xs text-muted-foreground">{selectedTemplate.description}</p>
				</div>
				{#if selectedTemplateId === 'blank'}
					<span class="rounded-full border border-border px-2 py-0.5 text-[10px] text-muted-foreground">One-step starter</span>
				{/if}
			</div>

			<div>
				<label for="workflow-name" class="mb-1 block text-xs font-medium text-muted-foreground">Name</label>
				<input id="workflow-name" bind:value={workflowName} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring" />
			</div>

			{#if selectedTemplate.schedule}
				<div data-template-schedule class="grid gap-3 rounded-lg border border-sky-500/25 bg-sky-500/5 p-3 sm:grid-cols-2">
					<div class="sm:col-span-2">
						<div class="text-xs font-medium">Automatic schedule</div>
						<p class="mt-1 text-xs leading-relaxed text-muted-foreground">{selectedTemplate.schedule.description} The workflow is created enabled; you can change or pause the schedule from its Inputs &amp; trigger block.</p>
					</div>
					<label for="schedule-cron" class="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
						Cron · server-local time
						<input id="schedule-cron" aria-label="Workflow schedule" bind:value={scheduleCron} class="mt-1 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs normal-case tracking-normal text-foreground outline-none focus:ring-1 focus:ring-ring" />
					</label>
					<label for="schedule-agent" class="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
						Agent for automatic runs
						<select id="schedule-agent" aria-label="Scheduled agent" bind:value={scheduleAgentId} class="mt-1 w-full rounded-md border border-input bg-background px-3 py-2 text-xs normal-case tracking-normal text-foreground outline-none focus:ring-1 focus:ring-ring">
							{#each agentList as agent}<option value={agent.id}>{agentLabel(agent)}</option>{/each}
						</select>
					</label>
				</div>
			{/if}

			<div class="rounded-lg border border-border/70 bg-background/50 p-3">
				<div class="text-xs font-medium">Run-time Agent roles</div>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">{selectedTemplate.roleGuidance}</p>
			</div>

			{#if error}<p class="text-xs text-destructive">{error}</p>{/if}
			<div class="flex justify-end gap-2">
				<a href="/automations" class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</a>
				<button onclick={createWorkflow} disabled={!canCreate} class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
					{creating ? 'Creating…' : selectedTemplate.schedule ? 'Create scheduled workflow' : 'Create workflow'}
				</button>
			</div>
		</div>
	{/if}
</div>
