<script lang="ts">
	import { onMount } from 'svelte';
	import type { UsageAccountReport, UsageBalance, UsageWindow } from '$lib/api';
	import {
		computeUsagePace,
		displayPercent,
		effectiveWindows,
		formatResetCountdown,
		usageStore,
	} from '$lib/stores/usage.svelte';
	import { harnessMark, timeAgo } from '$lib/utils';

	let accounts = $derived(usageStore.visibleAccounts);
	let deferredAutomations = $derived(usageStore.deferredAutomations);
	let displayMode = $derived(usageStore.displayMode);
	let now = $derived(usageStore.now);

	onMount(() => usageStore.subscribe());

	function toneBarClass(tone: UsageWindow['tone']): string {
		if (tone === 'danger') return 'bg-red-500';
		if (tone === 'warning') return 'bg-amber-500';
		if (tone === 'ok') return 'bg-emerald-500';
		return 'bg-muted-foreground/50';
	}

	function toneTextClass(tone: UsageWindow['tone']): string {
		if (tone === 'danger') return 'text-red-500';
		if (tone === 'warning') return 'text-amber-500';
		return 'text-foreground';
	}

	function formatBalance(balance: UsageBalance): string {
		const fmt = (n: number) =>
			balance.unit === 'usd'
				? `$${n.toFixed(2)}`
				: `${n.toLocaleString(undefined, { maximumFractionDigits: 2 })}${balance.unit === 'credits' ? ' cr' : ''}`;
		if (balance.used != null && balance.limit != null) return `${fmt(balance.used)} / ${fmt(balance.limit)}`;
		if (balance.remaining != null) return `${fmt(balance.remaining)} left`;
		if (balance.used != null) return `${fmt(balance.used)} used`;
		return '—';
	}

	function paceLabel(win: UsageWindow, report: UsageAccountReport): { text: string; cls: string } | null {
		const pace = computeUsagePace(win, report.observed_at ?? report.fetched_at, now);
		if (pace.kind === 'reached') return { text: 'Limit reached', cls: 'text-red-500' };
		if (pace.kind === 'runs_out') {
			const until = formatResetCountdown(pace.at, now);
			return { text: until ? `Runs out in ${until}` : 'Runs out early', cls: 'text-amber-500' };
		}
		if (pace.kind === 'tight') return { text: 'Tight', cls: 'text-amber-500' };
		if (pace.kind === 'on_track') return { text: 'On track', cls: 'text-emerald-500' };
		return null;
	}
</script>

