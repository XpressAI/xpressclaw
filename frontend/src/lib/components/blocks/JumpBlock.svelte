<script lang="ts">
	let {
		label = '', target = '',
		flowNames = [], flowColors = {},
		stepIds = [],
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; target?: string;
		flowNames?: string[]; flowColors?: Record<string, string>;
		stepIds?: { id: string; label: string; number: string }[];
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void; onremove?: () => void;
	} = $props();

	let targetDisplay = $derived((() => {
		if (!target) return '';
		if (target.startsWith('flow ')) {
			const name = target.replace('flow ', '').split(' ')[0];
			return `→ ${name}`;
		}
		if (target.startsWith('step ')) return `→ ${target.replace('step ', '')}`;
		if (target.startsWith('workflow ')) return `→ ${target.replace('workflow ', '')}`;
		return `→ ${target}`;
	})());

	let targetFlowName = $derived(target.startsWith('flow ') ? target.replace('flow ', '').split(' ')[0] : null);
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-slate-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">JUMP</span>
		<span class="text-sm text-foreground flex-1 truncate">{label}</span>
		<span class="text-xs font-mono flex items-center gap-1">
			{#if targetFlowName && flowColors[targetFlowName]}
				<span class="h-1.5 w-1.5 rounded-full" style="background: {flowColors[targetFlowName]}"></span>
			{/if}
			<span class="text-primary">{targetDisplay}</span>
		</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-slate-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">JUMP</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">{label}</span>
			<span class="text-xs font-mono flex items-center gap-1">
				{#if targetFlowName && flowColors[targetFlowName]}
					<span class="h-1.5 w-1.5 rounded-full" style="background: {flowColors[targetFlowName]}"></span>
				{/if}
				<span class="text-primary">{targetDisplay}</span>
			</span>
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		</div>

		{#if expanded}
			<div class="px-1 pb-2 pt-1 space-y-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">TARGET</label>
					<select value={target} onchange={(e) => onupdate({ target: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs mb-1">
						<option value="">Select target...</option>
						<optgroup label="Go to step">
							{#each stepIds as s}<option value="step {s.id}">→ {s.number} ({s.label})</option>{/each}
						</optgroup>
						<optgroup label="Jump to flow">
							{#each flowNames as f}<option value="flow {f}">{f}</option>{/each}
						</optgroup>
					</select>
				</div>
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1 text-xs" />
				</div>
			</div>
		{/if}
	</div>
{/if}
