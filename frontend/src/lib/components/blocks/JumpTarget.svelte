<script lang="ts">
	import { parseWorkflowTarget, type WorkflowStepTarget } from '$lib/workflows/targets';
	let { target, currentFlow, flowColors, stepsByFlow, onnavigate }: {
		target: string;
		currentFlow: string;
		flowColors: Record<string, string>;
		stepsByFlow: Record<string, WorkflowStepTarget[]>;
		onnavigate: (target: string) => void;
	} = $props();
	let destination = $derived(parseWorkflowTarget(target, currentFlow));
	let step = $derived(destination?.step ? stepsByFlow[destination.flow]?.find(step => step.id === destination?.step) : null);
	let exists = $derived(Boolean(destination && stepsByFlow[destination.flow] && (!destination.step || step)));
	let label = $derived(destination
		? [target.trim().startsWith('flow ') ? destination.flow : '', destination.step ? step ? `${step.number} · ${step.label}` : destination.step : ''].filter(Boolean).join(' → ')
		: target);
</script>

{#if destination}
	<button type="button" data-workflow-target={target} aria-label="Go to {target}"
		disabled={!exists} onclick={() => onnavigate(target)}
		title={exists ? `Go to ${target}` : `Target not found: ${target}`}
		class="inline-flex max-w-full items-center gap-1.5 rounded border border-border/70 bg-background/40 px-2 py-1 text-left text-xs font-medium text-foreground hover:border-primary/60 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-50">
		<span aria-hidden="true">→</span>
		<span class="h-1.5 w-1.5 shrink-0 rounded-full" style:background={flowColors[destination.flow] || '#818cf8'}></span>
		<span class="min-w-0 break-words">{label}</span>
		{#if !exists}<span class="text-muted-foreground">(missing)</span>{/if}
	</button>
{:else}
	<span class="text-xs text-muted-foreground">→ {target}</span>
{/if}