{#if accounts.length > 0 || deferredAutomations.length > 0}
	<section aria-label="Harness quotas" data-dashboard-quotas class="space-y-2">
		<div class="flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
			<div class="flex items-center gap-2">
				<span class="font-medium text-foreground/90">Harness quotas</span>
				{#if usageStore.snapshot}
					<span>· updated {timeAgo(usageStore.snapshot.fetched_at)}</span>
				{/if}
			</div>
			<div class="flex items-center gap-2">
				<div class="flex rounded-md border border-border bg-card p-0.5 text-[10px]" role="group" aria-label="Quota percentage display">
					<button
						type="button"
						onclick={() => usageStore.setDisplayMode('used')}
						aria-pressed={displayMode === 'used'}
						class="rounded px-2 py-0.5 font-medium {displayMode === 'used' ? 'bg-foreground text-background' : 'text-muted-foreground'}"
					>Used %</button>
					<button
						type="button"
						onclick={() => usageStore.setDisplayMode('remaining')}
						aria-pressed={displayMode === 'remaining'}
						class="rounded px-2 py-0.5 font-medium {displayMode === 'remaining' ? 'bg-foreground text-background' : 'text-muted-foreground'}"
					>Remaining %</button>
				</div>
				<button
					type="button"
					onclick={() => void usageStore.refresh(true)}
					disabled={usageStore.refreshing}
					class="rounded-md border border-border bg-card px-2.5 py-1 text-[10px] font-medium hover:bg-accent disabled:opacity-50"
				>
					{usageStore.refreshing ? 'Refreshing…' : 'Refresh'}
				</button>
			</div>
		</div>

		{#if deferredAutomations.length > 0}
			<div class="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3.5 py-2 text-xs">
				<div class="flex flex-wrap items-center gap-2">
					<span class="font-semibold text-amber-600 dark:text-amber-400">
						{deferredAutomations.length} automation{deferredAutomations.length === 1 ? '' : 's'} waiting for quota reset:
					</span>
					{#each deferredAutomations as item (item.id)}
						{@const countdown = formatResetCountdown(item.resume_at, now)}
						<span class="rounded-md bg-card/80 px-2 py-0.5 text-[11px] font-medium">
							{item.name} · {countdown ? `resumes in ${countdown}` : item.resume_at}
						</span>
					{/each}
				</div>
				<a href="/automations" class="text-[11px] font-medium text-primary hover:underline">Manage →</a>
			</div>
		{/if}

		{#if accounts.length > 0}
			<div class="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
				{#each accounts as account (account.id)}
					{@const wins = effectiveWindows(account)}
					{@const balances = account.status === 'available' ? account.balances : (account.last_successful?.balances ?? [])}
					<div class="rounded-xl border border-border bg-card/95 p-3.5 shadow-sm" data-dashboard-quota-card={account.provider}>
						<div class="flex items-center justify-between gap-2">
							<div class="flex min-w-0 items-center gap-2">
								<span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted font-mono text-[10px] font-bold">{harnessMark(account.provider)}</span>
								<div class="min-w-0">
									<div class="flex items-center gap-1.5">
										<h3 class="truncate text-xs font-semibold">{account.provider_label}</h3>
										{#if account.plan_label}
											<span class="rounded bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium text-primary">{account.plan_label}</span>
										{/if}
									</div>
									{#if account.account_label}
										<p class="truncate text-[10px] text-muted-foreground">{account.account_label}</p>
									{/if}
								</div>
							</div>
							<span class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] text-muted-foreground">{account.harness}</span>
						</div>

						{#if account.status !== 'available'}
							<p class="mt-2 text-[11px] text-amber-600 dark:text-amber-400">
								{account.error ?? (account.problem?.kind === 'rate_limited' ? account.problem.detail : account.problem?.kind === 'no_quota' ? account.problem.detail : account.problem?.kind === 'expired' ? 'Login expired — updates on next run' : 'Unavailable')}
								{account.last_successful ? ' · showing cached reading' : ''}
							</p>
						{/if}

						{#if wins.length > 0}
							<div class="mt-2.5 space-y-2">
								{#each wins as win (win.id)}
									{@const pct = displayPercent(win, displayMode)}
									{@const reset = formatResetCountdown(win.resets_at, now)}
									{@const pace = paceLabel(win, account)}
									<div>
										<div class="flex items-center justify-between gap-2 text-[11px]">
											<span class="truncate text-foreground/90">{win.label}</span>
											<span class="flex shrink-0 items-center gap-2 font-mono text-[10px] tabular-nums">
												{#if pace}<span class={pace.cls}>{pace.text}</span>{/if}
												{#if reset}<span class="text-muted-foreground">resets {reset}</span>{/if}
												<span class="font-semibold {toneTextClass(win.tone)}">
													{pct == null ? '—' : `${Math.round(pct)}% ${displayMode === 'remaining' ? 'left' : 'used'}`}
												</span>
											</span>
										</div>
										<div class="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-muted">
											<div
												class="h-full rounded-full {toneBarClass(win.tone)}"
												style:width={`${Math.max(0, Math.min(100, win.used_pct ?? 0))}%`}
											></div>
										</div>
									</div>
								{/each}
							</div>
						{/if}

						{#if balances.length > 0}
							<div class="mt-2.5 flex flex-wrap gap-3 border-t border-border/60 pt-2 text-[11px]">
								{#each balances as bal (bal.id)}
									<span class="text-muted-foreground">
										{bal.label}: <strong class="font-mono text-foreground">{formatBalance(bal)}</strong>
									</span>
								{/each}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</section>
{/if}
