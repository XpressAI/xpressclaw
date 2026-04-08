<script lang="ts">
	import { workflows, agents, connectors } from '$lib/api';
	import type { Workflow, WorkflowInstance, Agent, Connector } from '$lib/api';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import yaml from 'js-yaml';
	import StepBlock from '$lib/components/blocks/StepBlock.svelte';
	import WhenBlock from '$lib/components/blocks/WhenBlock.svelte';
	import LoopBlock from '$lib/components/blocks/LoopBlock.svelte';
	import SinkBlock from '$lib/components/blocks/SinkBlock.svelte';
	import JumpBlock from '$lib/components/blocks/JumpBlock.svelte';
	import TriggerBlock from '$lib/components/blocks/TriggerBlock.svelte';
	import BlockConnector from '$lib/components/blocks/BlockConnector.svelte';
	import VariablePopup from '$lib/components/blocks/VariablePopup.svelte';
	import JumpArrows from '$lib/components/blocks/JumpArrows.svelte';

	// --- Types ---

	interface OutputSchema { type?: string; description?: string }
	interface SinkCfg { connector: string; channel: string; template?: string }
	interface WhenArmDef { match?: string; continue?: boolean; goto?: string }

	interface Block {
		id: string;
		type: 'trigger' | 'step' | 'when' | 'loop' | 'sink' | 'jump';
		label: string;
		agent?: string; prompt?: string; procedure?: string;
		outputs?: Record<string, OutputSchema>;
		connector?: string; channel?: string; event?: string;
		filter?: Record<string, unknown>;
		sinks?: SinkCfg[];
		switchVar?: string;
		arms?: WhenArmDef[];
		overVar?: string; asVar?: string;
		children?: Block[];
		target?: string;
		expanded: boolean;
	}

	interface FlowDef { color: string; blocks: Block[] }

	// --- State ---

	let workflow = $state<Workflow | null>(null);
	let workflowName = $state('');
	let saving = $state(false);
	let running = $state(false);
	let showYaml = $state(false);
	let yamlContent = $state('');
	let agentList = $state<Agent[]>([]);
	let connectorList = $state<Connector[]>([]);
	let instances = $state<WorkflowInstance[]>([]);
	let showInstances = $state(false);
	let toast = $state<{ message: string; type: 'success' | 'error' } | null>(null);
	let compactView = $state(false);

	let flows = $state<Record<string, FlowDef>>({});
	let currentFlow = $state('main');
	let triggerConfig = $state<{ connector: string; channel: string; event: string; filter?: Record<string, unknown> } | null>(null);
	let globalVars = $state<Record<string, unknown>>({});

	let variablePopup = $state<{ x: number; y: number; filter: string; target: HTMLTextAreaElement | null } | null>(null);

	let currentBlocks = $derived(flows[currentFlow]?.blocks ?? []);
	let flowNames = $derived(Object.keys(flows));
	let flowColors = $derived(Object.fromEntries(Object.entries(flows).map(([k, v]) => [k, v.color])));

	// --- YAML ↔ Flows ---

	interface YamlDef {
		name?: string; description?: string; version?: number;
		trigger?: { connector: string; channel: string; event: string; filter?: Record<string, unknown> };
		variables?: Record<string, unknown>;
		flows?: Record<string, { color?: string; steps?: any[] }>;
	}

	function yamlToFlows(yamlStr: string): { flows: Record<string, FlowDef>; trigger: typeof triggerConfig; variables: Record<string, unknown> } {
		let def: YamlDef;
		try { def = yaml.load(yamlStr) as YamlDef; } catch { return { flows: { main: { color: '#22c55e', blocks: [] } }, trigger: null, variables: {} }; }
		if (!def?.flows) return { flows: { main: { color: '#22c55e', blocks: [] } }, trigger: null, variables: {} };

		const result: Record<string, FlowDef> = {};
		for (const [name, flow] of Object.entries(def.flows)) {
			result[name] = {
				color: flow.color || (name === 'main' ? '#22c55e' : name === 'on_error' ? '#ef4444' : '#8b5cf6'),
				blocks: (flow.steps || []).map(stepToBlock)
			};
		}
		return { flows: result, trigger: def.trigger || null, variables: def.variables || {} };
	}

	function stepToBlock(s: any): Block {
		const type = s.type || 'step';
		const block: Block = { id: s.id, type, label: s.label || s.id, expanded: false };
		if (type === 'step') { block.agent = s.agent; block.prompt = s.prompt; block.procedure = s.procedure; block.outputs = s.outputs; }
		if (type === 'sink') { block.sinks = s.sinks; }
		if (type === 'when') { block.switchVar = s.switch; block.arms = s.arms; }
		if (type === 'loop') { block.overVar = s.over; block.asVar = s.as; block.children = (s.steps || s.body || []).map(stepToBlock); }
		if (type === 'jump') { block.target = s.target; }
		return block;
	}

	function flowsToYaml(): string {
		const def: Record<string, unknown> = {
			name: workflowName || 'workflow',
			description: workflow?.description || '',
			version: 1,
		};
		if (triggerConfig?.connector) def.trigger = triggerConfig;
		if (Object.keys(globalVars).length > 0) def.variables = globalVars;

		const flowsOut: Record<string, unknown> = {};
		for (const [name, flow] of Object.entries(flows)) {
			flowsOut[name] = { color: flow.color, steps: flow.blocks.map(blockToStep) };
		}
		def.flows = flowsOut;
		return yaml.dump(def, { lineWidth: -1, noRefs: true, quotingType: '"' });
	}

	function blockToStep(b: Block): Record<string, unknown> {
		const s: Record<string, unknown> = { id: b.id, type: b.type, label: b.label };
		if (b.type === 'step') { if (b.agent) s.agent = b.agent; if (b.prompt) s.prompt = b.prompt; if (b.procedure) s.procedure = b.procedure; if (b.outputs && Object.keys(b.outputs).length) s.outputs = b.outputs; }
		if (b.type === 'sink' && b.sinks?.length) s.sinks = b.sinks;
		if (b.type === 'when') { if (b.switchVar) s.switch = b.switchVar; if (b.arms?.length) s.arms = b.arms; }
		if (b.type === 'loop') { if (b.overVar) s.over = b.overVar; if (b.asVar) s.as = b.asVar; if (b.children?.length) s.steps = b.children.map(blockToStep); }
		if (b.type === 'jump' && b.target) s.target = b.target;
		return s;
	}

	// --- Data loading ---

	onMount(async () => {
		const id = $page.params.id!;
		try {
			const [wf, al, cl] = await Promise.all([
				workflows.get(id), agents.list().catch(() => []), connectors.list().catch(() => [])
			]);
			workflow = wf; workflowName = wf.name; yamlContent = wf.yaml_content; agentList = al; connectorList = cl;
			const parsed = yamlToFlows(wf.yaml_content);
			flows = parsed.flows; triggerConfig = parsed.trigger; globalVars = parsed.variables;
			if (!flows.main) { flows = { main: { color: '#22c55e', blocks: [] }, ...flows }; }
			instances = await workflows.instances(id).catch(() => []);
		} catch (e) { showToast(`Failed to load: ${e}`, 'error'); }
	});

	// --- Actions ---

	function showToast(message: string, type: 'success' | 'error') {
		toast = { message, type }; setTimeout(() => { toast = null; }, 3000);
	}

	async function save() {
		if (!workflow) return;
		saving = true;
		try {
			const y = flowsToYaml();
			yamlContent = y;
			await workflows.update(workflow.id, { yaml_content: y, description: workflow.description ?? undefined });
			workflow = await workflows.get(workflow.id);
			showToast('Saved', 'success');
		} catch (e) { showToast(`Save failed: ${e}`, 'error'); }
		saving = false;
	}

	async function toggleEnabled() {
		if (!workflow) return;
		try { workflow = workflow.enabled ? await workflows.disable(workflow.id) : await workflows.enable(workflow.id); }
		catch (e) { showToast(String(e), 'error'); }
	}

	async function runWorkflow() {
		if (!workflow) return;
		running = true;
		try {
			await workflows.run(workflow.id);
			showToast('Instance started', 'success');
			showInstances = true; await loadInstances();
		} catch (e) { showToast(`Run failed: ${e}`, 'error'); }
		running = false;
	}

	async function loadInstances() {
		if (!workflow) return;
		try { instances = await workflows.instances(workflow.id); } catch { instances = []; }
	}

	function applyYaml() {
		const parsed = yamlToFlows(yamlContent);
		flows = parsed.flows; triggerConfig = parsed.trigger; globalVars = parsed.variables;
		showYaml = false;
	}

	// --- Flow management ---

	function addFlow() {
		const name = prompt('Sub-workflow name (e.g. on_rejected):');
		if (!name || flows[name]) return;
		flows = { ...flows, [name]: { color: '#8b5cf6', blocks: [] } };
		currentFlow = name;
	}

	function removeFlow(name: string) {
		if (name === 'main') return;
		const { [name]: _, ...rest } = flows;
		flows = rest;
		if (currentFlow === name) currentFlow = 'main';
	}

	// --- Block manipulation ---

	function slugify(label: string): string {
		return label.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '') || `step_${Date.now().toString(36)}`;
	}

	function nextStepNumber(): number {
		let count = 0;
		for (const flow of Object.values(flows)) {
			count += flow.blocks.filter(b => b.type === 'step').length;
		}
		return count + 1;
	}

	function addBlock(type: Block['type'], afterIdx?: number) {
		let label: string;
		let id: string;
		switch (type) {
			case 'step': label = `Step ${nextStepNumber()}`; break;
			case 'when': label = 'Condition'; break;
			case 'loop': label = 'For Each'; break;
			case 'sink': label = 'Notify'; break;
			case 'jump': label = 'Jump'; break;
			default: return;
		}
		id = slugify(label);
		// Ensure unique ID
		const allIds = new Set(Object.values(flows).flatMap(f => f.blocks.map(b => b.id)));
		while (allIds.has(id)) { id = `${slugify(label)}_${Date.now().toString(36)}`; }

		let block: Block;
		switch (type) {
			case 'step': block = { id, type, label, agent: '', prompt: '', expanded: true }; break;
			case 'when': block = { id, type, label, switchVar: '', expanded: true,
				arms: [{ match: 'approved', continue: true }, { match: 'rejected', goto: 'step ' }] }; break;
			case 'loop': block = { id, type, label, overVar: '', asVar: 'item', children: [], expanded: true }; break;
			case 'sink': block = { id, type, label, sinks: [{ connector: '', channel: '', template: '' }], expanded: true }; break;
			case 'jump': block = { id, type, label, target: '', expanded: true }; break;
			default: return;
		}
		const blocks = [...(flows[currentFlow]?.blocks ?? [])];
		const idx = afterIdx !== undefined ? afterIdx + 1 : blocks.length;
		blocks.splice(idx, 0, block);
		flows = { ...flows, [currentFlow]: { ...flows[currentFlow], blocks } };
	}

	function addTrigger() {
		if (triggerConfig) { showToast('Trigger already exists', 'error'); return; }
		triggerConfig = { connector: '', channel: '', event: '' };
	}

	function removeTrigger() { triggerConfig = null; }

	function updateBlock(flowName: string, idx: number, updates: Record<string, unknown>) {
		const blocks = [...(flows[flowName]?.blocks ?? [])];
		blocks[idx] = { ...blocks[idx], ...updates } as Block;
		flows = { ...flows, [flowName]: { ...flows[flowName], blocks } };
	}

	function removeBlock(flowName: string, idx: number) {
		const blocks = (flows[flowName]?.blocks ?? []).filter((_, i) => i !== idx);
		flows = { ...flows, [flowName]: { ...flows[flowName], blocks } };
	}

	function moveBlock(flowName: string, fromIdx: number, toIdx: number) {
		if (fromIdx === toIdx) return;
		const blocks = [...(flows[flowName]?.blocks ?? [])];
		const [item] = blocks.splice(fromIdx, 1);
		blocks.splice(toIdx > fromIdx ? toIdx - 1 : toIdx, 0, item);
		flows = { ...flows, [flowName]: { ...flows[flowName], blocks } };
	}

	// --- Step numbering ---

	function computeStepNumbers(blocks: Block[], prefix = ''): { id: string; number: string; label: string }[] {
		const result: { id: string; number: string; label: string }[] = [];
		let num = 1;
		for (const b of blocks) {
			if (b.type === 'trigger') continue;
			const n = prefix ? `${prefix}.${String.fromCharCode(96 + num)}` : String(num).padStart(2, '0');
			result.push({ id: b.id, number: n, label: b.label });
			if (b.type === 'loop' && b.children) {
				result.push(...computeStepNumbers(b.children, n));
			}
			num++;
		}
		return result;
	}

	let stepNumbers = $derived(computeStepNumbers(currentBlocks));
	function stepNum(id: string): string { return stepNumbers.find(s => s.id === id)?.number ?? ''; }

	// All step IDs across flows for when block goto targets
	let allStepIds = $derived(
		Object.entries(flows).flatMap(([, flow]) =>
			computeStepNumbers(flow.blocks)
		)
	);

	// --- Variables ---

	function availableVariables(upToIdx: number): { name: string; type?: string; source?: string }[] {
		const vars: { name: string; type?: string; source?: string }[] = [];
		if (triggerConfig) vars.push({ name: 'trigger.payload', type: 'object', source: 'Trigger' });
		for (const [k, v] of Object.entries(globalVars)) {
			vars.push({ name: k, type: typeof v, source: 'Global' });
		}
		const blocks = flows[currentFlow]?.blocks ?? [];
		for (let i = 0; i < upToIdx && i < blocks.length; i++) {
			const b = blocks[i];
			if (b.outputs) {
				for (const [name, schema] of Object.entries(b.outputs)) {
					vars.push({ name: `${b.id}.${name}`, type: schema.type || 'any', source: b.label });
				}
			}
		}
		return vars;
	}

	function handlePromptKeydown(e: KeyboardEvent, blockIdx: number) {
		if (e.key === '@') {
			const ta = e.target as HTMLTextAreaElement;
			const rect = ta.getBoundingClientRect();
			variablePopup = { x: rect.left + 20, y: rect.bottom, filter: '', target: ta };
		}
	}

	function insertVariable(name: string) {
		if (!variablePopup?.target) return;
		const ta = variablePopup.target;
		const pos = ta.selectionStart;
		const before = ta.value.slice(0, pos);
		const after = ta.value.slice(pos);
		ta.value = `${before}@${name}${after}`;
		ta.selectionStart = ta.selectionEnd = pos + name.length + 1;
		ta.dispatchEvent(new Event('input', { bubbles: true }));
		variablePopup = null;
		ta.focus();
	}

	// --- Drag ---
	let dragIdx = $state<number | null>(null);
	let dragOverIdx = $state<number | null>(null);
	let scrollContainerEl = $state<HTMLElement | null>(null);

	// Compute jump arrows from when arms and jump blocks
	let jumpArrows = $derived((() => {
		const arrows: { fromId: string; toId: string; color: string; label?: string; side: 'right' | 'left' }[] = [];
		const blocks = currentBlocks;
		for (const block of blocks) {
			if (block.type === 'when' && block.arms) {
				for (const arm of block.arms) {
					if (arm.goto?.startsWith('step ')) {
						const targetId = arm.goto.replace('step ', '');
						arrows.push({ fromId: block.id, toId: targetId, color: '#f59e0b', label: arm.match || '', side: 'right' });
					} else if (arm.goto?.startsWith('flow ')) {
						const flowName = arm.goto.replace('flow ', '').split(' ')[0];
						const color = flowColors[flowName] || '#8b5cf6';
						arrows.push({ fromId: block.id, toId: block.id, color, label: `→ ${flowName}`, side: 'right' });
					}
				}
			}
			if (block.type === 'jump' && block.target?.startsWith('step ')) {
				const targetId = block.target.replace('step ', '');
				arrows.push({ fromId: block.id, toId: targetId, color: '#818cf8', label: '', side: 'right' });
			}
		}
		return arrows;
	})());
