<script lang="ts">
	let {
		label = '', switchVar = '',
		arms = [],
		flowNames = [],
		flowColors = {},
		stepIds = [],
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; switchVar?: string;
		arms?: { match?: string; continue?: boolean; goto?: string }[];
		flowNames?: string[];
		flowColors?: Record<string, string>;
		stepIds?: { id: string; label: string; number: string }[];
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();

	const armColors = ['#22c55e', '#ef4444', '#f97316', '#8b5cf6', '#06b6d4', '#eab308'];

	function updateArm(idx: number, updates: Record<string, unknown>) {
		const a = [...arms];
		a[idx] = { ...a[idx], ...updates };
		onupdate({ arms: a });
	}

	function addArm() {
		onupdate({ arms: [...arms, { match: 'default', continue: true }] });
	}

	function removeArm(idx: number) {
		if (arms.length <= 2) return;
		onupdate({ arms: arms.filter((_, i) => i !== idx) });
	}

	function armActionLabel(arm: typeof arms[0]): string {
		if (arm.continue) return '→ continues';
		if (arm.goto?.startsWith('step ')) return `→ returns to ${arm.goto.replace('step ', '')}`;
		if (arm.goto?.startsWith('flow ')) return `→ ${arm.goto.replace('flow ', '')}`;
		if (arm.goto?.startsWith('workflow ')) return `→ workflow ${arm.goto.replace('workflow ', '')}`;
		return arm.goto || '→ continues';
	}
</script>

<div class="group rounded-lg border border-border/60 bg-amber-950/20 border-l-[3px] border-l-amber-500">
	<!-- Header -->
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-amber-400">WHEN</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Condition'}</span>
		{#if switchVar}
			<span class="text-[10px] text-amber-300/70 font-mono">{switchVar}</span>
		{/if}
		{#if !compact}
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if !compact}
		<!-- Arms (always visible, even when not fully expanded) -->
		<div class="border-t border-border/40">
			<div class="flex divide-x divide-border/30">
				{#each arms as arm, i}
					<div class="flex-1 px-3 py-2">
						<div class="flex items-center gap-1.5 mb-1.5">
							<span class="h-2 w-2 rounded-full flex-shrink-0" style="background: {armColors[i % armColors.length]}"></span>
							{#if expanded}
								<input type="text" value={arm.match || 'default'}
									oninput={(e) => updateArm(i, { match: e.currentTarget.value })}
									class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-semibold uppercase" />
								{#if arms.length > 2}
									<button onclick={() => removeArm(i)} class="text-muted-foreground/40 hover:text-destructive text-[10px]">x</button>
								{/if}
							{:else}
								<span class="text-xs font-semibold uppercase {i === 0 ? 'text-emerald-400' : i === 1 ? 'text-red-400' : 'text-amber-400'}">
									{arm.match || 'default'}
								</span>
							{/if}
						</div>
						{#if expanded}
							<select value={arm.continue ? 'continue' : (arm.goto || '')}
								onchange={(e) => {
									const v = e.currentTarget.value;
									if (v === 'continue') updateArm(i, { continue: true, goto: undefined });
									else updateArm(i, { continue: undefined, goto: v });
								}}
								class="w-full rounded border border-input bg-background px-1.5 py-1 text-xs mb-1">
								<option value="continue">→ continues</option>
								<optgroup label="Return to step">
									{#each stepIds as s}
										<option value="step {s.id}">→ returns to {s.number} ({s.label})</option>
									{/each}
								</optgroup>
								<optgroup label="Jump to flow">
									{#each flowNames as f}
										<option value="flow {f}">→ {f}</option>
									{/each}
								</optgroup>
								<option value="">→ (custom target)</option>
							</select>
							{#if !arm.continue && arm.goto && !arm.goto.startsWith('step ') && !arm.goto.startsWith('flow ')}
								<input type="text" value={arm.goto}
									oninput={(e) => updateArm(i, { goto: e.currentTarget.value })}
									class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] font-mono"
									placeholder="flow X / step Y / workflow Z" />
							{/if}
						{:else}
							<div class="text-xs text-muted-foreground">
								{#if arm.goto?.startsWith('flow ')}
									{@const flowName = arm.goto.replace('flow ', '').split(' ')[0]}
									<span class="inline-flex items-center gap-1">
										→
										<span class="h-1.5 w-1.5 rounded-full inline-block" style="background: {flowColors[flowName] || '#888'}"></span>
										<span class="font-medium">{flowName}</span>
									</span>
								{:else}
									{armActionLabel(arm)}
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>
			{#if expanded}
				<div class="px-3 pb-2">
					<button onclick={addArm} class="text-[10px] text-primary hover:underline">+ Add arm</button>
				</div>
			{/if}
		</div>

		{#if expanded}
			<div class="border-t border-border/40 px-3 py-2 space-y-2">
				<div class="grid grid-cols-2 gap-2">
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
						<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
							class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
					</div>
					<div>
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">SWITCH</label>
						<input type="text" value={switchVar} oninput={(e) => onupdate({ switchVar: e.currentTarget.value })}
							class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="@step.field" />
					</div>
				</div>
			</div>
		{/if}
	{/if}
</div>
