<script lang="ts">
	import { onMount } from 'svelte';
	import { budget as budgetApi } from '$lib/api';
	import type { LiveConfig, UsageRecord } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: {
			budget?: {
				daily: string | null;
				monthly: string | null;
				per_task: string | null;
				on_exceeded: string;
				fallback_model: string;
				warn_at_percent: number;
			} | null;
		}) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();

	let budgetEnabled = $state(false);
	let daily = $state('');
	let monthly = $state('');
	let perTask = $state('');
	let onExceeded = $state('pause');
	let fallbackModel = $state('local');
	let warnPercent = $state(80);

	let usageRecords = $state<UsageRecord[]>([]);
	let loadingUsage = $state(true);
	let usageError = $state<string | null>(null);

	$effect(() => {
		if (agentConfig) {
			if (agentConfig.budget) {
				budgetEnabled = true;
				daily = agentConfig.budget.daily ?? '';
				monthly = agentConfig.budget.monthly ?? '';
				perTask = agentConfig.budget.per_task ?? '';
				onExceeded = agentConfig.budget.on_exceeded ?? 'pause';
				fallbackModel = agentConfig.budget.fallback_model ?? 'local';
				warnPercent = agentConfig.budget.warn_at_percent ?? 80;
			} else {
				budgetEnabled = false;
			}
		}
	});

	onMount(() => {
		loadUsage();
	});

	async function loadUsage() {
		loadingUsage = true;
		usageError = null;
		try {
			usageRecords = await budgetApi.usage(agentId, 20);
		} catch (e) {
			usageError = `Failed to load usage: ${e}`;
		}
		loadingUsage = false;
	}

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			handleSave();
		}
	});

	function handleSave() {
		if (budgetEnabled) {
			onSave({
				budget: {
					daily: daily.trim() || null,
					monthly: monthly.trim() || null,
					per_task: perTask.trim() || null,
					on_exceeded: onExceeded,
					fallback_model: fallbackModel,
					warn_at_percent: warnPercent,
				},
			});
		} else {
			onSave({ budget: null });
		}
	}

	function formatCost(cost: number): string {
		return `$${cost.toFixed(4)}`;
	}
</script>

<div class="space-y-6">
	<!-- Budget Limits -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Budget Limits</h2>
		<label class="flex items-center gap-2 cursor-pointer">
			<input type="checkbox" bind:checked={budgetEnabled} class="rounded border-border" />
			<span class="text-sm">Enable budget limits for this agent</span>
		</label>

		{#if budgetEnabled}
			<div class="grid grid-cols-3 gap-3">
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Daily limit</label>
					<input
						type="text"
						bind:value={daily}
						placeholder="$20.00"
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Monthly limit</label>
					<input
						type="text"
						bind:value={monthly}
						placeholder="$500.00"
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Per-task limit</label>
					<input
						type="text"
						bind:value={perTask}
						placeholder="$5.00"
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
			</div>

			<div class="grid grid-cols-3 gap-3">
				<div>
					<label class="block text-xs text-muted-foreground mb-1">On exceeded</label>
					<select
						bind:value={onExceeded}
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					>
						<option value="pause">Pause</option>
						<option value="alert">Alert</option>
						<option value="degrade">Degrade</option>
						<option value="stop">Stop</option>
					</select>
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Fallback model</label>
					<input
						type="text"
						bind:value={fallbackModel}
						placeholder="local"
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
				<div>
					<label class="block text-xs text-muted-foreground mb-1">Warn at %</label>
					<input
						type="number"
						bind:value={warnPercent}
						min="0"
						max="100"
						class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
					/>
				</div>
			</div>
		{/if}
	</div>

	<!-- Usage History -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<div class="flex items-center justify-between">
			<h2 class="text-sm font-semibold">Usage History</h2>
			<button
				onclick={loadUsage}
				class="rounded-md border border-border px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
			>
				Refresh
			</button>
		</div>

		{#if loadingUsage}
			<p class="text-sm text-muted-foreground">Loading usage...</p>
		{:else if usageError}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{usageError}
			</div>
		{:else if usageRecords.length === 0}
			<p class="text-sm text-muted-foreground italic">No usage records for this agent.</p>
		{:else}
			<div class="overflow-x-auto">
				<table class="w-full text-sm">
					<thead>
						<tr class="border-b border-border text-left">
							<th class="py-2 pr-4 text-xs font-medium text-muted-foreground">Date</th>
							<th class="py-2 pr-4 text-xs font-medium text-muted-foreground">Model</th>
							<th class="py-2 pr-4 text-xs font-medium text-muted-foreground">Tokens</th>
							<th class="py-2 text-xs font-medium text-muted-foreground">Cost</th>
						</tr>
					</thead>
					<tbody>
						{#each usageRecords as record}
							<tr class="border-b border-border/50 hover:bg-accent/30">
								<td class="py-2 pr-4 text-xs text-muted-foreground">
									{timeAgo(record.timestamp)}
								</td>
								<td class="py-2 pr-4 text-xs font-mono">
									{record.model}
								</td>
								<td class="py-2 pr-4 text-xs">
									{record.input_tokens + record.output_tokens}
								</td>
								<td class="py-2 text-xs font-mono">
									{formatCost(record.cost_usd)}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>

</div>
