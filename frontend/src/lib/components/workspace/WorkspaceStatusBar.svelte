<script lang="ts">
	import { onMount } from 'svelte';
	import type { Agent, Task, UsageAccountReport, UsageBalance, UsageWindow } from '$lib/api';
	import { resourceStore } from '$lib/stores/resources.svelte';
	import {
		computeUsagePace,
		displayPercent,
		effectiveWindows,
		formatResetCountdown,
		summaryWindows,
		usageStore,
	} from '$lib/stores/usage.svelte';
	import { harnessMark, timeAgo } from '$lib/utils';

	let {
		agentList = [],
		taskList = [],
		attentionCount = 0,
		serverConnected = true,
		dockerAvailable = true,
		onnavigate,
	}: {
		agentList: Agent[];
		taskList: Task[];
		attentionCount: number;
		serverConnected: boolean;
		dockerAvailable: boolean;
		onnavigate?: (path: string) => void;
	} = $props();

	let openPopover = $state<'agents' | 'quotas' | null>(null);
	let barEl = $state<HTMLElement>();

	let resources = $derived(resourceStore.resources);
	let resourcesStale = $derived(resourceStore.stale);
	let pinnedAccounts = $derived(usageStore.pinnedAccounts);
	let allAccounts = $derived(usageStore.accounts);
	let deferredAutomations = $derived(usageStore.deferredAutomations);
	let displayMode = $derived(usageStore.displayMode);
	let now = $derived(usageStore.now);

	let activeAgents = $derived(
		agentList.filter((a) => {
			const s = (a.status || '').toLowerCase();
			return s === 'working' || s === 'running' || s === 'active' || s === 'busy';
		}),
	);
	let workingAgents = $derived(
		agentList.filter((a) => {
			const s = (a.status || '').toLowerCase();
			return s === 'working' || s === 'busy';
		}),
	);
	let queuedTaskCount = $derived(
		taskList.filter((t) => {
			const s = (t.activity_status ?? t.status ?? '').toLowerCase();
			return s === 'queued' || s === 'pending';
		}).length,
	);
	let primaryDisk = $derived(
		resources?.disks?.find((d) => d.capacity != null) ?? null,
	);

	onMount(() => {
		const unsubResources = resourceStore.subscribe();
		const unsubUsage = usageStore.subscribe();
		const handlePointerDown = (event: PointerEvent) => {
			if (!openPopover || !barEl) return;
			if (!barEl.contains(event.target as Node)) {
				openPopover = null;
			}
		};
		const handleKeyDown = (event: KeyboardEvent) => {
			if (event.key === 'Escape' && openPopover) {
				openPopover = null;
			}
		};
		window.addEventListener('pointerdown', handlePointerDown);
		window.addEventListener('keydown', handleKeyDown);
		return () => {
			unsubResources();
			unsubUsage();
			window.removeEventListener('pointerdown', handlePointerDown);
			window.removeEventListener('keydown', handleKeyDown);
		};
	});

	function navigate(path: string, event?: MouseEvent) {
		if (onnavigate && event && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) {
			event.preventDefault();
			openPopover = null;
			onnavigate(path);
			return;
		}
		openPopover = null;
	}

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

	function resourceLevelClass(pct: number | null | undefined): string {
		if (pct == null) return 'text-muted-foreground';
		if (pct >= 90) return 'text-red-500';
		if (pct >= 80) return 'text-amber-500';
		return 'text-foreground/85';
	}

	function formatBytesShort(bytes: number): string {
		const gib = bytes / 1024 ** 3;
		if (gib >= 1) return `${gib.toFixed(gib >= 10 ? 0 : 1)}G`;
		return `${Math.round(bytes / 1024 ** 2)}M`;
	}

	function formatBalance(balance: UsageBalance): string {
		const fmt = (n: number) =>
			balance.unit === 'usd'
				? `$${n.toFixed(2)}`
				: `${n.toLocaleString(undefined, { maximumFractionDigits: 2 })}${balance.unit === 'credits' ? ' cr' : ''}`;
		if (balance.used != null && balance.limit != null) {
			return `${fmt(balance.used)} / ${fmt(balance.limit)}`;
		}
		if (balance.remaining != null) {
			return `${fmt(balance.remaining)} left`;
		}
		if (balance.used != null) {
			return `${fmt(balance.used)} used`;
		}
		return '—';
	}

	function paceSummary(win: UsageWindow, report: UsageAccountReport): { label: string; tone: string } | null {
		const observed = report.observed_at ?? report.fetched_at;
		const pace = computeUsagePace(win, observed, now);
		if (pace.kind === 'reached') return { label: 'Limit reached', tone: 'text-red-500' };
		if (pace.kind === 'runs_out') {
			const until = formatResetCountdown(pace.at, now);
			return { label: until ? `Runs out in ${until}` : 'Runs out early', tone: 'text-amber-500' };
		}
		if (pace.kind === 'tight') return { label: 'Tight pace', tone: 'text-amber-500' };
		if (pace.kind === 'on_track') return { label: 'On track', tone: 'text-emerald-500' };
		return null;
	}

	function accountProblemLabel(report: UsageAccountReport): string | null {
		if (report.status === 'available') return null;
		if (report.problem) {
			switch (report.problem.kind) {
				case 'expired':
					return `Login expired · updates next time ${report.problem.refreshed_by ?? report.harness} runs`;
				case 'rejected':
					return `Auth rejected (HTTP ${report.problem.status})`;
				case 'no_quota':
					return report.problem.detail;
				case 'rate_limited':
					return report.problem.detail;
			}
		}
		return report.error ?? 'Usage unavailable';
	}
