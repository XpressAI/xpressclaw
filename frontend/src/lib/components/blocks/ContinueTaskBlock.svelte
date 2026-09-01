<script lang="ts">
	let {
		label = '', prompt = '', expanded = false, compact = false,
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {},
	}: {
		label?: string; prompt?: string; expanded?: boolean; compact?: boolean;
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();
</script>

<div class="group rounded-lg border border-border/60 border-l-[3px] border-l-violet-500 bg-card">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-violet-400">CONTINUE TASK</span>
		<span class="flex-1 truncate text-sm font-medium text-foreground">{label || 'Continue task'}</span>
		<span class="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">same task</span>
		{#if !compact}
			<button onclick={ontoggle} aria-label={expanded ? 'Collapse continue task' : 'Expand continue task'} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} aria-label="Remove continue task" class="text-muted-foreground/30 opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if expanded && !compact}
		<div class="space-y-3 border-t border-border/40 px-3 py-3">
			<label class="block text-[10px] font-medium text-muted-foreground">LABEL
				<input type="text" value={label} oninput={(event) => onupdate({ label: event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
			</label>
			<label class="block text-[10px] font-medium text-muted-foreground">FIXED PROMPT
				<textarea aria-label="Fixed prompt" value={prompt} oninput={(event) => onupdate({ prompt: event.currentTarget.value })} rows="4" class="mt-1 w-full resize-y rounded border border-input bg-background px-2 py-1.5 font-mono text-xs" placeholder="What should the agent check before this task is finished?"></textarea>
			</label>
			<p class="text-[10px] leading-relaxed text-muted-foreground">After the current task has finished and no response is queued, this prompt is sent once in that task's existing conversation.</p>
		</div>
	{/if}
</div>
