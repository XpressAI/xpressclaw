<script lang="ts">
	import type { Agent } from '$lib/api';

	let {
		label = '', agent = '', prompt = '', procedure = '',
		outputs = {},
		expanded = false, compact = false,
		agentList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {},
		onpromptkeydown = (_: KeyboardEvent) => {}
	}: {
		label?: string; agent?: string; prompt?: string; procedure?: string;
		outputs?: Record<string, { type?: string; description?: string }>;
		expanded?: boolean; compact?: boolean;
		agentList?: Agent[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
		onpromptkeydown?: (e: KeyboardEvent) => void;
	} = $props();

	let outputEntries = $derived(Object.entries(outputs));
	let detailText = $derived(
		[agent, ...outputEntries.map(([n]) => `{${n}}`)].filter(Boolean).join(' · ')
	);
</script>

{#if compact}
	<div class="flex items-center gap-2 px-1 py-1.5">
		<span class="rounded bg-blue-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">STEP</span>
		<span class="text-sm text-foreground flex-1 truncate">{label || 'Untitled'}</span>
		<span class="text-xs text-muted-foreground font-mono truncate max-w-[40%] text-right">{detailText}</span>
	</div>
{:else}
	<div class="group">
		<div class="flex items-center gap-2 px-1 py-1.5">
			<span class="rounded bg-blue-600 px-1.5 py-0.5 text-[9px] font-bold text-white leading-none">STEP</span>
			<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Untitled'}</span>
			<span class="text-xs text-muted-foreground font-mono">{detailText}</span>
			<button onclick={ontoggle} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		</div>

		{#if expanded}
			<div class="px-1 pb-3 pt-1 space-y-2">
				<div class="flex gap-2">
					<div class="flex-1">
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">AGENT</label>
						<div class="rounded bg-muted px-2.5 py-1.5">
							<select value={agent} onchange={(e) => onupdate({ agent: e.currentTarget.value })}
								class="w-full bg-transparent text-xs text-foreground focus:outline-none">
								<option value="">Select agent...</option>
								{#each agentList as a}<option value={a.name}>{a.config?.display_name || a.name}</option>{/each}
							</select>
						</div>
					</div>
					{#if outputEntries.length > 0}
						<div class="flex-1">
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">OUTPUT</label>
							<div class="rounded bg-muted px-2.5 py-1.5 text-xs font-mono text-foreground">
								{outputEntries.map(([n]) => `{${n}}`).join(', ')}
							</div>
						</div>
					{/if}
				</div>

				<div>
					<div class="flex items-center justify-between mb-1">
						<label class="text-[10px] font-medium text-muted-foreground">PROMPT</label>
					</div>
					<div class="rounded bg-muted px-2.5 py-2">
						<textarea value={prompt} oninput={(e) => onupdate({ prompt: e.currentTarget.value })}
							onkeydown={onpromptkeydown}
							rows="3" class="w-full bg-transparent text-xs font-mono text-foreground resize-none focus:outline-none"
							placeholder="What should this agent do?"></textarea>
					</div>
				</div>

				<!-- Editable label (collapsed into header normally) -->
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1 text-xs" />
				</div>

				<!-- Output editing -->
				<div>
					<div class="flex items-center justify-between mb-1">
						<label class="text-[10px] font-medium text-muted-foreground">OUTPUTS</label>
						<button onclick={() => {
							const o = { ...outputs, [`out_${Date.now().toString(36)}`]: { type: 'string', description: '' } };
							onupdate({ outputs: o });
						}} class="text-[10px] text-primary hover:underline">+ Add</button>
					</div>
					{#each outputEntries as [name, schema]}
						<div class="flex items-center gap-1.5 mb-1">
							<input type="text" value={name}
								oninput={(e) => {
									const o = { ...outputs }; const val = o[name]; delete o[name]; o[e.currentTarget.value] = val;
									onupdate({ outputs: o });
								}}
								class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-mono" />
							<select value={schema.type || 'string'}
								onchange={(e) => { onupdate({ outputs: { ...outputs, [name]: { ...schema, type: e.currentTarget.value } } }); }}
								class="rounded border border-input bg-background px-1 py-0.5 text-[10px]">
								<option value="string">string</option><option value="number">number</option><option value="boolean">boolean</option><option value="array">array</option><option value="object">object</option>
							</select>
							<button onclick={() => { const o = { ...outputs }; delete o[name]; onupdate({ outputs: o }); }}
								class="text-muted-foreground/40 hover:text-destructive text-xs">x</button>
						</div>
					{/each}
				</div>
			</div>
		{/if}
	</div>
{/if}