</script>

<div class="flex h-full flex-col overflow-hidden">
	<!-- Toolbar -->
	<div class="flex items-center gap-3 border-b border-border bg-card px-4 py-2 flex-shrink-0">
		<a href="/workflows" class="text-muted-foreground hover:text-foreground" title="Back">
			<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" /></svg>
		</a>
		<span class="text-xs text-muted-foreground/50">/</span>
		<input type="text" bind:value={workflowName}
			class="border-none bg-transparent text-sm font-semibold text-foreground focus:outline-none w-48" placeholder="Workflow name" />
		<div class="flex-1"></div>

		{#if workflow}
			<label class="relative inline-flex items-center cursor-pointer shrink-0" title={workflow.enabled ? 'Disable' : 'Enable'}>
				<input type="checkbox" checked={workflow.enabled} onchange={toggleEnabled} class="sr-only peer" />
				<div class="w-8 h-[18px] bg-muted rounded-full peer peer-checked:bg-emerald-600 transition-colors after:content-[''] after:absolute after:top-[2px] after:start-[2px] after:bg-white after:rounded-full after:h-3.5 after:w-3.5 after:transition-all peer-checked:after:translate-x-full"></div>
			</label>
		{/if}

		<button onclick={() => { showInstances = !showInstances; if (showInstances) loadInstances(); }}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {showInstances ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'} flex items-center gap-1.5">
			Runs{#if instances.length > 0}<span class="rounded-full bg-muted px-1.5 text-[10px]">{instances.length}</span>{/if}
		</button>
		<button onclick={() => { showYaml = !showYaml; if (showYaml) yamlContent = flowsToYaml(); }}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {showYaml ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}">YAML</button>
		<button onclick={() => (compactView = !compactView)}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {compactView ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}">{compactView ? 'Full' : 'Compact'}</button>
		<button onclick={runWorkflow} disabled={running}
			class="rounded-md bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700 disabled:opacity-50 flex items-center gap-1.5">
			<svg class="h-3 w-3" fill="currentColor" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg>
			{running ? 'Running...' : 'Run'}
		</button>
		<button onclick={save} disabled={saving}
			class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving...' : 'Save'}</button>
	</div>

	<!-- Sub-workflow tabs -->
	<div class="flex items-center gap-1 border-b border-border bg-card/50 px-4 py-1.5 flex-shrink-0 overflow-x-auto">
		{#each Object.entries(flows) as [name, flow]}
			<button onclick={() => (currentFlow = name)}
				class="flex items-center gap-1.5 rounded-md px-3 py-1 text-xs transition-colors {currentFlow === name ? 'bg-accent text-foreground font-medium' : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'}">
				<span class="h-2 w-2 rounded-full flex-shrink-0" style="background: {flow.color}"></span>
				<span>{name}</span>
				<span class="text-[10px] text-muted-foreground/60">{flow.blocks.length}</span>
				{#if name !== 'main'}
					<button onclick={(e) => { e.stopPropagation(); if (confirm(`Delete flow "${name}"?`)) removeFlow(name); }}
						class="ml-0.5 text-muted-foreground/30 hover:text-destructive">
						<svg class="h-2.5 w-2.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
					</button>
				{/if}
			</button>
		{/each}
		<button onclick={addFlow} class="rounded-md px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-accent/50" title="Add sub-workflow">+</button>
	</div>

	<!-- Main content -->
	<div class="flex-1 overflow-y-auto relative" bind:this={scrollContainerEl}>
		{#if showYaml}
			<div class="absolute inset-0 z-20 flex flex-col bg-background/95 backdrop-blur-sm">
				<div class="flex items-center justify-between px-4 py-2 border-b border-border">
					<span class="text-xs font-medium text-muted-foreground">YAML Editor</span>
					<div class="flex gap-2">
						<button onclick={applyYaml} class="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90">Apply</button>
						<button onclick={() => (showYaml = false)} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Close</button>
					</div>
				</div>
				<textarea bind:value={yamlContent} class="flex-1 w-full bg-transparent p-4 font-mono text-xs text-foreground resize-none focus:outline-none" spellcheck="false"></textarea>
			</div>
		{/if}

		<!-- Column header -->
		<div class="max-w-3xl mx-auto px-4">
			<div class="flex items-center text-[10px] font-medium text-muted-foreground/50 uppercase tracking-wider pt-3 pb-1 px-1">
				<span class="w-10">Line</span>
				<span class="flex-1">Block</span>
			</div>
		</div>

		<!-- Block list -->
		<div class="max-w-3xl mx-auto px-4 pb-6">
			<!-- Trigger (shown in main flow only) -->
			{#if currentFlow === 'main'}
				{#if triggerConfig}
					<div class="flex items-start gap-0">
						<div class="w-10 pt-2.5 text-right pr-3 text-xs font-mono text-muted-foreground/40">01</div>
						<div class="flex-1">
							<TriggerBlock
								connector={triggerConfig.connector} channel={triggerConfig.channel} event={triggerConfig.event}
								expanded={!compactView} {connectorList}
								onupdate={(u) => { triggerConfig = { ...triggerConfig!, ...u } as typeof triggerConfig; }}
								ontoggle={() => {}}
							/>
						</div>
						<button onclick={removeTrigger} class="mt-2.5 ml-1 text-muted-foreground/20 hover:text-destructive">
							<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
						</button>
					</div>
					<div class="flex"><div class="w-10"></div><BlockConnector /></div>
				{:else}
					<div class="flex items-center gap-0 mb-2">
						<div class="w-10"></div>
						<button onclick={addTrigger}
							class="rounded-lg border border-dashed border-emerald-600/30 hover:border-emerald-500/50 bg-emerald-950/10 hover:bg-emerald-950/20 px-4 py-2 text-xs text-emerald-400/60 hover:text-emerald-300 transition-all flex items-center gap-2">
							<svg class="h-3.5 w-3.5" fill="currentColor" viewBox="0 0 24 24"><path d="M13 2L3 14h9l-1 10 10-12h-9l1-10z" /></svg>
							Add Trigger
						</button>
					</div>
				{/if}
			{/if}

			<!-- Steps -->
			{#each currentBlocks as block, idx (block.id)}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div class="flex items-start gap-0" data-step-id={block.id}
					draggable="true"
					ondragstart={(e) => { e.dataTransfer?.setData('text/plain', String(idx)); dragIdx = idx; }}
					ondragover={(e) => { e.preventDefault(); dragOverIdx = idx; }}
					ondragleave={() => { if (dragOverIdx === idx) dragOverIdx = null; }}
					ondrop={(e) => { e.preventDefault(); dragOverIdx = null; const from = parseInt(e.dataTransfer?.getData('text/plain') || ''); if (!isNaN(from)) moveBlock(currentFlow, from, idx); }}
					ondragend={() => { dragIdx = null; dragOverIdx = null; }}
				>
					<!-- Line number -->
					<div class="w-10 pt-2.5 text-right pr-3 text-xs font-mono text-muted-foreground/40 select-none cursor-grab active:cursor-grabbing">
						{stepNum(block.id)}
					</div>

					<!-- Block -->
					<div class="flex-1 {dragOverIdx === idx ? 'ring-2 ring-primary/30 rounded-lg' : ''}">
						{#if block.type === 'step'}
							<StepBlock
								label={block.label} agent={block.agent || ''} prompt={block.prompt || ''} procedure={block.procedure || ''}
								outputs={block.outputs || {}}
								expanded={block.expanded} compact={compactView}
								{agentList}
								onupdate={(u) => updateBlock(currentFlow, idx, u)}
								ontoggle={() => updateBlock(currentFlow, idx, { expanded: !block.expanded })}
								onremove={() => removeBlock(currentFlow, idx)}
								onpromptkeydown={(e) => handlePromptKeydown(e, idx)}
							/>
						{:else if block.type === 'when'}
							<WhenBlock
								label={block.label} switchVar={block.switchVar || ''}
								arms={block.arms || []}
								{flowNames} {flowColors}
								stepIds={allStepIds}
								expanded={block.expanded} compact={compactView}
								onupdate={(u) => updateBlock(currentFlow, idx, u)}
								ontoggle={() => updateBlock(currentFlow, idx, { expanded: !block.expanded })}
								onremove={() => removeBlock(currentFlow, idx)}
							/>
						{:else if block.type === 'loop'}
							<LoopBlock
								label={block.label} overVar={block.overVar || ''} asVar={block.asVar || 'item'}
								expanded={block.expanded} compact={compactView}
								onupdate={(u) => updateBlock(currentFlow, idx, u)}
								ontoggle={() => updateBlock(currentFlow, idx, { expanded: !block.expanded })}
								onremove={() => removeBlock(currentFlow, idx)}
							>
								{#snippet children()}
									{#each block.children || [] as child, ci (child.id)}
										{@const childUpdate = (u: Record<string, unknown>) => {
													const children = [...(block.children || [])];
													children[ci] = { ...children[ci], ...u } as Block;
													updateBlock(currentFlow, idx, { children });
												}}
												{@const childToggle = () => {
													const children = [...(block.children || [])];
													children[ci] = { ...children[ci], expanded: !children[ci].expanded };
													updateBlock(currentFlow, idx, { children });
												}}
												{@const childRemove = () => {
													updateBlock(currentFlow, idx, { children: (block.children || []).filter((_: Block, i: number) => i !== ci) });
												}}
											<div class="flex items-start gap-0">
												<div class="w-8 pt-2 text-right pr-2 text-[10px] font-mono text-muted-foreground/30">
													{stepNum(block.id)}.{String.fromCharCode(97 + ci)}
												</div>
											<div class="flex-1">
												{#if child.type === 'step'}
													<StepBlock label={child.label} agent={child.agent || ''} prompt={child.prompt || ''} outputs={child.outputs || {}}
														expanded={child.expanded} compact={compactView} {agentList}
														onupdate={childUpdate} ontoggle={childToggle} onremove={childRemove}
														onpromptkeydown={(e) => handlePromptKeydown(e, idx)}
													/>
												{:else if child.type === 'when'}
													<WhenBlock label={child.label} switchVar={child.switchVar || ''} arms={child.arms || []}
														{flowNames} {flowColors} stepIds={allStepIds}
														expanded={child.expanded} compact={compactView}
														onupdate={childUpdate} ontoggle={childToggle} onremove={childRemove}
													/>
												{:else if child.type === 'sink'}
													<SinkBlock label={child.label} sinks={child.sinks || []}
														expanded={child.expanded} compact={compactView} {connectorList}
														onupdate={childUpdate} ontoggle={childToggle} onremove={childRemove}
													/>
												{/if}
											</div>
										</div>
										{#if ci < (block.children?.length ?? 0) - 1}
											<div class="flex"><div class="w-8"></div><BlockConnector /></div>
										{/if}
									{/each}
									<!-- Add step inside loop -->
									<div class="flex items-center gap-0 mt-2">
										<div class="w-8"></div>
										<button onclick={() => {
											const n = (block.children?.length ?? 0) + 1;
											const label = `Step ${nextStepNumber()}`;
											const id = slugify(label) + '_' + Date.now().toString(36);
											const children = [...(block.children || []), { id, type: 'step' as const, label, agent: '', prompt: '', expanded: true }];
											updateBlock(currentFlow, idx, { children });
										}} class="rounded border border-dashed border-border/40 px-3 py-1.5 text-[10px] text-muted-foreground hover:text-foreground hover:border-border transition-colors">
											+ Add Step
										</button>
									</div>
								{/snippet}
							</LoopBlock>
						{:else if block.type === 'sink'}
							<SinkBlock
								label={block.label} sinks={block.sinks || []}
								expanded={block.expanded} compact={compactView}
								{connectorList}
								onupdate={(u) => updateBlock(currentFlow, idx, u)}
								ontoggle={() => updateBlock(currentFlow, idx, { expanded: !block.expanded })}
								onremove={() => removeBlock(currentFlow, idx)}
							/>
						{:else if block.type === 'jump'}
							<JumpBlock
								label={block.label} target={block.target || ''}
								{flowNames} {flowColors} stepIds={allStepIds}
								expanded={block.expanded} compact={compactView}
								onupdate={(u) => updateBlock(currentFlow, idx, u)}
								ontoggle={() => updateBlock(currentFlow, idx, { expanded: !block.expanded })}
								onremove={() => removeBlock(currentFlow, idx)}
							/>
						{/if}
					</div>
				</div>

				<!-- Connector between blocks -->
				{#if idx < currentBlocks.length - 1}
					<div class="flex"><div class="w-10"></div><BlockConnector /></div>
				{/if}
			{/each}

			<!-- Add block buttons -->
			<div class="flex"><div class="w-10"></div>
				{#if currentBlocks.length > 0}<BlockConnector />{/if}
			</div>
			<div class="flex items-center gap-0">
				<div class="w-10"></div>
				<div class="flex items-center gap-2 py-1">
					<button onclick={() => addBlock('step')}
						class="rounded-lg border border-dashed border-border hover:border-blue-500/50 hover:bg-blue-950/20 px-3 py-2 text-xs text-muted-foreground hover:text-blue-300 transition-all flex items-center gap-1.5">
						<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15" /></svg>
						Step
					</button>
					<button onclick={() => addBlock('when')}
						class="rounded-lg border border-dashed border-border hover:border-amber-500/50 hover:bg-amber-950/20 px-3 py-2 text-xs text-muted-foreground hover:text-amber-300 transition-all flex items-center gap-1.5">
						<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" /></svg>
						When
					</button>
					<button onclick={() => addBlock('loop')}
						class="rounded-lg border border-dashed border-border hover:border-red-500/50 hover:bg-red-950/20 px-3 py-2 text-xs text-muted-foreground hover:text-red-300 transition-all flex items-center gap-1.5">
						<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0l3.181 3.183a8.25 8.25 0 0013.803-3.7M4.031 9.865a8.25 8.25 0 0113.803-3.7l3.181 3.182" /></svg>
						Loop
					</button>
					<button onclick={() => addBlock('sink')}
						class="rounded-lg border border-dashed border-border hover:border-purple-500/50 hover:bg-purple-950/20 px-3 py-2 text-xs text-muted-foreground hover:text-purple-300 transition-all flex items-center gap-1.5">
						<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5" /></svg>
						Notify
					</button>
					<button onclick={() => addBlock('jump')}
						class="rounded-lg border border-dashed border-border hover:border-indigo-500/50 hover:bg-indigo-950/20 px-3 py-2 text-xs text-muted-foreground hover:text-indigo-300 transition-all flex items-center gap-1.5">
						<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" /></svg>
						Jump
					</button>
				</div>
			</div>
		</div>

		<!-- Jump arrows overlay -->
		<JumpArrows arrows={jumpArrows} containerEl={scrollContainerEl} />
	</div>

	<!-- Instance tracking panel -->
	{#if showInstances}
		<div class="border-t border-border bg-card flex-shrink-0 max-h-48 overflow-y-auto">
			<div class="flex items-center justify-between px-4 py-2 border-b border-border/50">
				<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Workflow Runs</span>
				<div class="flex items-center gap-2">
					<button onclick={loadInstances} class="text-[10px] text-muted-foreground hover:text-foreground">Refresh</button>
					<button onclick={() => (showInstances = false)} class="text-muted-foreground hover:text-foreground">
						<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
					</button>
				</div>
			</div>
			{#if instances.length === 0}
				<div class="px-4 py-3 text-xs text-muted-foreground">No runs yet.</div>
			{:else}
				<table class="w-full text-xs">
					<thead>
						<tr class="text-[10px] text-muted-foreground border-b border-border/30">
							<th class="text-left font-medium px-4 py-1.5">Instance</th>
							<th class="text-left font-medium px-4 py-1.5">Status</th>
							<th class="text-left font-medium px-4 py-1.5">Flow</th>
							<th class="text-left font-medium px-4 py-1.5">Step</th>
							<th class="text-left font-medium px-4 py-1.5">Started</th>
							<th class="text-right font-medium px-4 py-1.5"></th>
						</tr>
					</thead>
					<tbody>
						{#each instances as inst}
							<tr class="border-b border-border/20 hover:bg-accent/30">
								<td class="px-4 py-1.5 font-mono text-muted-foreground">{inst.id.slice(0, 8)}</td>
								<td class="px-4 py-1.5">
									<span class="inline-flex items-center gap-1">
										<span class="h-1.5 w-1.5 rounded-full {inst.status === 'running' ? 'bg-blue-400 animate-pulse' : inst.status === 'completed' ? 'bg-emerald-400' : inst.status === 'failed' ? 'bg-red-400' : 'bg-muted-foreground'}"></span>
										{inst.status}
									</span>
								</td>
								<td class="px-4 py-1.5">
									<span class="inline-flex items-center gap-1">
										{#if flows[inst.current_flow]}<span class="h-1.5 w-1.5 rounded-full" style="background: {flows[inst.current_flow].color}"></span>{/if}
										{inst.current_flow}
									</span>
								</td>
								<td class="px-4 py-1.5 text-muted-foreground">{inst.current_step_index}</td>
								<td class="px-4 py-1.5 text-muted-foreground">{new Date(inst.started_at + 'Z').toLocaleTimeString()}</td>
								<td class="px-4 py-1.5 text-right">
									{#if inst.status === 'running'}
										<button onclick={async () => { await workflows.cancelInstance(inst.id); loadInstances(); }}
											class="text-[10px] text-muted-foreground hover:text-destructive">Cancel</button>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		</div>
	{/if}

	<!-- Variable popup -->
	{#if variablePopup}
		<VariablePopup
			variables={availableVariables(currentBlocks.length)}
			filter={variablePopup.filter}
			x={variablePopup.x} y={variablePopup.y}
			onselect={insertVariable}
			onclose={() => (variablePopup = null)}
		/>
	{/if}

	<!-- Toast -->
	{#if toast}
		<div class="fixed bottom-4 right-4 z-50 rounded-lg border px-4 py-2.5 text-xs font-medium shadow-lg
			{toast.type === 'success' ? 'border-emerald-800/60 bg-emerald-950/80 text-emerald-300' : 'border-red-800/60 bg-red-950/80 text-red-300'}">
			{toast.message}
		</div>
	{/if}
</div>
