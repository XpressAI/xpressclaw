<script lang="ts">
	import type { Agent } from '$lib/api';

	let {
		label = '', agent = '', event = 'github.pull_request.activity', resource = '', timeout = '14d', onTimeout = '',
		expanded = false, compact = false, agentList = [], agentRoles = [], flowNames = [],
		onupdate = (_: Record<string, unknown>) => {}, ontoggle = () => {}, onremove = () => {},
	}: {
		label?: string; agent?: string; event?: string; resource?: string; timeout?: string; onTimeout?: string;
		expanded?: boolean; compact?: boolean; agentList?: Agent[];
		agentRoles?: { name: string; description?: string }[]; flowNames?: string[];
		onupdate?: (updates: Record<string, unknown>) => void; ontoggle?: () => void; onremove?: () => void;
	} = $props();
</script>

<div class="group rounded-lg border border-border/60 bg-card border-l-[3px] border-l-cyan-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-cyan-400">WAIT</span>
		<span class="flex-1 truncate text-sm font-medium text-foreground">{label || 'Wait for event'}</span>
		<span class="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">{event.replace('github.pull_request.', 'PR ')}</span>
		{#if !compact}
			<button onclick={ontoggle} aria-label={expanded ? 'Collapse wait' : 'Expand wait'} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} aria-label="Remove wait" class="text-muted-foreground/30 opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if expanded && !compact}
		<div class="space-y-3 border-t border-border/40 px-3 py-3">
			<div class="grid gap-2 sm:grid-cols-2">
				<label class="text-[10px] font-medium text-muted-foreground">LABEL
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
				</label>
				<label class="text-[10px] font-medium text-muted-foreground">REPOSITORY CONTEXT
					<select value={agent} onchange={(e) => onupdate({ agent: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
						<option value="">Select agent…</option>
						{#if agentRoles.length > 0}<optgroup label="Run-time roles">{#each agentRoles as role}<option value="@{role.name}">@{role.name}</option>{/each}</optgroup>{/if}
						<optgroup label="Fixed agents">{#each agentList as item}<option value={item.id}>{item.title || item.name}</option>{/each}</optgroup>
					</select>
				</label>
			</div>
			<label class="block text-[10px] font-medium text-muted-foreground">EVENT
				<select value={event} onchange={(e) => onupdate({ event: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
					<option value="github.pull_request.activity">PR review or comment</option>
					<option value="github.pull_request.review">Formal PR review</option>
					<option value="github.pull_request.comment">PR conversation or inline comment</option>
				</select>
			</label>
			<label class="block text-[10px] font-medium text-muted-foreground">PULL REQUEST URL
				<input type="text" value={resource} oninput={(e) => onupdate({ resource: e.currentTarget.value })} placeholder="@mark_ready.pull_request_url" class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 font-mono text-xs" />
			</label>
			<div class="grid gap-2 sm:grid-cols-2">
				<label class="text-[10px] font-medium text-muted-foreground">TIMEOUT <span class="font-normal normal-case">optional</span>
					<input type="text" value={timeout} oninput={(e) => onupdate({ timeout: e.currentTarget.value })} placeholder="14d" class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 font-mono text-xs" />
				</label>
				<label class="text-[10px] font-medium text-muted-foreground">ON TIMEOUT <span class="font-normal normal-case">optional</span>
					<select value={onTimeout} onchange={(e) => onupdate({ onTimeout: e.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
						<option value="">Fail workflow</option>
						{#each flowNames as flow}<option value="flow {flow}">Go to flow: {flow}</option>{/each}
					</select>
				</label>
			</div>
			<p class="text-[10px] leading-relaxed text-muted-foreground">The workflow is persisted and no agent container runs while waiting. GitHub is polled with the selected agent context's project-scoped credential.</p>
		</div>
	{/if}
</div>
