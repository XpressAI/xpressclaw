<script lang="ts">
	import type { AcpCommand, AcpConfigOption, Agent } from '$lib/api';

	let {
		label = '', agent = '', prompt = '', command = '', procedure = '',
		sessionConfig = {}, newSession = false,
		mcpServer = '', mcpTool = '', mcpArguments = '',
		outputs = {},
		expanded = false, compact = false,
		agentList = [],
		agentRoles = [],
		capabilities = { commands: [], configOptions: [], mcpServers: [] },
		onupdate = (_: Record<string, unknown>) => {},
		ontoggle = () => {},
		onremove = () => {},
		onpromptkeydown = (_: KeyboardEvent) => {}
	}: {
		label?: string; agent?: string; prompt?: string; command?: string; procedure?: string;
		sessionConfig?: Record<string, string | boolean>; newSession?: boolean;
		mcpServer?: string; mcpTool?: string; mcpArguments?: string;
		outputs?: Record<string, { type?: string; description?: string }>;
		expanded?: boolean; compact?: boolean;
		agentList?: Agent[];
		agentRoles?: { name: string; description?: string }[];
		capabilities?: { commands: AcpCommand[]; configOptions: AcpConfigOption[]; mcpServers: string[] };
		onupdate?: (updates: Record<string, unknown>) => void;
		ontoggle?: () => void;
		onremove?: () => void;
		onpromptkeydown?: (e: KeyboardEvent) => void;
	} = $props();

	let advertisedCommands = $derived(capabilities.commands);
	let advertisedConfig = $derived(capabilities.configOptions);
	let attachedMcpServers = $derived(capabilities.mcpServers);

	function commandValue(name: string): string {
		return name.startsWith('/') ? name : `/${name}`;
	}

	function selectChoices(option: AcpConfigOption): { value: string; name: string; description?: string | null }[] {
		if (!Array.isArray(option.options)) return [];
		return option.options.flatMap((entry) => 'options' in entry ? entry.options : [entry]);
	}

	function configText(): string {
		return Object.entries(sessionConfig).map(([key, value]) => `${key}=${value}`).join('\n');
	}

	function parseConfig(text: string): Record<string, string | boolean> {
		return Object.fromEntries(text.split('\n').map((line) => line.trim()).filter(Boolean).map((line) => {
			const separator = line.indexOf('=');
			const key = separator === -1 ? line : line.slice(0, separator).trim();
			const raw = separator === -1 ? '' : line.slice(separator + 1).trim();
			const value = raw === 'true' ? true : raw === 'false' ? false : raw;
			return [key, value];
		}));
	}
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
			<button onclick={ontoggle} aria-label={expanded ? 'Collapse step' : 'Expand step'} class="text-muted-foreground hover:text-foreground">
				<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
			</button>
			<button onclick={onremove} aria-label="Remove step" class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity">
				<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
			</button>
		{/if}
	</div>

	{#if expanded && !compact}
		<div class="border-t border-border/40 px-3 py-3 space-y-3">
			<div class="grid grid-cols-2 gap-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">LABEL
						<input type="text" value={label} oninput={(e) => onupdate({ label: e.currentTarget.value })}
							class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
					</label>
				</div>
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">SESSION
					<select value={agent} onchange={(e) => onupdate({ agent: e.currentTarget.value })}
						class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
						<option value="">Select agent...</option>
						{#if agentRoles.length > 0}
							<optgroup label="Run-time roles">
								{#each agentRoles as role}<option value="@{role.name}">@{role.name}{role.description ? ` — ${role.description}` : ''}</option>{/each}
							</optgroup>
						{/if}
						<optgroup label="Fixed agents">
							{#each agentList as a}<option value={a.name}>{a.title || a.name}</option>{/each}
						</optgroup>
					</select>
					</label>
				</div>
			</div>

			<div class="grid grid-cols-[1fr_auto] gap-2">
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">HARNESS COMMAND <span class="font-normal normal-case">optional</span>
					{#if advertisedCommands.length > 0}
						<select value={command} onchange={(e) => onupdate({ command: e.currentTarget.value })}
							class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono">
							<option value="">Send a normal prompt</option>
							{#if command && !advertisedCommands.some((item) => commandValue(item.name) === command)}
								<option value={command}>{command} (custom)</option>
							{/if}
							{#each advertisedCommands as item}<option value={commandValue(item.name)}>{commandValue(item.name)} — {item.description}</option>{/each}
						</select>
					{:else}
						<input type="text" value={command} oninput={(e) => onupdate({ command: e.currentTarget.value })}
							class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="/loop" />
					{/if}
					</label>
				</div>
				<label class="mt-5 flex items-center gap-1.5 text-[10px] text-muted-foreground">
					<input type="checkbox" checked={newSession} onchange={(e) => onupdate({ newSession: e.currentTarget.checked })} />
					NEW SESSION
				</label>
			</div>

			<div>
				<label class="block text-[10px] font-medium text-muted-foreground mb-1">PROMPT
				<textarea value={prompt} oninput={(e) => onupdate({ prompt: e.currentTarget.value })}
					onkeydown={onpromptkeydown}
					rows="3" class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono resize-none"
					placeholder="What should this agent do? Use @step.field for variables"></textarea>
				</label>
			</div>

			{#if advertisedConfig.length > 0}
				<div>
					<div class="block text-[10px] font-medium text-muted-foreground mb-1">HARNESS MODE AND MODEL</div>
					<div class="grid gap-2 sm:grid-cols-2">
						{#each advertisedConfig as option}
							<label class="flex min-w-0 items-center gap-2 rounded border border-input bg-background px-2 py-1.5 text-[10px] text-muted-foreground" title={option.description || option.name}>
								<span class="truncate">{option.name}</span>
								{#if option.type === 'boolean'}
									<input class="ml-auto" type="checkbox" checked={Boolean(sessionConfig[option.id] ?? option.currentValue)}
										onchange={(e) => onupdate({ sessionConfig: { ...sessionConfig, [option.id]: e.currentTarget.checked } })} />
								{:else}
									<select value={String(sessionConfig[option.id] ?? option.currentValue)}
										onchange={(e) => onupdate({ sessionConfig: { ...sessionConfig, [option.id]: e.currentTarget.value } })}
										class="ml-auto max-w-40 bg-transparent text-xs text-foreground outline-none">
										{#each selectChoices(option) as choice}<option value={choice.value}>{choice.name}</option>{/each}
									</select>
								{/if}
							</label>
						{/each}
					</div>
				</div>
			{/if}

			<details>
				<summary class="cursor-pointer text-[10px] font-medium text-muted-foreground">ADVANCED ACP CONFIG</summary>
				<textarea aria-label="Advanced ACP configuration" value={configText()} oninput={(e) => onupdate({ sessionConfig: parseConfig(e.currentTarget.value) })}
					rows="2" class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono resize-none"
					placeholder={'mode=plan\nthought_level=high'}></textarea>
				<p class="mt-1 text-[10px] text-muted-foreground">One advertised option ID and value per line. Use this for adapter-specific controls not shown above.</p>
			</details>

			<details open={Boolean(mcpTool)}>
				<summary class="cursor-pointer text-[10px] font-medium text-muted-foreground">AGENT TOOL REQUEST</summary>
				<p class="mt-1 text-[10px] leading-relaxed text-muted-foreground">The harness is instructed to make this call during its turn. XpressClaw does not invoke or independently verify the tool call.</p>
				<div class="mt-1 grid gap-2 sm:grid-cols-2">
					{#if attachedMcpServers.length > 0}
						<select aria-label="MCP server" value={mcpServer} onchange={(e) => onupdate({ mcpServer: e.currentTarget.value })}
							class="rounded border border-input bg-background px-2 py-1.5 text-xs">
							<option value="">Any attached server</option>
							{#if mcpServer && !attachedMcpServers.includes(mcpServer)}<option value={mcpServer}>{mcpServer} (not attached)</option>{/if}
							{#each attachedMcpServers as server}<option value={server}>{server}</option>{/each}
						</select>
					{:else}
						<input aria-label="MCP server" type="text" value={mcpServer} oninput={(e) => onupdate({ mcpServer: e.currentTarget.value })}
							class="rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="MCP server" />
					{/if}
					<input aria-label="MCP tool" type="text" value={mcpTool} oninput={(e) => onupdate({ mcpTool: e.currentTarget.value })}
						class="rounded border border-input bg-background px-2 py-1.5 text-xs font-mono" placeholder="tool_name" />
				</div>
				<textarea aria-label="MCP tool arguments" value={mcpArguments} oninput={(e) => onupdate({ mcpArguments: e.currentTarget.value })}
					rows="3" class="mt-2 w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono resize-none"
					placeholder={'{"goal":"{{trigger.payload.goal}}"}'}></textarea>
				<p class="mt-1 text-[10px] text-muted-foreground">JSON arguments. The attached server is available inside the selected harness; the native agent performs the tool call as part of its turn.</p>
			</details>

			{#if procedure}
				<div>
					<label class="block text-[10px] font-medium text-muted-foreground mb-1">PROCEDURE
						<input type="text" value={procedure} oninput={(e) => onupdate({ procedure: e.currentTarget.value })}
							class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="procedure-name" />
					</label>
				</div>
			{/if}

			<!-- Outputs -->
			<div>
				<div class="flex items-center justify-between mb-1">
					<div class="text-[10px] font-medium text-muted-foreground">OUTPUT</div>
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
