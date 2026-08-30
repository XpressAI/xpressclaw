<script lang="ts">
	import type { DashboardSeriesPoint } from '$lib/api';
	import { serverTimestampMs } from '$lib/serverTime';

	let {
		points,
		mode,
	}: {
		points: DashboardSeriesPoint[];
		mode: 'context' | 'tools' | 'code';
	} = $props();

	const width = 920;
	const height = 250;
	const insetX = 38;
	const insetTop = 18;
	const insetBottom = 30;

	let values = $derived(points.map((point) => mode === 'context'
		? point.context_used
		: mode === 'tools' ? point.tool_calls : Math.max(point.code_additions, point.code_deletions)));
	let maximum = $derived(Math.max(1, ...values));
	let gitAvailability = $derived(overallGitAvailability(points));
	let noCodeActivity = $derived(mode === 'code'
		&& gitAvailability !== 'unavailable'
		&& points.every((point) => point.code_additions === 0 && point.code_deletions === 0));
	let spansMultipleDays = $derived(points.length > 1
		&& (serverTimestampMs(points.at(-1)?.timestamp) ?? 0) - (serverTimestampMs(points[0]?.timestamp) ?? 0) > 36 * 60 * 60 * 1_000);
	let mainPath = $derived(linePath(points.map((point) => mode === 'context' ? point.context_used : point.tool_calls)));
	let additionsPath = $derived(codeLinePath('code_additions'));
	let deletionsPath = $derived(codeLinePath('code_deletions'));
	let areaPath = $derived(mainPath ? `${mainPath} L ${x(points.length - 1)} ${height - insetBottom} L ${x(0)} ${height - insetBottom} Z` : '');

	function x(index: number): number {
		if (points.length <= 1) return insetX;
		return insetX + index * (width - insetX * 2) / (points.length - 1);
	}

	function y(value: number): number {
		return insetTop + (height - insetTop - insetBottom) * (1 - value / maximum);
	}

	function linePath(series: number[]): string {
		return series.map((value, index) => `${index === 0 ? 'M' : 'L'} ${x(index).toFixed(2)} ${y(value).toFixed(2)}`).join(' ');
	}

	function codeLinePath(field: 'code_additions' | 'code_deletions'): string {
		let drawing = false;
		return points.map((point, index) => {
			if (point.git_state === 'unavailable') {
				drawing = false;
				return '';
			}
			const command = drawing ? 'L' : 'M';
			drawing = true;
			return `${command} ${x(index).toFixed(2)} ${y(point[field]).toFixed(2)}`;
		}).filter(Boolean).join(' ');
	}

	function overallGitAvailability(series: DashboardSeriesPoint[]): DashboardSeriesPoint['git_state'] {
		const states = new Set(series.map((point) => point.git_state));
		if (states.has('partial') || (states.has('available') && states.has('unavailable'))) return 'partial';
		if (states.has('available')) return 'available';
		if (states.has('unavailable')) return 'unavailable';
		return 'none';
	}

	function compact(value: number): string {
		if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}m`;
		if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
		return String(value);
	}

	function tickLabel(point: DashboardSeriesPoint): string {
		const timestamp = serverTimestampMs(point.timestamp);
		if (timestamp === null) return '';
		const date = new Date(timestamp);
		return spansMultipleDays
			? date.toLocaleDateString([], { weekday: 'short', hour: 'numeric' })
			: date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
	}

	function pointLabel(point: DashboardSeriesPoint): string {
		if (mode === 'context') return `${tickLabel(point)}: ${point.context_used.toLocaleString()} context used${point.context_size ? ` of ${point.context_size.toLocaleString()}` : ''}`;
		if (mode === 'tools') return `${tickLabel(point)}: ${point.tool_calls} tool calls`;
		return `${tickLabel(point)}: ${point.code_additions} additions, ${point.code_deletions} deletions${point.git_state === 'partial' ? ' (partial)' : ''}`;
	}
</script>

<div class="relative min-h-0 w-full" data-dashboard-chart data-chart-mode={mode}>
	<svg
		class="h-[15.5rem] w-full overflow-visible"
		viewBox="0 0 {width} {height}"
		role="img"
		aria-label={mode === 'context' ? 'Context usage over time' : mode === 'tools' ? 'Tool-call volume over time' : 'Code additions and deletions over time'}
		preserveAspectRatio="none"
	>
		<defs>
			<linearGradient id="dashboard-chart-fill" x1="0" y1="0" x2="0" y2="1">
				<stop offset="0%" stop-color="hsl(var(--primary))" stop-opacity="0.28" />
				<stop offset="100%" stop-color="hsl(var(--primary))" stop-opacity="0" />
			</linearGradient>
		</defs>
		{#each [0, 0.25, 0.5, 0.75, 1] as fraction}
			<line x1={insetX} x2={width - insetX} y1={insetTop + fraction * (height - insetTop - insetBottom)} y2={insetTop + fraction * (height - insetTop - insetBottom)} stroke="hsl(var(--border))" stroke-width="1" stroke-dasharray={fraction === 1 ? undefined : '3 6'} />
		{/each}
		<text x="4" y={insetTop + 5} fill="hsl(var(--foreground-tertiary))" font-size="11">{compact(maximum)}</text>
		<text x="18" y={height - insetBottom + 4} fill="hsl(var(--foreground-tertiary))" font-size="11">0</text>
		{#if mode === 'code'}
			<path d={additionsPath} fill="none" stroke="hsl(var(--success))" stroke-width="2.5" vector-effect="non-scaling-stroke" />
			<path d={deletionsPath} fill="none" stroke="hsl(var(--warning))" stroke-width="2.5" vector-effect="non-scaling-stroke" />
		{:else}
			<path d={areaPath} fill="url(#dashboard-chart-fill)" />
			<path d={mainPath} fill="none" stroke="hsl(var(--primary))" stroke-width="2.5" vector-effect="non-scaling-stroke" />
		{/if}
		{#each points as point, index}
			{#if mode !== 'code' || point.git_state !== 'unavailable'}
				<circle cx={x(index)} cy={y(mode === 'context' ? point.context_used : mode === 'tools' ? point.tool_calls : point.code_additions)} r="9" fill="transparent">
					<title>{pointLabel(point)}</title>
				</circle>
			{/if}
		{/each}
		{#if points.length > 0}
			<text x={insetX} y={height - 5} fill="hsl(var(--foreground-tertiary))" font-size="11">{tickLabel(points[0])}</text>
			<text x={width - insetX} y={height - 5} text-anchor="end" fill="hsl(var(--foreground-tertiary))" font-size="11">{tickLabel(points[points.length - 1])}</text>
		{/if}
	</svg>
	{#if mode === 'code'}
		<div class="absolute right-3 top-2 flex items-center gap-4 text-[11px] text-muted-foreground" aria-hidden="true">
			<span class="flex items-center gap-1.5"><span class="h-0.5 w-4 bg-[hsl(var(--success))]"></span>Additions</span>
			<span class="flex items-center gap-1.5"><span class="h-0.5 w-4 bg-[hsl(var(--warning))]"></span>Deletions</span>
		</div>
	{/if}
	{#if mode === 'code' && gitAvailability === 'unavailable'}
		<div class="pointer-events-none absolute inset-0 flex items-center justify-center"><span class="rounded-full border border-border bg-card/90 px-3 py-1.5 text-xs text-muted-foreground shadow-sm">Git line metrics unavailable in this window</span></div>
	{:else if mode === 'code' && gitAvailability === 'partial'}
		<div class="absolute bottom-7 right-3 rounded-md border border-amber-500/25 bg-[hsl(var(--warning-tint))] px-2 py-1 text-[10px] text-[hsl(var(--warning))]" title="Some workspace data could not be measured. Binary and untracked files are excluded.">Partial Git data</div>
	{:else if noCodeActivity}
		<div class="pointer-events-none absolute inset-0 flex items-center justify-center"><span class="rounded-full border border-border bg-card/90 px-3 py-1.5 text-xs text-muted-foreground shadow-sm">No Git line changes in this window</span></div>
	{/if}
</div>
