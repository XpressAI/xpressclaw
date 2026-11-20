<script lang="ts">
	import type { Schedule, Workflow } from '$lib/api';
	import { serverTimestampMs } from '$lib/serverTime';
	import { timeAgo } from '$lib/utils';

	let {
		workflowList,
		scheduleList,
		activeWorkflowId = null,
		compact = false,
		showHeading = true,
		onnavigate,
	}: {
		workflowList: Workflow[];
		scheduleList: Schedule[];
		activeWorkflowId?: string | null;
		compact?: boolean;
		showHeading?: boolean;
		onnavigate?: () => void;
	} = $props();

	let sortedWorkflows = $derived([...workflowList].sort((left, right) =>
		(serverTimestampMs(right.updated_at) ?? 0) - (serverTimestampMs(left.updated_at) ?? 0)
		|| right.id.localeCompare(left.id)
	));
	let sortedSchedules = $derived([...scheduleList].sort((left, right) =>
		Number(right.enabled) - Number(left.enabled)
		|| (serverTimestampMs(right.last_run ?? right.created_at) ?? 0) - (serverTimestampMs(left.last_run ?? left.created_at) ?? 0)
		|| right.id.localeCompare(left.id)
	));

	function scheduleTiming(schedule: Schedule): string {
		if (schedule.schedule_type === 'once' && schedule.run_at) {
			const parsed = serverTimestampMs(schedule.run_at);
			return `Once · ${parsed === null ? schedule.run_at : new Date(parsed).toLocaleString()}`;
		}
		return schedule.cron;
	}

	function scheduleEnabled(schedule: Schedule): boolean {
		return schedule.enabled && !(schedule.schedule_type === 'once' && schedule.run_count > 0);
	}
</script>

{#if compact}
	<div data-sidebar-mode="automations" class="flex flex-col items-center gap-1">
		{#each sortedWorkflows as workflow (workflow.id)}
			<a
				href="/workflows/{workflow.id}"
				onclick={onnavigate}
				aria-current={activeWorkflowId === workflow.id ? 'page' : undefined}
				class="relative flex h-9 w-9 items-center justify-center rounded-lg text-xs font-semibold {activeWorkflowId === workflow.id ? 'bg-[hsl(var(--sidebar-active))]' : 'bg-muted/60 hover:bg-accent'}"
				title="Workflow: {workflow.name} — {workflow.enabled ? 'Enabled' : 'Disabled'}"
			>
				W
				<span class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {workflow.enabled ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
			</a>
		{/each}
		{#each sortedSchedules as schedule (schedule.id)}
			<a
				href="/automations#schedules"
				onclick={onnavigate}
				class="relative flex h-9 w-9 items-center justify-center rounded-lg bg-muted/60 text-xs font-semibold hover:bg-accent"
				title="Schedule: {schedule.name} — {scheduleTiming(schedule)}"
			>
				S
				<span class="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[hsl(var(--sidebar))] {scheduleEnabled(schedule) ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
			</a>
		{/each}
	</div>
{:else}
	<div data-sidebar-mode="automations">
		{#if showHeading}
			<div class="mb-2 flex items-center justify-between px-2">
				<a href="/automations" onclick={onnavigate} class="text-[10px] font-semibold uppercase tracking-[0.14em] text-muted-foreground hover:text-foreground">Automations</a>
			</div>
		{/if}

		<div class="mb-4">
			<div class="mb-1 flex items-center justify-between px-2">
				<a href="/automations#workflows" onclick={onnavigate} class="text-[10px] font-medium uppercase tracking-[0.12em] text-muted-foreground/80 hover:text-foreground">Workflows</a>
				<a href="/workflows/new" onclick={onnavigate} class="flex h-5 w-5 items-center justify-center rounded text-sm text-muted-foreground hover:bg-accent hover:text-foreground" title="New workflow">+</a>
			</div>
			{#if sortedWorkflows.length === 0}
				<div class="px-2 py-2 text-xs text-muted-foreground">No workflows yet.</div>
			{:else}
				<div class="space-y-0.5">
					{#each sortedWorkflows as workflow (workflow.id)}
						<a
							data-sidebar-workflow
							href="/workflows/{workflow.id}"
							onclick={onnavigate}
							aria-current={activeWorkflowId === workflow.id ? 'page' : undefined}
							class="flex items-start gap-2 rounded-lg px-2 py-2 transition-colors {activeWorkflowId === workflow.id ? 'bg-[hsl(var(--sidebar-active))] text-foreground' : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'}"
						>
							<span class="min-w-0 flex-1">
								<span class="block truncate text-xs">{workflow.name}</span>
								<span class="mt-0.5 block truncate text-[10px]">Workflow · {timeAgo(workflow.updated_at)}</span>
							</span>
							<span aria-label={workflow.enabled ? 'Enabled' : 'Disabled'} class="mt-1.5 h-2 w-2 shrink-0 rounded-full {workflow.enabled ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
						</a>
					{/each}
				</div>
			{/if}
		</div>

		<div>
			<div class="mb-1 flex items-center justify-between px-2">
				<a href="/automations#schedules" onclick={onnavigate} class="text-[10px] font-medium uppercase tracking-[0.12em] text-muted-foreground/80 hover:text-foreground">Schedules</a>
				<a href="/automations?new=schedule#schedules" onclick={onnavigate} class="flex h-5 w-5 items-center justify-center rounded text-sm text-muted-foreground hover:bg-accent hover:text-foreground" title="New schedule">+</a>
			</div>
			{#if sortedSchedules.length === 0}
				<div class="px-2 py-2 text-xs text-muted-foreground">No schedules yet.</div>
			{:else}
				<div class="space-y-0.5">
					{#each sortedSchedules as schedule (schedule.id)}
						<a data-sidebar-schedule href="/automations#schedules" onclick={onnavigate} class="flex items-start gap-2 rounded-lg px-2 py-2 text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground">
							<span class="min-w-0 flex-1">
								<span class="block truncate text-xs">{schedule.name}</span>
								<span class="mt-0.5 block truncate text-[10px]">{scheduleTiming(schedule)}</span>
							</span>
							<span aria-label={scheduleEnabled(schedule) ? 'Enabled' : 'Disabled'} class="mt-1.5 h-2 w-2 shrink-0 rounded-full {scheduleEnabled(schedule) ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
						</a>
					{/each}
				</div>
			{/if}
		</div>
	</div>
{/if}
