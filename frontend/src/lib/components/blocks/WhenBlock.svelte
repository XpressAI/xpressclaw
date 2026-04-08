<script lang="ts">
	let {
		label = '', switchVar = '',
		arms = [],
		flowNames = [], flowColors = {},
		stepIds = [],
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; switchVar?: string;
		arms?: { match?: string; continue?: boolean; goto?: string }[];
		flowNames?: string[]; flowColors?: Record<string, string>;
		stepIds?: { id: string; label: string; number: string }[];
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void; onremove?: () => void;
	} = $props();

	const armDotColors = ['#22c55e', '#ef4444', '#f97316', '#8b5cf6', '#06b6d4', '#eab308'];

	function armAction(arm: typeof arms[0]): string {
		if (arm.continue) return '→ continues';
		if (arm.goto?.startsWith('step ')) return `→ ${arm.goto.replace('step ', '')}`;
		if (arm.goto?.startsWith('flow ')) return `→ ${arm.goto.replace('flow ', '')}`;
		return arm.goto || '→ continues';
	}

	function updateArm(idx: number, updates: Record<string, unknown>) {
		const a = [...arms]; a[idx] = { ...a[idx], ...updates }; onupdate({ arms: a });
	}
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-amber-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">WHEN</span>
		<span class="text-sm text-foreground flex-1 truncate">{label}</span>
		<span class="flex items-center gap-3 text-xs shrink-0">
			{#each arms as arm, i}
				<span class="flex items-center gap-1 font-mono text-muted-foreground">
					<span class="h-1.5 w-1.5 rounded-full" style="background: {armDotColors[i % armDotColors.length]}"></span>
					{arm.match || 'default'} {armAction(arm)}
				</span>
			{/each}
		</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-amber-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">WHEN</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">{label}</span>
			{#if switchVar}
				<span class="text-xs text-muted-foreground font-mono">{switchVar}</span>
			{/if}
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		</div>

		<!-- Arms always visible (collapsed summary) -->
		<div class="flex gap-2 px-1 pb-1">
			{#each arms as arm, i}
				<div class="flex-1 rounded bg-muted px-2.5 py-1.5">
					<div class="flex items-center gap-1.5 mb-0.5">
						<span class="h-2 w-2 rounded-full" style="background: {armDotColors[i % armDotColors.length]}"></span>
						{#if expanded}
							<input type="text" value={arm.match || 'default'}
								oninput={(e) => updateArm(i, { match: e.currentTarget.value })}
								class="flex-1 bg-transparent text-xs font-semibold text-foreground focus:outline-none uppercase" />
						{:else}
							<span class="text-xs font-semibold text-foreground uppercase">{arm.match || 'default'}</span>
						{/if}
					</div>
					{#if expanded}
						<select value={arm.continue ? 'continue' : (arm.goto || '')}
							onchange={(e) => {
								const v = e.currentTarget.value;
								if (v === 'continue') updateArm(i, { continue: true, goto: undefined });
								else updateArm(i, { continue: undefined, goto: v });
							}}
							class="w-full bg-transparent text-xs text-muted-foreground focus:outline-none">
							<option value="continue">→ continues</option>
							{#each stepIds as s}<option value="step {s.id}">→ returns to {s.number}</option>{/each}
							{#each flowNames as f}<option value="flow {f}">→ {f}</option>{/each}
						</select>
					{:else}
						<div class="text-xs text-muted-foreground">{armAction(arm)}</div>
					{/if}
				</div>
			{/each}
		</div>

		{#if expanded}
			<div class="px-1 pb-2 pt-1 space-y-2">
				<div class="flex items-center gap-2">
					<button onclick={() => onupdate({ arms: [...arms, { match: 'default', continue: true }] })}
						class="text-[10px] text-primary hover:underline">+ Add arm</button>
					{#if arms.length > 2}
						<button onclick={() => onupdate({ arms: arms.slice(0, -1) })}
							class="text-[10px] text-muted-foreground hover:text-destructive">Remove last</button>
					{/if}
				</div>
				<div class="grid grid-cols-2 gap-2">
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
						<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
							class="w-full rounded border border-input bg-background px-2 py-1 text-xs" />
					</div>
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">SWITCH</label>
						<input type="text" value={switchVar} oninput={(e) => onupdate({ switchVar: e.currentTarget.value })}
							class="w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono" placeholder="@step.field" />
					</div>
				</div>
			</div>
		{/if}
	</div>
{/if}