</script>

<footer
	bind:this={barEl}
	data-workspace-statusbar
	aria-label="Workspace status bar"
	class="relative z-40 hidden h-7 shrink-0 select-none items-center justify-between gap-2 border-t border-border/70 bg-card/95 px-2.5 text-[11px] text-muted-foreground backdrop-blur lg:flex"
>
	<!-- Left: Health + Agents + Queue / Attention / Quota Waits -->
	<div class="flex min-w-0 items-center gap-1.5">
		<a
			href="/dashboard"
			onclick={(e) => navigate('/dashboard', e)}
			data-statusbar-connection
			class="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-accent hover:text-foreground"
			title={!serverConnected ? 'Reconnecting to XpressClaw server' : !dockerAvailable ? 'Container runtime is not running' : 'Connected · Open Control center'}
		>
			<span
				class="h-1.5 w-1.5 rounded-full {!serverConnected ? 'animate-pulse bg-amber-500' : !dockerAvailable ? 'bg-red-500' : 'bg-emerald-500'}"
				aria-hidden="true"
			></span>
			<span class="font-medium text-foreground/85">
				{!serverConnected ? 'Reconnecting' : !dockerAvailable ? 'Runtime off' : 'Live'}
			</span>
		</a>

		<span class="h-3 w-px bg-border/70" aria-hidden="true"></span>

		<button
			type="button"
			data-statusbar-agents
			aria-expanded={openPopover === 'agents'}
			aria-haspopup="dialog"
			onclick={() => (openPopover = openPopover === 'agents' ? null : 'agents')}
			class="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-accent hover:text-foreground {openPopover === 'agents' ? 'bg-accent text-foreground' : ''}"
			title="Active and configured agents"
		>
			<span class="h-1.5 w-1.5 rounded-full {workingAgents.length > 0 ? 'animate-pulse bg-emerald-500' : activeAgents.length > 0 ? 'bg-primary' : 'bg-muted-foreground/40'}" aria-hidden="true"></span>
			<span class="tabular-nums">
				<span class="font-medium text-foreground/90">{activeAgents.length}</span>/{agentList.length} agent{agentList.length === 1 ? '' : 's'}
			</span>
		</button>

		{#if queuedTaskCount > 0}
			<a
				href="/tasks"
				onclick={(e) => navigate('/tasks', e)}
				data-statusbar-queued
				class="rounded px-1.5 py-0.5 tabular-nums transition-colors hover:bg-accent hover:text-foreground"
				title="Queued tasks"
			>
				{queuedTaskCount} queued
			</a>
		{/if}

		{#if attentionCount > 0}
			<a
				href="/dashboard"
				onclick={(e) => navigate('/dashboard', e)}
				data-statusbar-attention
				class="flex items-center gap-1 rounded bg-amber-500/12 px-1.5 py-0.5 font-medium text-amber-600 transition-colors hover:bg-amber-500/20 dark:text-amber-400"
				title="Items waiting for your attention"
			>
				<span class="h-1.5 w-1.5 rounded-full bg-amber-500" aria-hidden="true"></span>
				<span class="tabular-nums">{attentionCount} needs you</span>
			</a>
		{/if}

		{#if deferredAutomations.length > 0}
			<button
				type="button"
				data-statusbar-quota-waits
				onclick={() => (openPopover = openPopover === 'quotas' ? null : 'quotas')}
				class="flex items-center gap-1 rounded bg-amber-500/12 px-1.5 py-0.5 font-medium text-amber-600 transition-colors hover:bg-amber-500/20 dark:text-amber-400"
				title="Automations paused until harness quota resets"
			>
				<span class="tabular-nums">{deferredAutomations.length} waiting on quota</span>
			</button>
		{/if}
	</div>

	<!-- Center: Pinned Harness Quotas -->
	<div class="flex min-w-0 items-center justify-center gap-1">
		<button
			type="button"
			data-statusbar-quotas
			aria-expanded={openPopover === 'quotas'}
			aria-haspopup="dialog"
			onclick={() => (openPopover = openPopover === 'quotas' ? null : 'quotas')}
			class="flex max-w-[34rem] items-center gap-2.5 overflow-hidden rounded px-2 py-0.5 transition-colors hover:bg-accent hover:text-foreground {openPopover === 'quotas' ? 'bg-accent text-foreground' : ''}"
			title="Harness usage quotas · Click to inspect or refresh"
		>
			{#if pinnedAccounts.length > 0}
				{#each pinnedAccounts as account (account.id)}
					{@const wins = summaryWindows(account)}
					<span class="flex shrink-0 items-center gap-1.5" data-statusbar-quota-pill={account.provider}>
						<span class="rounded bg-muted px-1 py-0.25 font-mono text-[9px] font-bold text-foreground/85">{harnessMark(account.provider)}</span>
						{#each wins as win (win.id)}
							{@const pct = displayPercent(win, displayMode)}
							<span class="flex items-center gap-1">
								{#if win.short_label}
									<span class="text-[10px] text-muted-foreground">{win.short_label}</span>
								{/if}
								<span class="h-1.5 w-6 overflow-hidden rounded-full bg-muted" aria-hidden="true">
									<span
										class="block h-full rounded-full {toneBarClass(win.tone)}"
										style:width={`${Math.max(0, Math.min(100, win.used_pct ?? 0))}%`}
									></span>
								</span>
								<span class="font-mono text-[10px] font-medium tabular-nums {toneTextClass(win.tone)}">
									{pct == null ? '—' : `${Math.round(pct)}%`}
								</span>
							</span>
						{/each}
					</span>
				{/each}
			{:else if allAccounts.length > 0}
				<span class="text-[11px] text-muted-foreground">Quotas ({allAccounts.length})</span>
			{:else}
				<span class="text-[11px] text-muted-foreground">Quotas</span>
			{/if}
		</button>
	</div>

	<!-- Right: Host System Resources (CPU / Mem / Disk) -->
	<a
		href="/dashboard"
		onclick={(e) => navigate('/dashboard', e)}
		data-statusbar-resources
		class="flex shrink-0 items-center gap-2.5 rounded px-1.5 py-0.5 font-mono text-[10px] tabular-nums transition-colors hover:bg-accent hover:text-foreground {resourcesStale ? 'opacity-70' : ''}"
		title={resources ? `Host resources · Sampled ${timeAgo(resources.sampled_at)} · Click for Control center` : 'Host resources · Click for Control center'}
	>
		<span data-statusbar-cpu class={resourceLevelClass(resources?.cpu_percent)}>
			CPU {resources?.cpu_percent != null ? `${Math.round(resources.cpu_percent)}%` : resources?.cpu_count ? '…' : '—'}
		</span>
		<span data-statusbar-memory class={resourceLevelClass(resources?.memory?.used_percent)}>
			MEM {resources?.memory ? `${Math.round(resources.memory.used_percent)}% (${formatBytesShort(resources.memory.used_bytes)}/${formatBytesShort(resources.memory.total_bytes)})` : '—'}
		</span>
		<span data-statusbar-disk class={resourceLevelClass(primaryDisk?.capacity?.used_percent)}>
			DISK {primaryDisk?.capacity ? `${Math.round(primaryDisk.capacity.used_percent)}%` : '—'}
		</span>
	</a>

	<!-- Popover: Agents Roster -->
	{#if openPopover === 'agents'}
		<div
			role="dialog"
			aria-label="Agents status"
			data-statusbar-agents-popover
			class="absolute bottom-8 left-2 z-50 w-72 rounded-xl border border-border bg-card p-3 text-xs text-foreground shadow-2xl"
		>
			<div class="mb-2 flex items-center justify-between">
				<span class="font-semibold">Agents ({activeAgents.length} active / {agentList.length} total)</span>
				<a href="/dashboard" onclick={(e) => navigate('/dashboard', e)} class="text-[10px] text-primary hover:underline">Control center →</a>
			</div>
			{#if agentList.length === 0}
				<p class="py-3 text-center text-muted-foreground">No agents configured yet.</p>
			{:else}
				<div class="max-h-60 space-y-1 overflow-y-auto">
					{#each agentList as agent (agent.id)}
						<a
							href="/agents/{agent.id}"
							onclick={(e) => navigate(`/agents/${agent.id}`, e)}
							class="flex items-center gap-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-accent"
						>
							<span class="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted font-mono text-[9px] font-bold">{harnessMark(agent.config?.runner?.kind || agent.backend)}</span>
							<span class="min-w-0 flex-1 truncate font-medium">{agent.title || agent.name}</span>
							<span class="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">{agent.status}</span>
						</a>
					{/each}
				</div>
			{/if}
		</div>
	{/if}

	<!-- Popover: Harness Quotas Roster -->
	{#if openPopover === 'quotas'}
		<div
			role="dialog"
			aria-label="Harness usage quotas"
			data-statusbar-quotas-popover
			class="absolute bottom-8 left-1/2 z-50 w-[26rem] max-w-[92vw] -translate-x-1/2 rounded-xl border border-border bg-card p-3.5 text-xs text-foreground shadow-2xl"
		>
			<div class="mb-2.5 flex items-center justify-between gap-2">
				<div>
					<h3 class="text-xs font-semibold">Harness Quotas</h3>
					<p class="text-[10px] text-muted-foreground">
						{#if usageStore.snapshot}
							Updated {timeAgo(usageStore.snapshot.fetched_at)}
						{:else if usageStore.loading}
							Checking local harness logins…
						{:else}
							Read-only local credential check
						{/if}
					</p>
				</div>
				<div class="flex items-center gap-1.5">
					<div class="flex rounded-md border border-border bg-muted/50 p-0.5 text-[10px]" role="group" aria-label="Percentage display">
						<button
							type="button"
							onclick={() => usageStore.setDisplayMode('used')}
							aria-pressed={displayMode === 'used'}
							class="rounded px-1.5 py-0.5 font-medium {displayMode === 'used' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground'}"
						>Used</button>
						<button
							type="button"
							onclick={() => usageStore.setDisplayMode('remaining')}
							aria-pressed={displayMode === 'remaining'}
							class="rounded px-1.5 py-0.5 font-medium {displayMode === 'remaining' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground'}"
						>Left</button>
					</div>
					<button
						type="button"
						onclick={() => void usageStore.refresh(true)}
						disabled={usageStore.refreshing}
						class="rounded-md border border-border px-2 py-1 text-[10px] font-medium hover:bg-accent disabled:opacity-50"
					>
						{usageStore.refreshing ? 'Refreshing…' : 'Refresh'}
					</button>
				</div>
			</div>

			{#if usageStore.error}
				<div class="mb-2 rounded-lg border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-[11px] text-destructive">
					{usageStore.error}
				</div>
			{/if}

			{#if deferredAutomations.length > 0}
				<div class="mb-2.5 rounded-lg border border-amber-500/30 bg-amber-500/10 p-2.5">
					<div class="mb-1 flex items-center justify-between text-[10px] font-semibold uppercase tracking-wide text-amber-600 dark:text-amber-400">
						<span>Waiting for quota reset ({deferredAutomations.length})</span>
						<a href="/automations" onclick={(e) => navigate('/automations', e)} class="underline">Automations →</a>
					</div>
					<div class="space-y-1">
						{#each deferredAutomations as item (item.id)}
							{@const countdown = formatResetCountdown(item.resume_at, now)}
							<div class="flex items-baseline justify-between gap-2 text-[11px]">
								<span class="min-w-0 truncate font-medium">{item.name}</span>
								<span class="shrink-0 font-mono text-[10px] text-amber-600 dark:text-amber-400">
									{countdown ? `resumes in ${countdown}` : item.resume_at}
								</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}

			{#if allAccounts.length === 0}
				<div class="rounded-lg border border-dashed border-border p-4 text-center text-muted-foreground">
					<p class="font-medium text-foreground/85">No harness quota sources detected</p>
					<p class="mt-1 text-[11px]">Sign in to Claude, Codex, Antigravity, Copilot, Cursor, Kimi, Grok, OpenCode Go, or GLM on this host to track live quotas here.</p>
				</div>
			{:else}
				<div class="max-h-80 space-y-2 overflow-y-auto pr-0.5">
					{#each allAccounts as account (account.id)}
						{@const wins = effectiveWindows(account)}
						{@const balances = account.status === 'available' ? account.balances : (account.last_successful?.balances ?? [])}
						{@const problemText = accountProblemLabel(account)}
						{@const isPinned = usageStore.pinnedIds.includes(account.id)}
						{@const isHidden = usageStore.hiddenIds.includes(account.id)}
						<div class="rounded-lg border border-border/80 bg-background/60 p-2.5 {isHidden ? 'opacity-55' : ''}" data-quota-card={account.provider}>
							<div class="flex items-center justify-between gap-2">
								<div class="flex min-w-0 items-center gap-1.5">
									<span class="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted font-mono text-[9px] font-bold">{harnessMark(account.provider)}</span>
									<span class="truncate font-semibold">{account.provider_label}</span>
									{#if account.plan_label}
										<span class="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium text-primary">{account.plan_label}</span>
									{/if}
									{#if account.account_label}
										<span class="truncate text-[10px] text-muted-foreground">{account.account_label}</span>
									{/if}
								</div>
								<div class="flex shrink-0 items-center gap-1">
									<button
										type="button"
										onclick={() => usageStore.togglePin(account.id)}
										class="rounded px-1.5 py-0.5 text-[10px] transition-colors hover:bg-accent {isPinned ? 'bg-primary/15 text-primary' : 'text-muted-foreground'}"
										title={isPinned ? 'Unpin from status bar' : 'Pin to status bar'}
									>
										{isPinned ? 'Pinned' : 'Pin'}
									</button>
									<button
										type="button"
										onclick={() => usageStore.toggleHidden(account.id)}
										class="rounded px-1.5 py-0.5 text-[10px] text-muted-foreground transition-colors hover:bg-accent"
									>
										{isHidden ? 'Show' : 'Hide'}
									</button>
								</div>
							</div>

							{#if problemText}
								<p class="mt-1.5 text-[10px] text-amber-600 dark:text-amber-400">
									{problemText}{account.last_successful ? ' · showing cached reading' : ''}
								</p>
							{/if}

							{#if wins.length > 0}
								<div class="mt-2 space-y-1.5">
									{#each wins as win (win.id)}
										{@const pct = displayPercent(win, displayMode)}
										{@const reset = formatResetCountdown(win.resets_at, now)}
										{@const pace = paceSummary(win, account)}
										<div>
											<div class="flex items-center justify-between gap-2 text-[11px]">
												<span class="truncate text-foreground/90">{win.label}</span>
												<span class="flex shrink-0 items-center gap-2 font-mono text-[10px] tabular-nums">
													{#if pace}
														<span class={pace.tone}>{pace.label}</span>
													{/if}
													{#if reset}
														<span class="text-muted-foreground">resets {reset}</span>
													{/if}
													<span class="font-semibold {toneTextClass(win.tone)}">
														{pct == null ? '—' : `${Math.round(pct)}% ${displayMode === 'remaining' ? 'left' : 'used'}`}
													</span>
												</span>
											</div>
											<div class="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-muted">
												<div
													class="h-full rounded-full transition-[width] duration-300 {toneBarClass(win.tone)}"
													style:width={`${Math.max(0, Math.min(100, win.used_pct ?? 0))}%`}
												></div>
											</div>
										</div>
									{/each}
								</div>
							{/if}

							{#if balances.length > 0}
								<div class="mt-2 flex flex-wrap gap-2 border-t border-border/50 pt-1.5 text-[10px]">
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

			<div class="mt-2.5 flex items-center justify-between border-t border-border/60 pt-2 text-[10px] text-muted-foreground">
				<span>Read-only · never refreshes harness OAuth tokens</span>
				<a href="/dashboard" onclick={(e) => navigate('/dashboard', e)} class="font-medium text-primary hover:underline">Control center →</a>
			</div>
		</div>
	{/if}
</footer>
