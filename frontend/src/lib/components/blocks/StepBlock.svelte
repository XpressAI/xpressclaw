<script lang="ts">
	import type { Agent } from '$lib/api';

	let {
		label = '', agent = '', prompt = '', procedure = '',
		outputs = {},
		expanded = false, compact = false,
		agentList = [],
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {}
	}: {
		label?: string; agent?: string; prompt?: string; procedure?: string;
		outputs?: Record<string, { type?: string; description?: string }>;
		expanded?: boolean; compact?: boolean;
		agentList?: Agent[];
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
	} = $props();
</script>

<div class="group rounded-lg border border-border/60 bg-card border-l-[3px] border-l-blue-500">
	<!-- Header -->
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-blue-400">STEP</span>
		<span class="text-sm font-medium text-foreground flex-1 truncate">{label || 'Untitled'}</span>
		{#if agent}
			<span class="text-[10px] text-muted-foreground bg-muted rounded px-1.5 py-0.5">{agent}</span>
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
		<div class="border-t border-border/40 px-3 py-3 space-y-3">
			<div class="grid grid-cols-2 gap-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL</label>
					<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
				</div>
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">AGENT</label>
					<select value={agent} onchange={(e) => onupdate({ agent: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
						<option value="">Select agent...</option>
						{#each agentList as a}<option value={a.name}>{a.config?.display_name || a.name}</option>{/each}
					</select>
				</div>
			</div>

			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">PROMPT</label>
				<textarea value={prompt} oninput={(e) => onupdate({ prompt: e.currentTarget.value })}
					rows="3" class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono resize-none"
					placeholder="What should this agent do? Use @step.field for variables"></textarea>
			</div>

			{#if procedure}
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">PROCEDURE</label>
					<input type="text" value={procedure} oninput={(e) => onupdate({ procedure: e.currentTarget.value })}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="procedure-name" />
				</div>
			{/if}

			<!-- Outputs -->
			<div>
				<div class="flex items-center justify-between mb-1">
					<label class="text-[10px] font-medium text-muted-foreground">OUTPUT</label>
					<button onclick={() => {
						const o = { ...outputs, [`output_${Date.now().toString(36)}`]: { type: 'string', description: '' } };
						onupdate({ outputs: o });
					}} class="text-[10px] text-primary hover:underline">+ Add</button>
				</div>
				{#each Object.entries(outputs) as [name, schema]}
					<div class="flex items-center gap-1.5 mb-1">
						<span class="text-amber-400 text-xs font-mono">{'{'}</span>
						<input type="text" value={name}
							oninput={(e) => {
								const o = { ...outputs };
								const val = o[name];
								delete o[name];
								o[e.currentTarget.value] = val;
								onupdate({ outputs: o });
							}}
							class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-mono" placeholder="field_name" />
						<select value={schema.type || 'string'}
							onchange={(e) => { onupdate({ outputs: { ...outputs, [name]: { ...schema, type: e.currentTarget.value } } }); }}
							class="rounded border border-input bg-background px-1 py-0.5 text-[10px]">
							<option value="string">string</option>
							<option value="number">number</option>
							<option value="boolean">boolean</option>
							<option value="array">array</option>
							<option value="object">object</option>
						</select>
						<span class="text-amber-400 text-xs font-mono">{'}'}</span>
						<button onclick={() => {
							const o = { ...outputs };
							delete o[name];
							onupdate({ outputs: o });
						}} class="text-muted-foreground/40 hover:text-destructive text-xs">x</button>
					</div>
				{/each}
			</div>
		</div>
	{/if}
</div>
