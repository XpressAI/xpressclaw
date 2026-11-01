<script lang="ts">
	import type { Agent } from '$lib/api';
	let { label = '', agent = '', command = '', cwd = '.', timeout = '5m', allowFailure = false,
		expanded = false, compact = false, agentList = [], agentRoles = [],
		onupdate = (_: Record<string, unknown>) => {}, ontoggle = () => {}, onremove = () => {},
	}: {
		label?: string; agent?: string; command?: string; cwd?: string; timeout?: string; allowFailure?: boolean;
		expanded?: boolean; compact?: boolean; agentList?: Agent[]; agentRoles?: { name: string; description?: string }[];
		onupdate?: (update: Record<string, unknown>) => void; ontoggle?: () => void; onremove?: () => void;
	} = $props();
</script>

<div class="rounded-lg border border-teal-500/30 bg-card" data-command-block>
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-teal-400">COMMAND</span>
		<span class="flex-1 truncate text-sm font-medium">{label || 'Run command'}</span>
		<span class="text-[10px] text-muted-foreground">{allowFailure ? 'Capture failure' : 'Require success'}</span>
		{#if !compact}
			<button type="button" onclick={ontoggle} aria-label={expanded ? 'Collapse command' : 'Expand command'} class="text-xs text-muted-foreground hover:text-foreground">{expanded ? 'Collapse' : 'Edit'}</button>
			<button type="button" onclick={onremove} aria-label="Remove command" class="text-xs text-muted-foreground hover:text-destructive">Remove</button>
		{/if}
	</div>
	{#if expanded && !compact}
		<div class="space-y-3 border-t border-border px-3 py-3 text-xs">
			<label class="block">Agent environment
				<select aria-label="Command Agent" value={agent} onchange={(event) => onupdate({ agent: event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background p-2">
					<option value="">Choose an Agent or role</option>
					{#each agentRoles as role}<option value={'@' + role.name}>@{role.name}</option>{/each}
					{#each agentList as item}<option value={item.id}>{item.name}</option>{/each}
				</select>
			</label>
			<label class="block">Shell command
				<textarea aria-label="Shell command" rows="4" value={command} oninput={(event) => onupdate({ command: event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background p-2 font-mono"></textarea>
			</label>
			<div class="grid grid-cols-2 gap-3">
				<label>Working directory<input aria-label="Command working directory" value={cwd} oninput={(event) => onupdate({ cwd: event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background p-2 font-mono" /></label>
				<label>Timeout<input aria-label="Command timeout" value={timeout} oninput={(event) => onupdate({ timeout: event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background p-2 font-mono" /></label>
			</div>
			<label class="flex items-center gap-2"><input type="checkbox" checked={allowFailure} onchange={(event) => onupdate({ allowFailure: event.currentTarget.checked })} />Continue after a nonzero exit for review</label>
			<p class="text-muted-foreground">Runs in the Agent’s workspace. Later steps receive exit_code, stdout, stderr, signal, timed_out, and output truncation flags.</p>
		</div>
	{/if}
</div>
