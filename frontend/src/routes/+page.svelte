<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import yaml from 'js-yaml';
	import { setup, sessions, agents as agentsApi, conversations as conversationsApi, projects as projectsApi, workflows as workflowsApi } from '$lib/api';
	import type { Agent, Conversation, ImageAttachmentUpload, Project, Task, Workflow } from '$lib/api';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';
	import { clearComposerDraft, loadComposerDraft, loadComposerTarget, saveComposerDraft, saveComposerTarget } from '$lib/composerDrafts';
	import { appendImageFiles, imageDataUrl, IMAGE_FILE_ACCEPT, MAX_IMAGE_ATTACHMENTS, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';
	import AgentLoading from '$lib/components/AgentLoading.svelte';
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
	let composerMode = $state<'agent' | 'workflow'>('agent');
	let composerModeReady = $state(false);
	let roleAgents = $state<Record<string, string>>({});
	let workflowInputValues = $state<Record<string, string>>({});
	let workflowValuesFor = $state('');
	let workflowInputError = $state('');
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
			composerMode = loadComposerTarget(`${messageDraftScope}-mode`) === 'workflow' ? 'workflow' : 'agent';
			composerModeReady = true;
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

	$effect(() => {
		if (composerModeReady) saveComposerTarget(`${messageDraftScope}-mode`, composerMode);
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
		type?: 'string' | 'number' | 'boolean' | 'json' | 'agent';
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

	function usesConnectorSink(steps: WorkflowStepSummary[]): boolean {
		return steps.some((step) => step.type === 'sink' || usesConnectorSink(step.steps ?? step.body ?? []));
	}

	function fixedStepAgents(steps: WorkflowStepSummary[]): string[] {
		return steps.flatMap((step) => {
			const configured = step.agent?.trim();
			const current = configured && !configured.startsWith('@') ? [configured] : [];
			return [...current, ...fixedStepAgents(step.steps ?? step.body ?? [])];
		});
	}

	function supportsNewWork(workflow: Workflow): boolean {
		const definition = workflowDefinition(workflow);
		if (!definition || definition.trigger) return false;
		if (Object.values(definition.flows ?? {}).some((flow) => usesConnectorSink(flow.steps ?? []))) return false;
		const projectAgentIds = new Set(projectAgents.map((agent) => agent.id));
		return Object.values(definition.flows ?? {}).every((flow) =>
			fixedStepAgents(flow.steps ?? []).every((agentId) => projectAgentIds.has(agentId))
		);
	}

	let projectAgents = $derived(selectedProject ? agentList.filter((agent) => agent.project_id === selectedProject) : agentList);
	let projectConversations = $derived(conversationList.filter((conversation) => conversation.project_id === selectedProject));
	let selectedAgentObj = $derived(projectAgents.find(a => a.id === selectedAgent));
	let compatibleWorkflows = $derived(workflowList.filter(supportsNewWork));
	let selectedWorkflowObj = $derived(compatibleWorkflows.find((workflow) => workflow.id === selectedWorkflow));
	let selectedWorkflowInputs = $derived(workflowDefinition(selectedWorkflowObj)?.inputs ?? {});
	let workflowInputEntries = $derived(Object.entries(selectedWorkflowInputs));
	let workflowGoalInput = $derived(
		selectedWorkflowInputs.goal && (selectedWorkflowInputs.goal.type ?? 'string') === 'string'
			? selectedWorkflowInputs.goal
			: null
	);
	let additionalWorkflowInputs = $derived(workflowInputEntries.filter(([name]) => name !== 'goal' || !workflowGoalInput));
	let workflowAgentInputs = $derived(Object.entries(selectedWorkflowInputs).filter(([, input]) => input.type === 'agent'));
	let primaryAgentRole = $derived(workflowAgentInputs.find(([, input]) => input.primary)?.[0] ?? workflowAgentInputs[0]?.[0] ?? null);
	let secondaryAgentRoles = $derived(workflowAgentInputs.filter(([name]) => name !== primaryAgentRole));
	let workflowReady = $derived(Boolean(selectedWorkflowObj) && workflowInputEntries.every(([name, input]) =>
		!input.required || (input.type !== 'agent' && input.default !== undefined) || Boolean(workflowInputValue(name, input).trim())
	));
	let canSend = $derived(!sending && (composerMode === 'workflow'
		? workflowReady && imageAttachments.length === 0
		: Boolean(selectedAgent) && (Boolean(message.trim()) || imageAttachments.length > 0)
			&& (!selectedConversation || imageAttachments.length === 0)
	));

	$effect(() => {
		if (!selectedProjectReady) return;
		if (!projectAgents.some((agent) => agent.id === selectedAgent)) selectedAgent = projectAgents[0]?.id ?? '';
		if (!projectConversations.some((conversation) => conversation.id === selectedConversation)) selectedConversation = '';
	});

	$effect(() => {
		if (!selectedWorkflowReady) return;
		if (selectedWorkflow && !compatibleWorkflows.some((workflow) => workflow.id === selectedWorkflow)) selectedWorkflow = '';
		if (composerMode === 'workflow' && !selectedWorkflow) selectedWorkflow = compatibleWorkflows[0]?.id ?? '';
	});

	$effect(() => {
		if (!selectedWorkflowObj) {
			roleAgents = {};
			workflowInputValues = {};
			workflowValuesFor = '';
			return;
		}
		if (workflowValuesFor !== selectedWorkflowObj.id) {
			const nextValues: Record<string, string> = {};
			for (const [name, input] of workflowInputEntries) {
				if ((name === 'goal' && workflowGoalInput) || input.type === 'agent') continue;
				const saved = loadComposerDraft(workflowInputDraftScope(selectedWorkflowObj.id, name));
				nextValues[name] = saved || displayWorkflowValue(input.default, input.type);
			}
			workflowInputValues = nextValues;
			workflowValuesFor = selectedWorkflowObj.id;
			workflowInputError = '';
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

	function setComposerMode(mode: 'agent' | 'workflow') {
		composerMode = mode;
		workflowInputError = '';
		if (mode === 'workflow' && !selectedWorkflow) selectedWorkflow = compatibleWorkflows[0]?.id ?? '';
	}

	function selectWorkflow(workflowId: string) {
		selectedWorkflow = workflowId;
		workflowValuesFor = '';
		roleAgents = {};
		workflowInputError = '';
	}

	function workflowInputDraftScope(workflowId: string, name: string): string {
		return `${messageDraftScope}-workflow-${workflowId}-input-${name}`;
	}

	function displayWorkflowValue(value: unknown, type: WorkflowInputSummary['type']): string {
		if (value === undefined || value === null) return '';
		if (type === 'json') {
			try {
				return JSON.stringify(value, null, 2);
			} catch {
				return '';
			}
		}
		return String(value);
	}

	function workflowInputLabel(name: string): string {
		const label = name.replaceAll('_', ' ');
		return `${label.slice(0, 1).toUpperCase()}${label.slice(1)}`;
	}

	function workflowInputValue(name: string, input: WorkflowInputSummary): string {
		if (name === 'goal' && (input.type ?? 'string') === 'string') return message;
		if (input.type === 'agent') return name === primaryAgentRole ? selectedAgent : roleAgents[name] ?? '';
		return workflowInputValues[name] ?? '';
	}

	function setWorkflowInputValue(name: string, input: WorkflowInputSummary, value: string) {
		workflowInputError = '';
		if (name === 'goal' && (input.type ?? 'string') === 'string') {
			message = value;
			return;
		}
		if (input.type === 'agent') {
			if (name === primaryAgentRole) selectedAgent = value;
			else {
				roleAgents = { ...roleAgents, [name]: value };
				if (selectedWorkflowObj) saveComposerTarget(`${messageDraftScope}-workflow-${selectedWorkflowObj.id}-role-${name}`, value);
			}
			return;
		}
		workflowInputValues = { ...workflowInputValues, [name]: value };
		if (selectedWorkflowObj) saveComposerDraft(workflowInputDraftScope(selectedWorkflowObj.id, name), value);
	}

	function typedWorkflowInputs(): Record<string, unknown> | null {
		const values: Record<string, unknown> = {};
		try {
			for (const [name, input] of workflowInputEntries) {
				const value = workflowInputValue(name, input);
				const raw = value.trim();
				if (!raw) {
					if (input.required && input.default === undefined) throw new Error(`${workflowInputLabel(name)} is required`);
					continue;
				}
				if (input.type === 'number') {
					const number = Number(raw);
					if (!Number.isFinite(number)) throw new Error(`${workflowInputLabel(name)} must be a number`);
					values[name] = number;
				} else if (input.type === 'boolean') {
					if (raw !== 'true' && raw !== 'false') throw new Error(`${workflowInputLabel(name)} must be yes or no`);
					values[name] = raw === 'true';
				} else if (input.type === 'json') {
					values[name] = JSON.parse(value);
				} else {
					values[name] = value;
				}
			}
			workflowInputError = '';
			return values;
		} catch (error) {
			workflowInputError = error instanceof Error ? error.message : String(error);
			return null;
		}
	}

	function clearWorkflowInputDrafts(workflow: Workflow) {
		for (const name of Object.keys(workflowDefinition(workflow)?.inputs ?? {})) {
			if (name !== 'goal') clearComposerDraft(workflowInputDraftScope(workflow.id, name));
		}
	}

	async function send() {
		if (!canSend) return;
		const workflowInputs = composerMode === 'workflow' ? typedWorkflowInputs() : null;
		if (composerMode === 'workflow' && !workflowInputs) return;
		sending = true;
		sendError = '';

		try {
			if (composerMode === 'workflow' && selectedWorkflowObj && workflowInputs) {
				const instance = selectedConversation
					? await startConversationWorkflow(selectedWorkflowObj, workflowInputs)
					: await workflowsApi.run(selectedWorkflowObj.id, workflowInputs, selectedProject || undefined);
				message = '';
				clearComposerDraft(messageDraftScope);
				clearWorkflowInputDrafts(selectedWorkflowObj);
				workflowValuesFor = '';
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
		const goal = typeof inputs.goal === 'string' ? inputs.goal : '';
		const result = await conversationsApi.createTask(selectedConversation, {
			title: goal ? taskTitle(goal) : `${workflow.name} run`,
			workflow_id: workflow.id,
			workflow_inputs: inputs,
		});
		if (!('workflow_instance_id' in result)) throw new Error('The conversation workflow did not start');
		const details = await workflowsApi.getInstance(result.workflow_instance_id);
		const currentTaskId = [...details.step_executions]
			.reverse()
			.find((execution) => execution.task_id)?.task_id ?? null;
		return { ...details.instance, current_task_id: currentTaskId };
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
	<div class="flex h-full items-center justify-center"><AgentLoading label={status_text} /></div>
{:else}
	<div data-new-work-scroll class="workspace-scroll-y flex h-full flex-col items-center px-4 sm:px-6">
		<div class="my-auto w-full max-w-2xl shrink-0 space-y-6 py-8 sm:space-y-8">
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
				<div class="space-y-4">
					<div class="flex justify-center">
						<div class="inline-flex rounded-full bg-[hsl(var(--field))] p-0.5" role="group" aria-label="Work mode">
							<button type="button" onclick={() => setComposerMode('agent')} aria-label="Agent mode" aria-pressed={composerMode === 'agent'}
								class="rounded-full px-4 py-1.5 text-xs font-medium transition-all {composerMode === 'agent' ? 'bg-card text-foreground shadow-[var(--shadow-control)]' : 'text-muted-foreground hover:text-foreground'}">
								Agent
							</button>
							<button type="button" onclick={() => setComposerMode('workflow')} aria-label="Workflow mode" aria-pressed={composerMode === 'workflow'}
								class="rounded-full px-4 py-1.5 text-xs font-medium transition-all {composerMode === 'workflow' ? 'bg-card text-foreground shadow-[var(--shadow-control)]' : 'text-muted-foreground hover:text-foreground'}">
								Workflow
							</button>
						</div>
					</div>

					<div>
						<div data-new-work-composer class="ai-card relative z-10 overflow-hidden transition-all focus-within:ring-1 focus-within:ring-primary/35">
							{#if composerMode === 'workflow'}
								{#if compatibleWorkflows.length > 0}
									<div class="flex items-start gap-2 px-4 pb-2 pt-3">
										<svg class="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M4 6h6M14 6h6M7 3v6m10 2v6M4 14h6m4 4h6" /></svg>
										<div class="min-w-0 flex-1">
											<label for="new-work-workflow" class="sr-only">Workflow</label>
											<div class="relative inline-flex max-w-full items-center">
												<select id="new-work-workflow" value={selectedWorkflow} onchange={(event) => selectWorkflow(event.currentTarget.value)} aria-label="Workflow"
													class="composer-value-select max-w-full cursor-pointer appearance-none bg-transparent py-0.5 pl-0 pr-5 text-sm font-medium text-foreground outline-none">
													{#each compatibleWorkflows as workflow}<option value={workflow.id}>{workflow.name}</option>{/each}
												</select>
												<svg class="pointer-events-none absolute right-0 h-3 w-3 text-muted-foreground/60" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
											</div>
											{#if selectedWorkflowObj?.description}<p class="mt-0.5 truncate text-[11px] text-muted-foreground">{selectedWorkflowObj.description}</p>{/if}
										</div>
									</div>

									{#if workflowGoalInput}
										<textarea
											value={message}
											oninput={(event) => setWorkflowInputValue('goal', workflowGoalInput, event.currentTarget.value)}
											onkeydown={handleKeydown}
											oncompositionstart={() => (composing = true)}
											oncompositionend={() => setTimeout(() => (composing = false), 0)}
											placeholder={workflowGoalInput.description || 'Describe the outcome you want…'}
											aria-label="Workflow input goal"
											rows="2"
											disabled={sending}
											class="max-h-32 w-full resize-none bg-transparent px-4 pb-2 pt-1 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
										></textarea>
									{/if}

									{#if additionalWorkflowInputs.length > 0}
										<div class="grid gap-3 border-t border-border/60 px-4 py-3 sm:grid-cols-2">
											{#each additionalWorkflowInputs as [name, input]}
												<label class="min-w-0 {input.type === 'json' || input.type === 'string' ? 'sm:col-span-2' : ''}">
													<span class="mb-1 flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground">
														<span>{workflowInputLabel(name)}</span>
														{#if input.required}<span class="text-destructive">required</span>{/if}
														<span class="ml-auto font-mono text-[9px] font-normal text-muted-foreground/70">{input.type ?? 'string'}</span>
													</span>
													{#if input.type === 'agent'}
														<select aria-label="Agent role {name}" value={workflowInputValue(name, input)} onchange={(event) => setWorkflowInputValue(name, input, event.currentTarget.value)}
															class="w-full rounded-lg border border-input bg-card px-3 py-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring">
															<option value="">Select an agent…</option>
															{#each projectAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}
														</select>
													{:else if input.type === 'boolean'}
														<select aria-label="Workflow input {name}" value={workflowInputValue(name, input)} onchange={(event) => setWorkflowInputValue(name, input, event.currentTarget.value)}
															class="w-full rounded-lg border border-input bg-card px-3 py-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring">
															<option value="">{input.default === undefined ? 'Choose…' : 'Use default'}</option><option value="true">Yes</option><option value="false">No</option>
														</select>
													{:else if input.type === 'number'}
														<input type="number" aria-label="Workflow input {name}" value={workflowInputValue(name, input)} oninput={(event) => setWorkflowInputValue(name, input, event.currentTarget.value)}
															class="w-full rounded-lg border border-input bg-card px-3 py-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring" />
													{:else}
														<textarea aria-label="Workflow input {name}" rows={input.type === 'json' ? 3 : 2} value={workflowInputValue(name, input)} oninput={(event) => setWorkflowInputValue(name, input, event.currentTarget.value)}
															placeholder={input.description || workflowInputLabel(name)} class="w-full resize-y rounded-lg border border-input bg-card px-3 py-2 {input.type === 'json' ? 'font-mono' : ''} text-xs text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"></textarea>
													{/if}
													{#if input.description && input.type !== 'string'}<span class="mt-1 block text-[10px] leading-snug text-muted-foreground">{input.description}</span>{/if}
												</label>
											{/each}
										</div>
									{:else if !workflowGoalInput}
										<p class="px-4 pb-3 text-xs text-muted-foreground">This workflow has no inputs and is ready to run.</p>
									{/if}
								{:else}
									<div class="px-6 py-8 text-center">
										<p class="text-sm font-medium">No manual workflows yet</p>
										<p class="mt-1 text-xs text-muted-foreground">Create a workflow to coordinate reusable multi-agent work.</p>
										<a href="/workflows/new" class="mt-3 inline-flex rounded-lg border border-border bg-card px-3 py-1.5 text-xs font-medium hover:bg-accent">Create workflow</a>
									</div>
								{/if}
							{:else}
								<textarea
									bind:value={message}
									onkeydown={handleKeydown}
									oncompositionstart={() => (composing = true)}
									oncompositionend={() => setTimeout(() => (composing = false), 0)}
									onpaste={handlePaste}
									placeholder="Describe the outcome you want…"
									rows="2"
									disabled={sending}
									class="max-h-32 w-full resize-none bg-transparent px-4 pb-1 pt-3 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
								></textarea>
							{/if}

							<ImageAttachmentPreviews attachments={imagePreviews} onremove={(index) => (imageAttachments = imageAttachments.filter((_, itemIndex) => itemIndex !== index))} />

							<div class="flex min-h-10 items-center gap-2 px-3 pb-2">
								{#if composerMode === 'agent'}
									<input bind:this={imageInput} type="file" accept={IMAGE_FILE_ACCEPT} multiple onchange={handleImageInput} class="hidden" />
									<button type="button" onclick={() => imageInput?.click()} disabled={sending || Boolean(selectedConversation) || imageAttachments.length >= MAX_IMAGE_ATTACHMENTS} aria-label="Attach images" title={selectedConversation ? 'Attach files in the conversation before creating linked work' : 'Attach images (you can also paste)'}
										class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-30">
										<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>
									</button>
									{#if !selectedConversation}
										<label class="flex cursor-pointer items-center gap-1.5 px-1 text-xs text-muted-foreground hover:text-foreground" title="Do not inherit context from this agent's current conversation">
											<input type="checkbox" bind:checked={startFresh} class="h-3.5 w-3.5 rounded border-border accent-primary" />
											<span>Fresh</span>
										</label>
									{/if}
									<div class="flex-1"></div>
									<label class="relative flex min-w-0 items-center gap-1.5" title="Agent">
										{#if selectedAgentObj}
											<span class="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted text-[9px] font-semibold">{harnessMark(selectedAgentObj.backend)}</span>
											<span class="h-1.5 w-1.5 shrink-0 rounded-full {statusDot(selectedAgentObj.status)}"></span>
										{/if}
										<span class="sr-only">Agent</span>
										<select bind:value={selectedAgent} aria-label="Agent" class="composer-value-select min-w-0 max-w-36 cursor-pointer appearance-none bg-transparent py-1 pl-0 pr-4 text-xs text-muted-foreground outline-none hover:text-foreground sm:max-w-48">
											{#each projectAgents as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}
										</select>
										<svg class="pointer-events-none absolute right-0 h-3 w-3 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
									</label>
								{:else}
									<span class="text-[11px] text-muted-foreground">{workflowInputEntries.length} input{workflowInputEntries.length === 1 ? '' : 's'}</span>
									<div class="flex-1"></div>
								{/if}
								<button type="button" onclick={send} aria-label={composerMode === 'workflow' ? 'Run workflow' : 'Send work'} title={composerMode === 'workflow' ? 'Run workflow' : 'Send work'} disabled={!canSend}
									class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground transition-[background-color,transform] hover:bg-primary/90 enabled:active:scale-95 disabled:cursor-not-allowed disabled:opacity-30">
									{#if sending}
										<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary-foreground border-t-transparent"></span>
									{:else}
										<svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path stroke-linecap="round" stroke-linejoin="round" d="M12 19V5M5 12l7-7 7 7" /></svg>
									{/if}
								</button>
							</div>
						</div>

						<div data-new-work-context class="mx-3 flex min-h-10 flex-wrap items-center gap-x-3 gap-y-1 rounded-b-xl border-x border-b border-border/70 bg-secondary/75 px-3 py-2 shadow-sm">
							<label class="relative flex min-w-0 items-center gap-1.5" title="Project">
								<svg class="h-3.5 w-3.5 shrink-0 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6.75h6l1.5 2.25h9v8.25a2.25 2.25 0 0 1-2.25 2.25H6a2.25 2.25 0 0 1-2.25-2.25V6.75Z" /></svg>
								<span class="sr-only">Project</span>
								<select bind:value={selectedProject} aria-label="Project" class="composer-value-select min-w-0 max-w-36 cursor-pointer appearance-none bg-transparent py-0.5 pl-0 pr-4 text-xs text-muted-foreground outline-none hover:text-foreground sm:max-w-48">
									{#each projectList as project}<option value={project.id}>{project.name}</option>{/each}
								</select>
								<svg class="pointer-events-none absolute right-0 h-3 w-3 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
							</label>
							<span class="h-4 w-px bg-border" aria-hidden="true"></span>
							<label class="relative flex min-w-0 items-center gap-1.5" title="Conversation">
								<svg class="h-3.5 w-3.5 shrink-0 text-muted-foreground" fill="none" stroke="currentColor" stroke-width="1.8" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M8.625 9.75h6.75m-6.75 3h4.5m8.25-.75a8.625 8.625 0 0 1-8.625 8.625 9.1 9.1 0 0 1-3.75-.81L3.75 21l1.185-4.53A8.625 8.625 0 1 1 21.375 12Z" /></svg>
								<span class="sr-only">Conversation</span>
								<select bind:value={selectedConversation} aria-label="Conversation" class="composer-value-select min-w-0 max-w-36 cursor-pointer appearance-none bg-transparent py-0.5 pl-0 pr-4 text-xs text-muted-foreground outline-none hover:text-foreground sm:max-w-48">
									<option value="">Private task</option>
									{#each projectConversations as conversation}<option value={conversation.id}>{conversation.title || 'Untitled conversation'}</option>{/each}
								</select>
								<svg class="pointer-events-none absolute right-0 h-3 w-3 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
							</label>
							<a href="/projects" aria-label="Manage projects, conversations, and Agents" class="ml-auto text-[11px] text-muted-foreground transition-colors hover:text-foreground hover:underline">Manage</a>
						</div>
					</div>

					{#if composerMode === 'workflow' && imageAttachments.length > 0}
						<p class="text-center text-xs text-amber-600">Workflow runs currently accept text input only. Remove the attachments or switch to Agent mode.</p>
					{:else if selectedConversation && imageAttachments.length > 0}
						<p class="text-center text-xs text-amber-600">Put files in the conversation before creating linked work, or choose Private task.</p>
					{/if}
					{#if workflowInputError}<p class="text-center text-xs text-destructive">{workflowInputError}</p>{/if}
					{#if sendError}<p class="text-center text-sm text-destructive">{sendError}</p>{/if}
				</div>
			{/if}
		</div>
	</div>
{/if}
