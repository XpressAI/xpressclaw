<script lang="ts">
	import { onMount } from 'svelte';
	import { budget, agents } from '$lib/api';
	import type { BudgetSummary, UsageRecord } from '$lib/api';
	import { formatCost, timeAgo } from '$lib/utils';

	let summary = $state<BudgetSummary | null>(null);
	let usageHistory = $state<UsageRecord[]>([]);
	let loading = $state(true);

	// Resume dialog state
	let showResumeDialog = $state(false);
	let resumeAgentId = $state('');
	let resumeNewDaily = $state('');
	let resumeNewMonthly = $state('');
	let resuming = $state(false);

	onMount(load);

	async function load() {
		const [s, u] = await Promise.all([
			budget.summary().catch(() => null),
			budget.usage(undefined, 50).catch(() => [])
		]);
		summary = s;
		usageHistory = u;
		loading = false;
	}

	function openResumeDialog(agentId: string) {
		resumeAgentId = agentId;
		resumeNewDaily = '';
		resumeNewMonthly = '';
		showResumeDialog = true;
	}

	async function handleResume() {
		resuming = true;
		try {
			// Update budget if new limits provided
			if (resumeNewDaily || resumeNewMonthly) {
				const budgetUpdate: Record<string, unknown> = {
					daily: resumeNewDaily ? `$${resumeNewDaily}` : null,
					monthly: resumeNewMonthly ? `$${resumeNewMonthly}` : null,
					on_exceeded: 'pause',
					fallback_model: 'local',
					warn_at_percent: 80,
					per_task: null,
				};
				await agents.updateConfig(resumeAgentId, { budget: budgetUpdate as any });
			}

			// Resume the agent
			await budget.resume(resumeAgentId);
			showResumeDialog = false;

			// Reload data
			await load();
		} catch (e) {
			alert(String(e));
		}
		resuming = false;
	}
</script>

