<script lang="ts">
	import JumpTarget from './JumpTarget.svelte';
	import type { WorkflowStepTarget } from '$lib/workflows/targets';
	let {
		label = '', target = '',
		flowNames = [],
		flowColors = {},
		stepsByFlow = {}, currentFlow = '', onnavigate = (_: string) => {},
		expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; target?: string;
		flowNames?: string[];
		flowColors?: Record<string, string>;
		stepsByFlow?: Record<string, WorkflowStepTarget[]>; currentFlow?: string;
		onnavigate?: (target: string) => void;
		expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();

</script>

<div class="group rounded-lg border border-border/60 bg-indigo-950/20 border-l-[3px] border-l-indigo-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-indigo-400">JUMP</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Jump'}</span>
		{#if target}
			<div class="max-w-[60%] min-w-0"><JumpTarget {target} {currentFlow} {flowColors} {stepsByFlow} {onnavigate} /></div>
		{/if}
		{#if !compact}
			<button onclick={ontoggle} aria-label={expanded ? 'Collapse jump' : 'Expand jump'} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} aria-label="Remove jump" class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if expanded && !compact}
		<div class="border-t border-border/40 px-3 py-3 space-y-2">
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
						class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
				</label>
			</div>
			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">TARGET
				<select value={target}
					onchange={(e) => onupdate({ target: e.currentTarget.value })}
					class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs mb-1">
					<option value="">Select target...</option>
					<optgroup label="Go to step (current flow)">
						{#each stepsByFlow[currentFlow] ?? [] as s}
							<option value="step {s.id}">→ {s.number} ({s.label})</option>
						{/each}
					</optgroup>
					<optgroup label="Jump to flow (start)">
						{#each flowNames as f}
							<option value="flow {f}">{f}</option>
						{/each}
					</optgroup>
					{#each flowNames as f}
						{@const fSteps = stepsByFlow[f] ?? []}
						{#if fSteps.length > 0}
							<optgroup label="Jump to {f} → step">
								{#each fSteps as s}
									<option value="flow {f} step {s.id}">{f} → {s.number} ({s.label})</option>
								{/each}
							</optgroup>
						{/if}
					{/each}
					<option value="">Custom target...</option>
				</select>
				</label>
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
