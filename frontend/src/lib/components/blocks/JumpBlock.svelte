<script lang="ts">
	let {
		label = '', target = '',
		flowNames = [],
		flowColors = {},
		stepIds = [],
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; target?: string;
		flowNames?: string[];
		flowColors?: Record<string, string>;
		stepIds?: { id: string; label: string; number: string }[];
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();

	let targetDisplay = $derived(() => {
		if (!target) return '(no target)';
		if (target.startsWith('flow ')) {
			const parts = target.replace('flow ', '').split(' step ');
			const flowName = parts[0];
			const step = parts[1];
			return step ? `→ ${flowName} step ${step}` : `→ ${flowName}`;
		}
		if (target.startsWith('workflow ')) return `→ workflow ${target.replace('workflow ', '')}`;
		return `→ ${target}`;
	});
</script>

<div class="group rounded-lg border border-border/60 bg-indigo-950/20 border-l-[3px] border-l-indigo-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-indigo-400">JUMP</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Jump'}</span>
		{#if target && !expanded}
			{@const flowName = target.startsWith('flow ') ? target.replace('flow ', '').split(' ')[0] : null}
			<span class="text-xs text-muted-foreground flex items-center gap-1">
				→
				{#if flowName && flowColors[flowName]}
					<span class="h-1.5 w-1.5 rounded-full inline-block" style="background: {flowColors[flowName]}"></span>
				{/if}
				<span class="font-mono">{flowName || target}</span>
			</span>
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

	{#if expanded && !compact}
		<div class="border-t border-border/40 px-3 py-3 space-y-2">
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
				<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
					class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
			</div>
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">TARGET</label>
				<select value={target}
					onchange={(e) => onupdate({ target: e.currentTarget.value })}
					class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs mb-1">
					<option value="">Select target...</option>
					<optgroup label="Go to step (current flow)">
						{#each stepIds as s}
							<option value="step {s.id}">→ {s.number} ({s.label})</option>
						{/each}
					</optgroup>
					<optgroup label="Jump to flow (start)">
						{#each flowNames as f}
							<option value="flow {f}">{f}</option>
						{/each}
					</optgroup>
					{#each flowNames as f}
						{@const fSteps = stepIds.filter(() => true)}
						{#if fSteps.length > 0}
							<optgroup label="Jump to {f} → step">
								{#each stepIds as s}
									<option value="flow {f} step {s.id}">{f} → {s.number} ({s.label})</option>
								{/each}
							</optgroup>
						{/if}
					{/each}
					<option value="">Custom target...</option>
				</select>
				{#if target && !target.startsWith('step ') && !target.startsWith('flow ') && target !== ''}
					<input type="text" value={target}
						oninput={(e) => onupdate({ target: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono"
						placeholder="workflow name" />
				{/if}
			</div>
		</div>
	{/if}
</div>