<div class="p-6 space-y-6">
	<div>
		<h1 class="text-2xl font-bold">Budget</h1>
		<p class="text-sm text-muted-foreground mt-1">
			{#if summary}
				Total spent: {formatCost(summary.global.total_spent)}
			{:else}
				Loading...
			{/if}
		</p>
	</div>

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else if summary}
		<!-- Global budget -->
		<div class="grid grid-cols-1 md:grid-cols-3 gap-4">
			<div class="rounded-lg border border-border bg-card p-4">
				<div class="text-sm text-muted-foreground">Daily Spend</div>
				<div class="mt-1 text-2xl font-bold">{formatCost(summary.global.daily_spent)}</div>
				{#if summary.global.daily_limit}
					<div class="mt-2 h-2 rounded-full bg-muted overflow-hidden">
						<div
							class="h-full rounded-full {summary.global.daily_spent / summary.global.daily_limit > 0.8 ? 'bg-destructive' : 'bg-emerald-500'}"
							style="width: {Math.min(100, (summary.global.daily_spent / summary.global.daily_limit) * 100)}%"
						></div>
					</div>
					<div class="text-xs text-muted-foreground mt-1">of {formatCost(summary.global.daily_limit)} limit</div>
				{/if}
			</div>

			<div class="rounded-lg border border-border bg-card p-4">
				<div class="text-sm text-muted-foreground">Monthly Spend</div>
				<div class="mt-1 text-2xl font-bold">{formatCost(summary.global.monthly_spent)}</div>
				{#if summary.global.monthly_limit}
					<div class="mt-2 h-2 rounded-full bg-muted overflow-hidden">
						<div
							class="h-full rounded-full {summary.global.monthly_spent / summary.global.monthly_limit > 0.8 ? 'bg-destructive' : 'bg-emerald-500'}"
							style="width: {Math.min(100, (summary.global.monthly_spent / summary.global.monthly_limit) * 100)}%"
						></div>
					</div>
					<div class="text-xs text-muted-foreground mt-1">of {formatCost(summary.global.monthly_limit)} limit</div>
				{/if}
			</div>

			<div class="rounded-lg border border-border bg-card p-4">
				<div class="text-sm text-muted-foreground">Total Spend</div>
				<div class="mt-1 text-2xl font-bold">{formatCost(summary.global.total_spent)}</div>
			</div>
		</div>

		<!-- Per-agent breakdown -->
		{#if summary.agents.length > 0}
			<div class="rounded-lg border border-border bg-card">
				<div class="border-b border-border px-4 py-3">
					<h2 class="text-sm font-semibold">Agent Breakdown</h2>
				</div>
				<div class="divide-y divide-border">
					{#each summary.agents as a}
						<div class="flex items-center justify-between px-4 py-3">
							<div>
								<div class="text-sm font-medium">{a.agent_id}</div>
								{#if a.is_paused}
									<span class="text-xs text-destructive">Paused (budget exceeded)</span>
								{/if}
							</div>
							<div class="flex items-center gap-3">
								<div class="text-right text-sm">
									<div class="font-medium">{formatCost(a.total_spent)}</div>
									<div class="text-xs text-muted-foreground">
										{formatCost(a.daily_spent)} today
									</div>
								</div>
								{#if a.is_paused}
									<button
										onclick={() => openResumeDialog(a.agent_id)}
										class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90"
									>
										Resume
									</button>
								{/if}
							</div>
						</div>
					{/each}
				</div>
			</div>
		{/if}

		<!-- Usage history -->
		{#if usageHistory.length > 0}
			<div class="rounded-lg border border-border bg-card">
				<div class="border-b border-border px-4 py-3">
					<h2 class="text-sm font-semibold">Recent Usage</h2>
				</div>
				<div class="overflow-x-auto">
					<table class="w-full text-sm">
						<thead>
							<tr class="border-b border-border text-muted-foreground text-xs">
								<th class="px-4 py-2 text-left font-medium">Time</th>
								<th class="px-4 py-2 text-left font-medium">Agent</th>
								<th class="px-4 py-2 text-left font-medium">Model</th>
								<th class="px-4 py-2 text-right font-medium">Tokens</th>
								<th class="px-4 py-2 text-right font-medium">Cost</th>
							</tr>
						</thead>
						<tbody class="divide-y divide-border">
							{#each usageHistory.slice(0, 20) as record}
								<tr class="text-xs">
									<td class="px-4 py-2 text-muted-foreground">{timeAgo(record.timestamp)}</td>
									<td class="px-4 py-2">{record.agent_id}</td>
									<td class="px-4 py-2 font-mono text-muted-foreground">{record.model}</td>
									<td class="px-4 py-2 text-right">{(record.input_tokens + record.output_tokens).toLocaleString()}</td>
									<td class="px-4 py-2 text-right">{formatCost(record.cost_usd)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</div>
		{/if}
	{/if}
</div>

<!-- Resume dialog -->
{#if showResumeDialog}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
		<div class="rounded-lg border border-border bg-card p-6 shadow-lg w-96 space-y-4">
			<h3 class="text-lg font-semibold">Resume Agent: {resumeAgentId}</h3>
			<p class="text-sm text-muted-foreground">
				This agent was paused because it exceeded its budget.
				Set a new budget limit to resume, or resume with the current limit.
			</p>
			<div class="space-y-3">
				<div>
					<label class="block text-xs text-muted-foreground mb-1">New daily limit (optional)</label>
					<input type="text" bind:value={resumeNewDaily} placeholder="e.g. 20.00"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">New monthly limit (optional)</label>
					<input type="text" bind:value={resumeNewMonthly} placeholder="e.g. 500.00"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
				</div>
			</div>
			<div class="flex gap-2 justify-end">
				<button
					onclick={() => { showResumeDialog = false; }}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent"
				>
					Cancel
				</button>
				<button
					onclick={handleResume}
					disabled={resuming}
					class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
				>
					{resuming ? 'Resuming...' : 'Resume Agent'}
				</button>
			</div>
		</div>
	</div>
{/if}
