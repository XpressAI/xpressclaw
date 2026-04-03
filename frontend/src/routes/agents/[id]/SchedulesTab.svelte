<script lang="ts">
	import { onMount } from 'svelte';
	import { schedules } from '$lib/api';
	import type { Schedule } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	interface Props {
		agentId: string;
	}

	let { agentId }: Props = $props();

	let scheduleList = $state<Schedule[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Create form
	let showCreateForm = $state(false);
	let newName = $state('');
	let newCron = $state('');
	let newTitle = $state('');
	let creating = $state(false);

	let togglingId = $state<string | null>(null);
	let triggeringId = $state<string | null>(null);

	onMount(() => {
		loadSchedules();
	});

	async function loadSchedules() {
		loading = true;
		error = null;
		try {
			scheduleList = await schedules.list(agentId);
		} catch (e) {
			error = `Failed to load schedules: ${e}`;
		}
		loading = false;
	}

	async function toggleEnabled(schedule: Schedule) {
		togglingId = schedule.id;
		try {
			if (schedule.enabled) {
				await schedules.disable(schedule.id);
			} else {
				await schedules.enable(schedule.id);
			}
			await loadSchedules();
		} catch (e) {
			error = `Failed to toggle schedule: ${e}`;
		}
		togglingId = null;
	}

	async function triggerNow(id: string) {
		triggeringId = id;
		try {
			await schedules.trigger(id);
		} catch (e) {
			error = `Failed to trigger: ${e}`;
		}
		triggeringId = null;
	}

	async function createSchedule() {
		if (!newName.trim() || !newCron.trim() || creating) return;
		creating = true;
		error = null;
		try {
			await schedules.create({
				name: newName.trim(),
				cron: newCron.trim(),
				agent_id: agentId,
				title: newTitle.trim() || newName.trim(),
			});
			newName = '';
			newCron = '';
			newTitle = '';
			showCreateForm = false;
			await loadSchedules();
		} catch (e) {
			error = `Failed to create schedule: ${e}`;
		}
		creating = false;
	}

	async function deleteSchedule(id: string) {
		try {
			await schedules.delete(id);
			await loadSchedules();
		} catch (e) {
			error = `Failed to delete schedule: ${e}`;
		}
	}
</script>

<div class="space-y-6">
	<div class="rounded-lg border border-border bg-card p-4 space-y-4">
		<div class="flex items-center justify-between">
			<h2 class="text-sm font-semibold">Schedules</h2>
			<button
				onclick={() => { showCreateForm = !showCreateForm; }}
				class="rounded-md border border-border px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
			>
				{showCreateForm ? 'Cancel' : 'Create Schedule'}
			</button>
		</div>

		{#if showCreateForm}
			<div class="rounded-md border border-border p-3 space-y-3">
				<div class="grid grid-cols-2 gap-3">
					<div>
						<label class="block text-xs text-muted-foreground mb-1">Name</label>
						<input
							type="text"
							bind:value={newName}
							placeholder="daily-check"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
						/>
					</div>
					<div>
						<label class="block text-xs text-muted-foreground mb-1">Cron Expression</label>
						<input
							type="text"
							bind:value={newCron}
							placeholder="0 9 * * *"
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring"
						/>
					</div>
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Task Title</label>
					<input
						type="text"
						bind:value={newTitle}
						placeholder="Task title when triggered (defaults to name)"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div class="flex justify-end">
					<button
						onclick={createSchedule}
						disabled={!newName.trim() || !newCron.trim() || creating}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
					>
						{creating ? 'Creating...' : 'Create'}
					</button>
				</div>
			</div>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading schedules...</p>
		{:else if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{error}
			</div>
		{:else if scheduleList.length === 0}
			<p class="text-sm text-muted-foreground italic">No schedules configured for this agent.</p>
		{:else}
			<div class="space-y-2">
				{#each scheduleList as schedule}
					<div class="rounded-md border border-border p-3 space-y-2 hover:bg-accent/30">
						<div class="flex items-center justify-between">
							<div class="flex items-center gap-3 min-w-0">
								<button
									onclick={() => toggleEnabled(schedule)}
									disabled={togglingId === schedule.id}
									class="shrink-0 w-9 h-5 rounded-full relative transition-colors {schedule.enabled ? 'bg-emerald-500' : 'bg-muted'}"
									title={schedule.enabled ? 'Disable' : 'Enable'}
								>
									<span
										class="absolute top-0.5 w-4 h-4 rounded-full bg-white shadow transition-transform {schedule.enabled ? 'left-4' : 'left-0.5'}"
									></span>
								</button>
								<div class="min-w-0">
									<span class="text-sm font-medium text-foreground">{schedule.name}</span>
									<span class="ml-2 text-xs font-mono text-muted-foreground">{schedule.cron}</span>
								</div>
							</div>
							<div class="flex items-center gap-2 shrink-0">
								<button
									onclick={() => triggerNow(schedule.id)}
									disabled={triggeringId === schedule.id}
									class="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
								>
									{triggeringId === schedule.id ? 'Triggering...' : 'Trigger Now'}
								</button>
								<button
									onclick={() => deleteSchedule(schedule.id)}
									class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-destructive transition-colors"
									title="Delete schedule"
								>
									&#x2715;
								</button>
							</div>
						</div>
						<div class="flex items-center gap-4 text-xs text-muted-foreground">
							<span>Runs: {schedule.run_count}</span>
							{#if schedule.last_run}
								<span>Last run: {timeAgo(schedule.last_run)}</span>
							{:else}
								<span>Never run</span>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
