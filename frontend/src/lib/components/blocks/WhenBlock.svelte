<script lang="ts">
	import JumpTarget from './JumpTarget.svelte';
	import type { WorkflowStepTarget } from '$lib/workflows/targets';
	let {
		label = '', switchVar = '', arms = [], flowNames = [], flowColors = {},
		stepsByFlow = {}, currentFlow = '', expanded = false, compact = false,
		onnavigate = (_: string) => {}, onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {}, onremove = () => {}
	}: {
		label?: string; switchVar?: string;
		arms?: { match?: string; continue?: boolean; goto?: string }[];
		flowNames?: string[]; flowColors?: Record<string, string>;
		stepsByFlow?: Record<string, WorkflowStepTarget[]>; currentFlow?: string;
		expanded?: boolean; compact?: boolean;
		onnavigate?: (target: string) => void;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void; onremove?: () => void;
	} = $props();
	const armColors = ['#22c55e', '#ef4444', '#f97316', '#8b5cf6', '#06b6d4', '#eab308'];
	let editing = $derived(expanded && !compact);
	function updateArm(index: number, updates: Record<string, unknown>) {
		const updated = [...arms];
		updated[index] = { ...updated[index], ...updates };
		onupdate({ arms: updated });
	}
</script>

<div class="group rounded-lg border border-border/60 bg-amber-950/20 border-l-[3px] border-l-amber-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-amber-400">WHEN</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Condition'}</span>
		{#if switchVar}<span title={switchVar} class="max-w-[50%] truncate text-[10px] text-amber-300/70 font-mono">{switchVar}</span>{/if}
		{#if !compact}
			<button onclick={ontoggle} aria-label={expanded ? 'Collapse condition' : 'Expand condition'} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} aria-label="Remove condition" class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>
	<div class="divide-y divide-border/30 border-t border-border/40">
		{#each arms as arm, i}
			<div data-jump-arm={i} class="grid grid-cols-[minmax(5rem,1fr)_minmax(0,2fr)] items-center gap-3 px-3 {compact ? 'py-1' : 'py-2'}">
				<div class="flex min-w-0 items-center gap-1.5">
					<span class="h-2 w-2 shrink-0 rounded-full" style:background={armColors[i % armColors.length]}></span>
					{#if editing}
						<input type="text" aria-label="Match for branch {i + 1}" value={arm.match || 'default'} oninput={e => updateArm(i, { match: e.currentTarget.value })}
							class="min-w-0 flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-semibold uppercase" />
						{#if arms.length > 2}<button aria-label="Remove branch {i + 1}" onclick={() => onupdate({ arms: arms.filter((_, index) => index !== i) })} class="text-xs text-muted-foreground hover:text-destructive">×</button>{/if}
					{:else}<span class="break-words text-xs font-semibold uppercase text-foreground">{arm.match || 'default'}</span>{/if}
				</div>
				<div class="min-w-0 space-y-1 text-right">
					{#if editing}
						<select aria-label="Destination for branch {i + 1}" value={arm.continue ? 'continue' : arm.goto || ''}
							onchange={e => updateArm(i, e.currentTarget.value === 'continue' ? { continue: true, goto: undefined } : { continue: undefined, goto: e.currentTarget.value })}
							class="w-full rounded border border-input bg-background px-1.5 py-1 text-xs">
							<option value="continue">→ continues</option>
							<optgroup label="Go to step (current flow)">
								{#each stepsByFlow[currentFlow] ?? [] as step}<option value="step {step.id}">→ {step.number} ({step.label})</option>{/each}
							</optgroup>
							<optgroup label="Jump to flow">
								{#each flowNames as flow}<option value="flow {flow}">→ {flow}</option>{/each}
							</optgroup>
							{#each flowNames as flow}
								{#if stepsByFlow[flow]?.length}<optgroup label="Jump to {flow} → step">
									{#each stepsByFlow[flow] as step}<option value="flow {flow} step {step.id}">{flow} → {step.number} ({step.label})</option>{/each}
								</optgroup>{/if}
							{/each}
							<option value="">→ (custom target)</option>
						</select>
						{#if !arm.continue && arm.goto && !arm.goto.startsWith('step ') && !arm.goto.startsWith('flow ')}
							<input type="text" value={arm.goto} oninput={e => updateArm(i, { goto: e.currentTarget.value })} class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-xs font-mono" placeholder="flow X / step Y / workflow Z" />
						{/if}
					{/if}
					{#if !arm.continue && arm.goto}
						<JumpTarget target={arm.goto} {currentFlow} {flowColors} {stepsByFlow} {onnavigate} />
					{:else}<span class="text-xs text-muted-foreground">→ continues</span>{/if}
				</div>
			</div>
		{/each}
	</div>
	{#if editing}
		<div class="px-3 py-2"><button onclick={() => onupdate({ arms: [...arms, { match: 'default', continue: true }] })} class="text-xs text-primary hover:underline">+ Add arm</button></div>
		<div class="grid grid-cols-2 gap-2 border-t border-border/40 px-3 py-2">
			<label class="block text-[10px] font-medium text-muted-foreground">LABEL
				<input type="text" value={label} oninput={e => onupdate({ label: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
			</label>
			<label class="block text-[10px] font-medium text-muted-foreground">SWITCH
				<input type="text" value={switchVar} oninput={e => onupdate({ switchVar: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="@step.field" />
			</label>
		</div>
	{/if}
</div>
