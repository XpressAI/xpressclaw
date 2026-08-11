<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import yaml from 'js-yaml';
	import { setup, sessions, agents as agentsApi, conversations as conversationsApi, projects as projectsApi, workflows as workflowsApi } from '$lib/api';
	import type { Agent, Conversation, ImageAttachmentUpload, Project, Task, Workflow } from '$lib/api';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';
	import { clearComposerDraft, loadComposerDraft, loadComposerTarget, saveComposerDraft, saveComposerTarget } from '$lib/composerDrafts';
	import { appendImageFiles, imageDataUrl, IMAGE_FILE_ACCEPT, MAX_IMAGE_ATTACHMENTS, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';
	import { harnessMark } from '$lib/utils';

	const messageDraftScope = 'new-work';
	let status_text = $state('Connecting to server...');
	let loading = $state(true);
	let retries = 0;

	let message = $state('');
	let messageDraftReady = $state(false);
	let agentList = $state<Agent[]>([]);
	let projectList = $state<Project[]>([]);
	let conversationList = $state<Conversation[]>([]);
	let selectedProject = $state('');
	let selectedProjectReady = $state(false);
	let selectedConversation = $state('');
	let selectedConversationReady = $state(false);
	let selectedAgent = $state('');
	let selectedAgentReady = $state(false);
	let workflowList = $state<Workflow[]>([]);
	let selectedWorkflow = $state('');
	let selectedWorkflowReady = $state(false);
	let roleAgents = $state<Record<string, string>>({});
	let sending = $state(false);
	let composing = $state(false);
	let sendError = $state('');
	let startFresh = $state(false);
	let imageAttachments = $state<ImageAttachmentUpload[]>([]);
	let imageInput = $state<HTMLInputElement>();
	let imagePreviews = $derived(imageAttachments.map((attachment) => ({
		name: attachment.name,
		src: imageDataUrl(attachment),
	})));

	async function checkReady() {
		try {
			status_text = 'Checking setup...';
			const status = await setup.status();
			if (!status.setup_complete) {
				goto('/setup', { replaceState: true });
				return;
			}

			const [agts, projects, conversationRecords, workflowRecords] = await Promise.all([
				agentsApi.list().catch(() => []),
				projectsApi.list().catch(() => []),
				conversationsApi.list(undefined, 200).catch(() => []),
				workflowsApi.list().catch(() => []),
			]);
			agentList = agts;
			projectList = projects;
			conversationList = conversationRecords;
			workflowList = workflowRecords;
			const savedAgent = loadComposerTarget(messageDraftScope);
			const savedProject = loadComposerTarget(`${messageDraftScope}-project`);
			selectedProject = projects.find((project) => project.id === savedProject)?.id
				?? agts.find((agent) => agent.id === savedAgent)?.project_id
				?? projects[0]?.id
				?? '';
			selectedProjectReady = true;
			selectedAgent = agts.find((agent) => agent.id === savedAgent && (!selectedProject || agent.project_id === selectedProject))?.id
				?? agts.find((agent) => !selectedProject || agent.project_id === selectedProject)?.id
				?? '';
			selectedAgentReady = true;
			const savedConversation = loadComposerTarget(`${messageDraftScope}-conversation`);
			selectedConversation = conversationRecords.find((conversation) => conversation.id === savedConversation && conversation.project_id === selectedProject)?.id ?? '';
			selectedConversationReady = true;
			const savedWorkflow = loadComposerTarget(`${messageDraftScope}-workflow`);
			selectedWorkflow = workflowRecords.some((workflow) => workflow.id === savedWorkflow) ? savedWorkflow : '';
			selectedWorkflowReady = true;
			loading = false;
		} catch {
			retries++;
			if (retries < 60) {
				status_text = 'Waiting for server...';
				setTimeout(checkReady, 500);
			} else {
				loading = false;
			}
		}
	}

	$effect(() => {
		if (messageDraftReady) saveComposerDraft(messageDraftScope, message);
	});

	$effect(() => {
		if (selectedAgentReady) saveComposerTarget(messageDraftScope, selectedAgent);
	});

	$effect(() => {
		if (selectedProjectReady) saveComposerTarget(`${messageDraftScope}-project`, selectedProject);
	});

	$effect(() => {
		if (selectedConversationReady) saveComposerTarget(`${messageDraftScope}-conversation`, selectedConversation);
	});

	$effect(() => {
		if (selectedWorkflowReady) saveComposerTarget(`${messageDraftScope}-workflow`, selectedWorkflow);
	});

	onMount(() => {
		message = loadComposerDraft(messageDraftScope);
		messageDraftReady = true;
		void checkReady();
	});

	function greeting(): string {
		const hour = new Date().getHours();
		if (hour < 12) return 'Good morning';
		if (hour < 18) return 'Good afternoon';
		return 'Good evening';
	}

	interface WorkflowInputSummary {
		type?: string;
		required?: boolean;
		default?: unknown;
		primary?: boolean;
		description?: string;
	}

	interface WorkflowStepSummary {
		type?: string;
		agent?: string;
		steps?: WorkflowStepSummary[];
		body?: WorkflowStepSummary[];
	}

	interface WorkflowDefinitionSummary {
		trigger?: unknown;
		inputs?: Record<string, WorkflowInputSummary>;
		flows?: Record<string, { steps?: WorkflowStepSummary[] }>;
	}

	function workflowDefinition(workflow: Workflow | undefined): WorkflowDefinitionSummary | null {
		if (!workflow) return null;
		try {
			return yaml.load(workflow.yaml_content) as WorkflowDefinitionSummary;
		} catch {
			return null;
		}
	}

	function firstStepAgent(steps: WorkflowStepSummary[]): string | null {
		for (const step of steps) {
			if ((step.type ?? 'step') === 'step' && step.agent) return step.agent;
			const nested = firstStepAgent(step.steps ?? step.body ?? []);
			if (nested) return nested;
		}
		return null;
	}

	function usesConnectorSink(steps: WorkflowStepSummary[]): boolean {
		return steps.some((step) => step.type === 'sink' || usesConnectorSink(step.steps ?? step.body ?? []));
	}

	function supportsNewWork(workflow: Workflow, agentId: string): boolean {
		const definition = workflowDefinition(workflow);
		if (!definition || definition.trigger) return false;
		const mainSteps = definition.flows?.main?.steps ?? [];
		if (Object.values(definition.flows ?? {}).some((flow) => usesConnectorSink(flow.steps ?? []))) return false;
		const inputs = definition.inputs ?? {};
		if ((inputs.goal?.type ?? 'string') !== 'string' || !inputs.goal) return false;
		const agentInputs = Object.entries(inputs).filter(([, input]) => input.type === 'agent');
		if (agentInputs.length === 0 && firstStepAgent(mainSteps) !== agentId) return false;
		return Object.entries(inputs).every(([name, input]) =>
			name === 'goal' || input.type === 'agent' || !input.required || input.default !== undefined && input.default !== null
		);
	}

	let projectAgents = $derived(selectedProject ? agentList.filter((agent) => agent.project_id === selectedProject) : agentList);
	let projectConversations = $derived(conversationList.filter((conversation) => conversation.project_id === selectedProject));
	let selectedAgentObj = $derived(projectAgents.find(a => a.id === selectedAgent));
	let compatibleWorkflows = $derived(workflowList.filter((workflow) => supportsNewWork(workflow, selectedAgent)));
	let selectedWorkflowObj = $derived(compatibleWorkflows.find((workflow) => workflow.id === selectedWorkflow));
	let selectedWorkflowInputs = $derived(workflowDefinition(selectedWorkflowObj)?.inputs ?? {});
	let workflowAgentInputs = $derived(Object.entries(selectedWorkflowInputs).filter(([, input]) => input.type === 'agent'));
	let primaryAgentRole = $derived(workflowAgentInputs.find(([, input]) => input.primary)?.[0] ?? workflowAgentInputs[0]?.[0] ?? null);
	let secondaryAgentRoles = $derived(workflowAgentInputs.filter(([name]) => name !== primaryAgentRole));
	let canSend = $derived(Boolean(selectedAgent) && !sending && (
		selectedWorkflow
			? Boolean(message.trim()) && imageAttachments.length === 0 && Boolean(selectedWorkflowObj)
				&& secondaryAgentRoles.every(([name, input]) => !input.required || Boolean(roleAgents[name] || input.default))
			: (Boolean(message.trim()) || imageAttachments.length > 0) && (!selectedConversation || imageAttachments.length === 0)
	));

	$effect(() => {
		if (!selectedProjectReady) return;
		if (!projectAgents.some((agent) => agent.id === selectedAgent)) selectedAgent = projectAgents[0]?.id ?? '';
		if (!projectConversations.some((conversation) => conversation.id === selectedConversation)) selectedConversation = '';
	});

	$effect(() => {
		if (selectedWorkflowReady && selectedWorkflow && !compatibleWorkflows.some((workflow) => workflow.id === selectedWorkflow)) {
			selectedWorkflow = '';
		}
	});

	$effect(() => {
		if (!selectedWorkflowObj) {
			roleAgents = {};
			return;
		}
		const next: Record<string, string> = {};
		for (const [name, input] of secondaryAgentRoles) {
			const configured = typeof input.default === 'string' ? input.default : '';
			const saved = loadComposerTarget(`${messageDraftScope}-workflow-${selectedWorkflowObj.id}-role-${name}`);
			const alternative = projectAgents.find((agent) => agent.id !== selectedAgent)?.id ?? selectedAgent;
			next[name] = projectAgents.some((agent) => agent.id === roleAgents[name])
				? roleAgents[name]
				: projectAgents.some((agent) => agent.id === saved) ? saved
					: projectAgents.some((agent) => agent.id === configured) ? configured : alternative;
		}
		if (JSON.stringify(next) !== JSON.stringify(roleAgents)) roleAgents = next;
	});

	async function send() {
		if (!canSend) return;
		sending = true;
		sendError = '';

		try {
			if (selectedWorkflowObj) {
				const inputs: Record<string, unknown> = { goal: message.trim() };
				if (primaryAgentRole) inputs[primaryAgentRole] = selectedAgent;
				for (const [name, input] of secondaryAgentRoles) {
					const value = roleAgents[name] || input.default;
					if (value) inputs[name] = value;
				}
				const instance = selectedConversation
					? await startConversationWorkflow(selectedWorkflowObj, inputs)
					: await workflowsApi.run(selectedWorkflowObj.id, inputs, selectedProject || undefined);
				message = '';
				clearComposerDraft(messageDraftScope);
				imageAttachments = [];
				await goto(instance.current_task_id ? `/tasks/${instance.current_task_id}` : `/workflows/${selectedWorkflowObj.id}`);
				return;
			}
			if (selectedConversation) {
				const task = await conversationsApi.createTask(selectedConversation, {
					title: taskTitle(message),
					description: message.trim(),
					agent_id: selectedAgent,
				}) as Task;
				message = '';
				clearComposerDraft(messageDraftScope);
				imageAttachments = [];
				await goto(`/tasks/${task.id}`);
				return;
			}
			const queued = await sessions.sendMessage(selectedAgent, message.trim(), {
				newSession: startFresh,
				attachments: imageAttachments,
			});
			message = '';
			clearComposerDraft(messageDraftScope);
			imageAttachments = [];
			await goto(`/tasks/${queued.task.id}`);
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
		} finally {
			sending = false;
		}
	}

	function taskTitle(content: string): string {
		const firstLine = content.trim().split(/\r?\n/, 1)[0] || 'Conversation task';
		return firstLine.length > 120 ? `${firstLine.slice(0, 117)}…` : firstLine;
	}

	async function startConversationWorkflow(workflow: Workflow, inputs: Record<string, unknown>) {
		const result = await conversationsApi.createTask(selectedConversation, {
			title: taskTitle(message),
			workflow_id: workflow.id,
			workflow_inputs: inputs,
		});
		if (!('workflow_instance_id' in result)) throw new Error('The conversation workflow did not start');
		return workflowsApi.getInstance(result.workflow_instance_id);
	}

	async function addImages(files: File[]) {
		try {
			imageAttachments = await appendImageFiles(imageAttachments, files);
			sendError = '';
		} catch (e) {
			sendError = e instanceof Error ? e.message : String(e);
		}
	}

	function handleImageInput(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		void addImages(Array.from(input.files ?? [])).finally(() => (input.value = ''));
	}

	function handlePaste(event: ClipboardEvent) {
		if (!shouldHandleImagePaste(event)) return;
		event.preventDefault();
		void pastedImageFiles(event)
			.then(addImages)
			.catch((e) => (sendError = e instanceof Error ? e.message : String(e)));
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !composing && e.keyCode !== 229) {
			e.preventDefault();
			send();
		}
	}

	function statusDot(status: string): string {
		if (status === 'running') return 'bg-blue-500 animate-pulse';
		if (status === 'queued') return 'bg-amber-400';
		if (status === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		return 'bg-emerald-500';
	}
</script>

{#if loading}
	<div class="flex h-full flex-col items-center justify-center gap-3">
		<div class="h-8 w-8 animate-spin rounded-full border-2 border-muted-foreground border-t-primary"></div>
		<span class="text-sm text-muted-foreground">{status_text}</span>
	</div>
{:else}
	<div class="flex min-h-full flex-col items-center justify-center px-4 py-8 sm:px-6">
		<div class="w-full max-w-2xl space-y-6 sm:space-y-8">
			<!-- Greeting -->
			<div class="text-center">
				<h1 class="text-2xl font-semibold text-foreground sm:text-3xl">{greeting()}</h1>
				<p class="mt-2 text-sm text-muted-foreground">Send work directly to an agent or run it through a workflow.</p>
			</div>

			{#if agentList.length === 0}
				<div class="rounded-2xl border border-dashed border-border bg-card p-8 text-center">
					<h2 class="text-base font-semibold">Create an agent first</h2>
					<p class="mx-auto mt-2 max-w-md text-sm text-muted-foreground">Give a durable context a workspace and a Codex, Claude, OpenCode, or custom harness.</p>
					<a href="/setup?mode=add-session" class="mt-5 inline-flex rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Create agent</a>
				</div>
			{:else}
			<!-- Queue work directly into the selected logical session. -->
			<div class="rounded-2xl border border-border bg-card shadow-lg shadow-black/10 focus-within:border-primary/40">
				<textarea
					bind:value={message}
					onkeydown={handleKeydown}
					oncompositionstart={() => (composing = true)}
					oncompositionend={() => setTimeout(() => (composing = false), 0)}
					onpaste={handlePaste}
					placeholder="Describe the outcome you want…"
					rows="3"
					disabled={sending}
					class="w-full resize-none rounded-t-2xl bg-transparent px-5 pt-5 pb-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
				></textarea>
				<ImageAttachmentPreviews attachments={imagePreviews} onremove={(index) => (imageAttachments = imageAttachments.filter((_, itemIndex) => itemIndex !== index))} />
				{#if selectedWorkflowObj && secondaryAgentRoles.length > 0}
					<div class="mx-4 mb-3 grid gap-2 rounded-lg border border-border/70 bg-background/40 p-3 sm:grid-cols-2">
						{#each secondaryAgentRoles as [name, input]}
							<label class="text-[10px] font-medium text-muted-foreground">
								{name.replaceAll('_', ' ').toUpperCase()}{input.required ? ' · REQUIRED' : ''}
								<select aria-label="Agent role {name}" value={roleAgents[name] ?? ''} onchange={(event) => {
									roleAgents = { ...roleAgents, [name]: event.currentTarget.value };
									saveComposerTarget(`${messageDraftScope}-workflow-${selectedWorkflowObj.id}-role-${name}`, event.currentTarget.value);
								}} class="mt-1 w-full rounded border border-input bg-secondary px-2.5 py-1.5 text-xs text-foreground">
									<option value="">Select agent…</option>
									{#each projectAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}
								</select>
								{#if input.description}<span class="mt-1 block font-normal normal-case leading-relaxed">{input.description}</span>{/if}
							</label>
						{/each}
					</div>
				{/if}
				<div class="flex flex-col gap-3 px-4 pb-4 sm:flex-row sm:items-center sm:justify-between">
					{#if selectedWorkflowObj}
						<p class="text-xs text-muted-foreground">Bind each workflow role for this run. The definition is reusable across projects.</p>
					{:else if !selectedConversation}
						<label class="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground" title="Do not inherit context from this agent's current conversation">
							<input type="checkbox" bind:checked={startFresh} class="h-3.5 w-3.5 rounded border-border accent-primary" />
							Start a fresh conversation
						</label>
					{:else}
						<p class="text-xs text-muted-foreground">This task will stay linked to the selected conversation.</p>
					{/if}
					<div class="flex min-w-0 flex-wrap items-center justify-end gap-2 sm:gap-3">
						<input bind:this={imageInput} type="file" accept={IMAGE_FILE_ACCEPT} multiple onchange={handleImageInput} class="hidden" />
						<button type="button" onclick={() => imageInput?.click()} disabled={sending || Boolean(selectedWorkflow) || Boolean(selectedConversation) || imageAttachments.length >= MAX_IMAGE_ATTACHMENTS} aria-label="Attach images" title={selectedConversation ? 'Attach files in the conversation before creating linked work' : selectedWorkflow ? 'Workflow runs currently accept text input only' : 'Attach images (you can also paste)'}
							class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-30">
							<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>
						</button>
						<div class="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
							<span class="flex h-5 w-5 items-center justify-center rounded bg-muted text-[10px] font-semibold">P</span>
							<select bind:value={selectedProject} aria-label="Project" class="min-w-0 max-w-28 cursor-pointer bg-transparent text-xs text-foreground focus:outline-none sm:max-w-40">
								{#each projectList as project}<option value={project.id}>{project.name}</option>{/each}
							</select>
						</div>
						<div class="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
							<span class="flex h-5 w-5 items-center justify-center rounded bg-muted text-[10px] font-semibold">#</span>
							<select bind:value={selectedConversation} aria-label="Conversation" class="min-w-0 max-w-32 cursor-pointer bg-transparent text-xs text-foreground focus:outline-none sm:max-w-44">
								<option value="">Private task</option>
								{#each projectConversations as conversation}<option value={conversation.id}>{conversation.title || 'Untitled conversation'}</option>{/each}
							</select>
						</div>
						<div class="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
								{#if selectedAgentObj}
									<span class="flex h-5 w-5 items-center justify-center rounded bg-muted text-[10px] font-semibold">{harnessMark(selectedAgentObj.backend)}</span>
									<span class="h-2 w-2 rounded-full {statusDot(selectedAgentObj.status)}"></span>
								{/if}
								<select
									bind:value={selectedAgent}
									aria-label={primaryAgentRole ? `Agent role ${primaryAgentRole}` : 'Agent'}
									class="min-w-0 max-w-28 cursor-pointer bg-transparent text-xs text-foreground focus:outline-none sm:max-w-40"
								>
									{#each projectAgents as agent}
										<option value={agent.id}>
											{agent.title || agent.name}
										</option>
									{/each}
								</select>
								{#if primaryAgentRole}<span class="hidden text-[9px] text-muted-foreground sm:inline">as {primaryAgentRole}</span>{/if}
						</div>
						<div class="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-secondary px-2.5 py-1.5">
							<span class="flex h-5 w-5 items-center justify-center rounded bg-muted text-[10px] font-semibold">W</span>
							<select bind:value={selectedWorkflow} aria-label="Workflow" class="min-w-0 max-w-28 cursor-pointer bg-transparent text-xs text-foreground focus:outline-none sm:max-w-44">
								<option value="">No workflow</option>
								{#each compatibleWorkflows as workflow}
									<option value={workflow.id}>{workflow.name}</option>
								{/each}
							</select>
						</div>
						<button
							onclick={send}
							disabled={!canSend}
							class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors shadow-lg shadow-primary/20"
						>
							{#if sending}
								<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary-foreground border-t-transparent"></span>
							{:else}
								<svg class="h-4 w-4" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
							{/if}
						</button>
					</div>
				</div>
			</div>
			{#if selectedWorkflow && imageAttachments.length > 0}
				<p class="text-center text-xs text-amber-600">Workflow runs currently accept text input only. Remove the attachments or choose No workflow.</p>
			{:else if selectedConversation && imageAttachments.length > 0}
				<p class="text-center text-xs text-amber-600">Put files in the conversation before creating linked work, or choose Private task.</p>
			{/if}
			{#if sendError}<p class="text-center text-sm text-destructive">{sendError}</p>{/if}
			<div class="text-center"><a href="/projects" class="text-xs text-muted-foreground hover:text-foreground hover:underline">Manage projects, conversations, and Agents</a></div>
			{/if}
		</div>
	</div>
{/if}
