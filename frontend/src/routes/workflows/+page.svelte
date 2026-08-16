<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { agents, schedules, workflows } from '$lib/api';
	import type { Agent, Schedule, Workflow } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import { serverTimestampMs } from '$lib/serverTime';
	import yaml from 'js-yaml';

	let workflowList = $state<Workflow[]>([]);
	let scheduleList = $state<Schedule[]>([]);
	let agentList = $state<Agent[]>([]);
	let loading = $state(true);
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let confirmDeleteId = $state<string | null>(null);
	let showScheduleCreate = $state(false);
	let scheduleForm = $state({
		schedule_type: 'cron' as 'cron' | 'once',
		name: '',
		cron: '',
		run_at: '',
		agent_id: '',
		title: '',
		description: '',
	});
	let scheduleFormError = $state('');
	let creatingSchedule = $state(false);
	let handledScheduleCreateRequest = '';

	let sortedWorkflows = $derived([...workflowList].sort((left, right) =>
		(serverTimestampMs(right.updated_at) ?? 0) - (serverTimestampMs(left.updated_at) ?? 0)
		|| right.id.localeCompare(left.id)
	));
	let sortedSchedules = $derived([...scheduleList].sort((left, right) =>
		Number(right.enabled) - Number(left.enabled)
		|| (serverTimestampMs(right.last_run ?? right.created_at) ?? 0) - (serverTimestampMs(left.last_run ?? left.created_at) ?? 0)
		|| right.id.localeCompare(left.id)
	));
	let enabledWorkflowCount = $derived(workflowList.filter((workflow) => workflow.enabled && workflowMetadata(workflow.yaml_content).cron).length);
	let activeScheduleCount = $derived(scheduleList.filter((schedule) => scheduleEnabled(schedule)).length);

	$effect(() => {
		const requestKey = `${$page.url.pathname}${$page.url.search}`;
		if ($page.url.searchParams.get('new') !== 'schedule') {
			handledScheduleCreateRequest = '';
			return;
		}
		if (agentList.length === 0 || handledScheduleCreateRequest === requestKey) return;

		handledScheduleCreateRequest = requestKey;
		showScheduleCreate = true;
		if (!scheduleForm.agent_id) scheduleForm.agent_id = agentList[0].id;
	});

	onMount(async () => {
		await loadAll();
		pollTimer = setInterval(loadAutomations, 10000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	async function loadAll() {
		await Promise.all([loadAutomations(), loadAgents()]);
		loading = false;
	}

	async function loadAutomations() {
		const [nextWorkflows, nextSchedules] = await Promise.all([
			workflows.list().catch(() => workflowList),
			schedules.list().catch(() => scheduleList),
		]);
		workflowList = nextWorkflows;
		scheduleList = nextSchedules;
	}

	async function loadAgents() {
		agentList = await agents.list().catch(() => []);
		if (!scheduleForm.agent_id && agentList.length > 0) scheduleForm.agent_id = agentList[0].id;
	}

	function workflowMetadata(yamlContent: string): { steps: number; flows: number; inputs: number; cron: string | null } {
		const stepMatches = yamlContent.match(/^\s+- id:/gm);
		let definition: { inputs?: Record<string, unknown>; schedule?: { cron?: string }; flows?: Record<string, unknown> } = {};
		try { definition = yaml.load(yamlContent) as typeof definition; } catch {}
		return {
			steps: stepMatches?.length ?? 0,
			flows: Math.max(1, Object.keys(definition?.flows ?? {}).length),
			inputs: Object.keys(definition?.inputs ?? {}).length,
			cron: definition?.schedule?.cron?.trim() || null,
		};
	}

	async function toggleWorkflow(workflow: Workflow) {
		try {
			if (workflow.enabled) await workflows.disable(workflow.id);
			else await workflows.enable(workflow.id);
			await loadAutomations();
		} catch {}
	}

	async function deleteWorkflow(id: string) {
		try {
			await workflows.delete(id);
			confirmDeleteId = null;
			await loadAutomations();
		} catch {}
	}

	function openScheduleCreate() {
		if (agentList.length === 0) return;
		if (showScheduleCreate) {
			void closeScheduleCreate();
			return;
		}
		scheduleFormError = '';
		if (!scheduleForm.agent_id) scheduleForm.agent_id = agentList[0].id;
		showScheduleCreate = true;
	}

	async function closeScheduleCreate() {
		showScheduleCreate = false;
		if ($page.url.searchParams.get('new') !== 'schedule') return;

		const consumedUrl = new URL($page.url);
		consumedUrl.searchParams.delete('new');
		await goto(`${consumedUrl.pathname}${consumedUrl.search}${consumedUrl.hash}`, {
			replaceState: true,
			keepFocus: true,
			noScroll: true,
		});
	}

	async function createSchedule() {
		const timing = scheduleForm.schedule_type === 'once' ? scheduleForm.run_at : scheduleForm.cron;
		if (!scheduleForm.name.trim() || !timing.trim() || !scheduleForm.agent_id || !scheduleForm.title.trim() || creatingSchedule) return;
		creatingSchedule = true;
		scheduleFormError = '';
		try {
			const common = {
				name: scheduleForm.name.trim(),
				agent_id: scheduleForm.agent_id,
				title: scheduleForm.title.trim(),
				description: scheduleForm.description.trim() || undefined,
			};
			if (scheduleForm.schedule_type === 'once') {
				await schedules.createOnce({ ...common, run_at: new Date(scheduleForm.run_at).toISOString() });
			} else {
				await schedules.create({ ...common, cron: scheduleForm.cron.trim() });
			}
			scheduleForm = {
				schedule_type: 'cron',
				name: '',
				cron: '',
				run_at: '',
				agent_id: agentList[0]?.id ?? '',
				title: '',
				description: '',
			};
			await closeScheduleCreate();
			await loadAutomations();
		} catch (error) {
			scheduleFormError = error instanceof Error ? error.message : String(error);
		} finally {
			creatingSchedule = false;
		}
	}

	function agentName(id: string): string {
		const agent = agentList.find((candidate) => candidate.id === id);
		return agent?.title || agent?.name || id;
	}

	function scheduleTiming(schedule: Schedule): string {
		if (schedule.schedule_type === 'once' && schedule.run_at) {
			const parsed = serverTimestampMs(schedule.run_at);
			return `Once · ${parsed === null ? schedule.run_at : new Date(parsed).toLocaleString()}`;
		}
		return schedule.cron;
	}

	function completedOneShot(schedule: Schedule): boolean {
		return schedule.schedule_type === 'once' && schedule.run_count > 0;
	}

	function scheduleEnabled(schedule: Schedule): boolean {
		return schedule.enabled && !completedOneShot(schedule);
	}

	async function toggleSchedule(schedule: Schedule) {
		if (schedule.enabled) await schedules.disable(schedule.id);
		else await schedules.enable(schedule.id);
		await loadAutomations();
	}

	async function triggerSchedule(schedule: Schedule) {
		try {
			const result = await schedules.trigger(schedule.id);
			window.alert('message' in result ? 'Conversation wake-up sent.' : `Task created: ${result.title}`);
			await loadAutomations();
		} catch (error) {
			window.alert(String(error));
		}
	}

	async function deleteSchedule(schedule: Schedule) {
		if (!window.confirm(`Delete “${schedule.name}”?`)) return;
		await schedules.delete(schedule.id);
		await loadAutomations();
	}
</script>

<div data-automations-scroll data-workflows-scroll class="workspace-scroll-y h-full">
	<div class="mx-auto max-w-7xl space-y-10 p-4 sm:p-6">
		<header class="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
			<div>
				<h1 class="text-2xl font-bold">Automations</h1>
				<p class="mt-1 text-sm text-muted-foreground">
					{workflowList.length} workflow{workflowList.length !== 1 ? 's' : ''} · {enabledWorkflowCount} automatic trigger{enabledWorkflowCount !== 1 ? 's' : ''} · {activeScheduleCount} task schedule{activeScheduleCount !== 1 ? 's' : ''}
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<a href="/workflows/new" class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90">New Workflow</a>
				<button type="button" onclick={openScheduleCreate} disabled={agentList.length === 0} class="rounded-md border border-border bg-card px-4 py-2 text-sm font-medium transition-colors hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50">New Schedule</button>
			</div>
		</header>

		<section id="workflows" class="scroll-mt-4 space-y-4">
			<div>
				<h2 class="text-lg font-semibold">Workflows</h2>
				<p class="mt-1 text-sm text-muted-foreground">Coordinate one or more agents through reusable steps, decisions, and bounded loops.</p>
			</div>

			{#if loading}
				<div class="text-sm text-muted-foreground">Loading automations…</div>
			{:else if sortedWorkflows.length === 0}
				<div class="space-y-3 rounded-lg border border-dashed border-border bg-card p-8 text-center">
					<p class="text-sm font-medium">No workflows yet</p>
					<p class="text-xs text-muted-foreground">Start with a pipeline, review loop, or goal loop.</p>
					<a href="/workflows/new" class="inline-flex rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Create Workflow</a>
				</div>
			{:else}
				<div class="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
					{#each sortedWorkflows as workflow (workflow.id)}
						{@const metadata = workflowMetadata(workflow.yaml_content)}
						<article data-workflow-card class="group space-y-3 rounded-lg border border-border bg-card p-4">
							<div class="flex items-start justify-between gap-3">
								<div class="min-w-0 flex-1">
									<a href="/workflows/{workflow.id}" class="text-sm font-semibold text-foreground hover:underline">{workflow.name}</a>
									{#if workflow.description}<p class="mt-0.5 line-clamp-2 text-xs text-muted-foreground">{workflow.description}</p>{/if}
								</div>
								{#if metadata.cron}
									<label class="relative inline-flex shrink-0 cursor-pointer items-center" title={workflow.enabled ? 'Disable schedule' : 'Enable schedule'}>
										<input type="checkbox" checked={workflow.enabled} onchange={() => toggleWorkflow(workflow)} class="peer sr-only" />
										<span class="h-[18px] w-8 rounded-full bg-muted transition-colors after:absolute after:start-[2px] after:top-[2px] after:h-3.5 after:w-3.5 after:rounded-full after:bg-white after:transition-all after:content-[''] peer-checked:bg-emerald-600 peer-checked:after:translate-x-full"></span>
									</label>
								{:else}
									<span class="rounded-full bg-muted px-2 py-0.5 text-[10px] text-muted-foreground">Manual</span>
								{/if}
							</div>
							<div class="flex flex-wrap items-center gap-3 text-[10px] text-muted-foreground"><span>{metadata.steps} step{metadata.steps !== 1 ? 's' : ''}</span><span>{metadata.flows} flow{metadata.flows !== 1 ? 's' : ''}</span><span>{metadata.inputs} input{metadata.inputs !== 1 ? 's' : ''}</span><span>v{workflow.version}</span></div>
							{#if metadata.cron}<div class="rounded bg-muted/50 px-2 py-1 font-mono text-[10px] text-muted-foreground">{workflow.enabled ? 'Scheduled' : 'Schedule paused'} · {metadata.cron}</div>{/if}
							{#if workflow.trigger_error}<div class="line-clamp-2 text-[10px] text-destructive" title={workflow.trigger_error}>Last trigger failed: {workflow.trigger_error}</div>{/if}
							<div class="flex flex-wrap items-center justify-between gap-2">
								<span class="text-[10px] text-muted-foreground">Updated {timeAgo(workflow.updated_at)}</span>
								<div class="flex items-center gap-1.5">
									<a href="/workflows/{workflow.id}?run=1" class="rounded-md bg-emerald-600 px-2.5 py-1 text-[10px] font-medium text-white transition-colors hover:bg-emerald-700">Run</a>
									<a href="/workflows/{workflow.id}" class="rounded-md border border-border bg-secondary px-2.5 py-1 text-[10px] font-medium transition-colors hover:bg-accent">View</a>
									{#if confirmDeleteId === workflow.id}
										<button type="button" onclick={() => deleteWorkflow(workflow.id)} class="rounded border border-destructive/50 bg-destructive/10 px-2.5 py-1 text-[10px] font-medium text-destructive hover:bg-destructive/20">Delete</button>
										<button type="button" onclick={() => (confirmDeleteId = null)} class="rounded border border-border px-2.5 py-1 text-[10px] text-muted-foreground hover:bg-accent">Cancel</button>
									{:else}
										<button type="button" onclick={() => (confirmDeleteId = workflow.id)} class="rounded-md border border-border bg-secondary px-2.5 py-1 text-[10px] font-medium text-destructive opacity-100 transition-colors hover:bg-destructive/10 sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100">Delete</button>
									{/if}
								</div>
							</div>
						</article>
					{/each}
				</div>
			{/if}
		</section>

		<section id="schedules" class="scroll-mt-4 space-y-4">
			<div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
				<div>
					<h2 class="text-lg font-semibold">Schedules</h2>
					<p class="mt-1 text-sm text-muted-foreground">Start or resume agent work once or on a recurring schedule.</p>
				</div>
				<button type="button" onclick={openScheduleCreate} disabled={agentList.length === 0} class="self-start rounded-md border border-border bg-card px-3 py-1.5 text-xs font-medium transition-colors hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50">New Schedule</button>
			</div>

			{#if agentList.length === 0}
				<div class="rounded-lg border border-dashed border-border bg-card p-5 text-sm text-muted-foreground">Schedules send tasks to an agent. <a href="/setup?mode=add-session" class="font-medium text-primary hover:underline">Create an agent</a> first.</div>
			{/if}

			{#if showScheduleCreate}
				<form data-schedule-form class="space-y-3 rounded-lg border border-border bg-card p-4" onsubmit={(event) => { event.preventDefault(); void createSchedule(); }}>
					<div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
						<label class="space-y-1"><span class="text-xs text-muted-foreground">Name</span><input type="text" bind:value={scheduleForm.name} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" /></label>
						<label class="space-y-1"><span class="text-xs text-muted-foreground">Timing</span><select bind:value={scheduleForm.schedule_type} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"><option value="cron">Recurring cron</option><option value="once">One-off follow-up</option></select></label>
						{#if scheduleForm.schedule_type === 'once'}
							<label class="space-y-1"><span class="text-xs text-muted-foreground">Run at</span><input type="datetime-local" bind:value={scheduleForm.run_at} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" /></label>
						{:else}
							<label class="space-y-1"><span class="text-xs text-muted-foreground">Cron</span><input type="text" placeholder="0 9 * * *" bind:value={scheduleForm.cron} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" /></label>
						{/if}
						<label class="space-y-1"><span class="text-xs text-muted-foreground">Agent</span><select bind:value={scheduleForm.agent_id} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">{#each agentList as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}</select></label>
						<label class="space-y-1 sm:col-span-2"><span class="text-xs text-muted-foreground">Task title</span><input type="text" placeholder={'Use {date} or {time} if useful'} bind:value={scheduleForm.title} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" /></label>
					</div>
					<label class="block space-y-1"><span class="text-xs text-muted-foreground">Description <span class="text-muted-foreground/70">(optional)</span></span><textarea bind:value={scheduleForm.description} rows="2" class="w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"></textarea></label>
					<p class="text-[11px] text-muted-foreground">{scheduleForm.schedule_type === 'once' ? 'One-off follow-ups resume the agent conversation once, even after a restart.' : "Cron uses the server's local time. Example: 0 9 * * 1 runs every Monday at 09:00."}</p>
					{#if scheduleFormError}<p class="text-xs text-destructive">{scheduleFormError}</p>{/if}
					<div class="flex flex-wrap gap-2"><button type="submit" disabled={!scheduleForm.name.trim() || !(scheduleForm.schedule_type === 'once' ? scheduleForm.run_at : scheduleForm.cron.trim()) || !scheduleForm.agent_id || !scheduleForm.title.trim() || creatingSchedule} class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{creatingSchedule ? 'Creating…' : 'Create schedule'}</button><button type="button" onclick={() => void closeScheduleCreate()} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">Cancel</button></div>
				</form>
			{/if}

			{#if !loading}
				<div class="space-y-2">
					{#each sortedSchedules as schedule (schedule.id)}
						<article data-schedule-card class="flex flex-col gap-3 rounded-lg border border-border bg-card p-4 sm:flex-row sm:items-center sm:gap-4">
							<div class="flex items-center gap-3 sm:contents">
								{#if completedOneShot(schedule)}
									<span class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] font-medium text-muted-foreground">Completed</span>
								{:else}
									<button type="button" onclick={() => toggleSchedule(schedule)} class="relative h-5 w-9 shrink-0 rounded-full transition-colors {schedule.enabled ? 'bg-emerald-500' : 'bg-muted'}" title={schedule.enabled ? 'Disable' : 'Enable'} aria-label="{schedule.enabled ? 'Disable' : 'Enable'} {schedule.name}"><span class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform {schedule.enabled ? 'translate-x-4' : 'translate-x-0.5'}"></span></button>
								{/if}
								<div class="min-w-0 flex-1"><div class="truncate text-sm font-semibold {!schedule.enabled ? 'text-muted-foreground' : ''}">{schedule.name}</div><div class="mt-0.5 flex flex-wrap items-center gap-x-1 text-xs text-muted-foreground"><code class="rounded bg-muted px-1 py-0.5">{scheduleTiming(schedule)}</code><span>· {agentName(schedule.agent_id)}</span><span>· {schedule.run_count} runs</span>{#if schedule.last_run}<span>· last {timeAgo(schedule.last_run)}</span>{/if}</div></div>
							</div>
							<div class="flex shrink-0 flex-wrap gap-2 sm:ml-auto"><button type="button" onclick={() => triggerSchedule(schedule)} disabled={completedOneShot(schedule)} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50">Run Now</button><button type="button" onclick={() => deleteSchedule(schedule)} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-destructive transition-colors hover:bg-destructive/10">Delete</button></div>
						</article>
					{:else}
						<div class="rounded-lg border border-dashed border-border bg-card p-8 text-center text-sm text-muted-foreground">No schedules configured</div>
					{/each}
				</div>
			{/if}
		</section>
	</div>
</div>
