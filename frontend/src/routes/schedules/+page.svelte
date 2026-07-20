<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, schedules } from '$lib/api';
	import type { Agent, Schedule } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let scheduleList = $state<Schedule[]>([]);
	let agentList = $state<Agent[]>([]);
	let loading = $state(true);
	let showCreate = $state(false);
	let form = $state({
		schedule_type: 'cron' as 'cron' | 'once',
		name: '',
		cron: '',
		run_at: '',
		agent_id: '',
		title: '',
		description: ''
	});
	let formError = $state('');
	let creating = $state(false);

	onMount(async () => {
		await Promise.all([load(), loadAgents()]);
	});

	async function loadAgents() {
		agentList = await agents.list().catch(() => []);
		if (!form.agent_id && agentList.length > 0) form.agent_id = agentList[0].id;
	}

	async function load() {
		scheduleList = await schedules.list().catch(() => []);
		loading = false;
	}

	async function create() {
		const hasTiming = form.schedule_type === 'once' ? form.run_at : form.cron;
		if (!form.name || !hasTiming || !form.agent_id || !form.title || creating) return;
		creating = true;
		formError = '';
		try {
			const common = {
				name: form.name.trim(),
				agent_id: form.agent_id,
				title: form.title.trim(),
				description: form.description.trim() || undefined
			};
			if (form.schedule_type === 'once') {
				await schedules.createOnce({
					...common,
					run_at: new Date(form.run_at).toISOString()
				});
			} else {
				await schedules.create({ ...common, cron: form.cron.trim() });
			}
			form = {
				schedule_type: 'cron',
				name: '',
				cron: '',
				run_at: '',
				agent_id: agentList[0]?.id ?? '',
				title: '',
				description: ''
			};
			showCreate = false;
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		} finally {
			creating = false;
		}
	}

	function openCreate() {
		if (agentList.length === 0) return;
		formError = '';
		if (!form.agent_id) form.agent_id = agentList[0].id;
		showCreate = !showCreate;
	}

	function agentName(id: string): string {
		const agent = agentList.find((candidate) => candidate.id === id);
		return agent?.title || agent?.name || id;
	}

	function timing(schedule: Schedule): string {
		if (schedule.schedule_type === 'once' && schedule.run_at) {
			return `Once · ${new Date(schedule.run_at).toLocaleString()}`;
		}
		return schedule.cron;
	}

	function completedOneShot(schedule: Schedule): boolean {
		return schedule.schedule_type === 'once' && schedule.run_count > 0;
	}

	async function toggle(s: Schedule) {
		if (s.enabled) {
			await schedules.disable(s.id);
		} else {
			await schedules.enable(s.id);
		}
		await load();
	}

	async function trigger(id: string) {
		try {
			const task = await schedules.trigger(id);
			alert(`Task created: ${task.title}`);
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	async function remove(id: string) {
		if (!confirm('Delete this schedule?')) return;
		await schedules.delete(id);
		await load();
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Schedules</h1>
			<p class="text-sm text-muted-foreground mt-1">
				{scheduleList.filter((s) => s.enabled).length} active of {scheduleList.length}
			</p>
		</div>
		<button
			onclick={openCreate}
			disabled={agentList.length === 0}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors disabled:cursor-not-allowed disabled:opacity-50"
		>
			New Schedule
		</button>
	</div>

	{#if agentList.length === 0}
		<div class="rounded-lg border border-dashed border-border bg-card p-5 text-sm text-muted-foreground">
			Schedules send tasks to a project. <a href="/setup?mode=add-session" class="font-medium text-primary hover:underline">Create a project</a> first.
		</div>
	{/if}

	{#if showCreate}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div class="grid grid-cols-2 gap-3">
				<input type="text" placeholder="Name" bind:value={form.name} class="rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
				<select bind:value={form.schedule_type} class="rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
					<option value="cron">Recurring cron</option>
					<option value="once">One-off follow-up</option>
				</select>
				{#if form.schedule_type === 'once'}
					<input type="datetime-local" aria-label="Run at" bind:value={form.run_at} class="rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" />
				{:else}
					<input type="text" placeholder="Cron (e.g. 0 9 * * *)" bind:value={form.cron} class="rounded-md border border-input bg-background px-3 py-2 text-sm font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
				{/if}
				<select bind:value={form.agent_id} class="rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
					{#each agentList as agent}<option value={agent.id}>{agent.title || agent.name}</option>{/each}
				</select>
				<input type="text" placeholder={"Task title (use {date}, {time})"} bind:value={form.title} class="rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
			</div>
			<textarea placeholder="Description (optional)" bind:value={form.description} rows="2" class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring resize-none"></textarea>
			<p class="text-[11px] text-muted-foreground">
				{form.schedule_type === 'once'
					? 'One-off follow-ups resume the project conversation once, even after a restart.'
					: "Cron uses the server's local time. Example: 0 9 * * 1 runs every Monday at 09:00."}
			</p>
			{#if formError}<p class="text-xs text-destructive">{formError}</p>{/if}
			<div class="flex gap-2">
				<button onclick={create} disabled={!form.name.trim() || !(form.schedule_type === 'once' ? form.run_at : form.cron.trim()) || !form.agent_id || !form.title.trim() || creating} class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{creating ? 'Creating…' : 'Create schedule'}</button>
				<button onclick={() => (showCreate = false)} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">Cancel</button>
			</div>
		</div>
	{/if}

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else}
		<div class="space-y-2">
			{#each scheduleList as s}
				<div class="rounded-lg border border-border bg-card p-4 flex items-center gap-4">
					{#if completedOneShot(s)}
						<span class="rounded-full bg-muted px-2 py-0.5 text-[10px] font-medium text-muted-foreground">Completed</span>
					{:else}
						<button
							onclick={() => toggle(s)}
							class="h-5 w-9 rounded-full transition-colors {s.enabled ? 'bg-emerald-500' : 'bg-muted'} relative"
							title={s.enabled ? 'Disable' : 'Enable'}
						>
							<div class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform {s.enabled ? 'translate-x-4' : 'translate-x-0.5'}"></div>
						</button>
					{/if}

					<div class="flex-1 min-w-0">
						<div class="text-sm font-semibold {!s.enabled ? 'text-muted-foreground' : ''}">{s.name}</div>
						<div class="text-xs text-muted-foreground mt-0.5">
							<code class="bg-muted px-1 py-0.5 rounded">{timing(s)}</code>
							&middot; {agentName(s.agent_id)}
							&middot; {s.run_count} runs
							{#if s.last_run}
								&middot; last: {timeAgo(s.last_run)}
							{/if}
						</div>
					</div>

					<div class="flex gap-2 shrink-0">
						<button
							onclick={() => trigger(s.id)}
							disabled={completedOneShot(s)}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent transition-colors disabled:cursor-not-allowed disabled:opacity-50"
						>
							Run Now
						</button>
						<button
							onclick={() => remove(s.id)}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/10 transition-colors"
						>
							Delete
						</button>
					</div>
				</div>
			{:else}
				<div class="rounded-lg border border-border bg-card p-8 text-center text-sm text-muted-foreground">
					No schedules configured
				</div>
			{/each}
		</div>
	{/if}
</div>
