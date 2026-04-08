<script lang="ts">
	import { workflows, agents, connectors } from '$lib/api';
	import type { Workflow, WorkflowInstance, Agent, Connector } from '$lib/api';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import yaml from 'js-yaml';

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

	// Block model — a linearized view of the workflow
	interface Block {
		id: string;
		type: 'trigger' | 'task' | 'branch' | 'sink';
		label: string;
		agent?: string;
		prompt?: string;
		procedure?: string;
		connector?: string;
		channel?: string;
		event?: string;
		filter?: Record<string, unknown>;
		sinks?: { connector: string; channel: string; template?: string }[];
		branches?: { name: string; condition: string; blocks: Block[] }[];
		expanded: boolean;
	}

	let blocks = $state<Block[]>([]);
	let selectedBlockId = $state<string | null>(null);
	let dragOverId = $state<string | null>(null);
	let dragIdx = $state<number | null>(null);

	// --- YAML ↔ Blocks ---

	interface WfDef {
		name?: string; description?: string; version?: number;
		trigger?: { connector: string; channel: string; event: string; filter?: Record<string, unknown> };
		nodes: { id: string; label?: string; type?: string; agent?: string; prompt?: string; procedure?: string; sinks?: { connector: string; channel: string; template?: string }[]; outputs?: string[]; position?: { x: number; y: number } }[];
		edges: { from: string; to: string; condition?: string; label?: string }[];
	}

	function yamlToBlocks(yamlStr: string): Block[] {
		let def: WfDef;
		try { def = yaml.load(yamlStr) as WfDef; } catch { return []; }
		if (!def?.nodes) return [];

		const result: Block[] = [];

		// Trigger
		if (def.trigger) {
			result.push({
				id: '__trigger__', type: 'trigger', label: 'Trigger',
				connector: def.trigger.connector, channel: def.trigger.channel,
				event: def.trigger.event, filter: def.trigger.filter,
				expanded: false
			});
		}

		// Build adjacency from edges
		const outgoing = new Map<string, { to: string; condition: string }[]>();
		for (const e of def.edges) {
			const list = outgoing.get(e.from) || [];
			list.push({ to: e.to, condition: e.condition || 'completed' });
			outgoing.set(e.from, list);
		}
		const nodeMap = new Map(def.nodes.map(n => [n.id, n]));

		// Find entry nodes (not targeted by any edge)
		const targets = new Set(def.edges.map(e => e.to));
		const entryIds = def.nodes.filter(n => !targets.has(n.id)).map(n => n.id);

		// Linearize by following the chain
		const visited = new Set<string>();
		function buildChain(nodeId: string): Block[] {
			if (visited.has(nodeId)) return [];
			visited.add(nodeId);
			const n = nodeMap.get(nodeId);
			if (!n) return [];

			const edges = outgoing.get(nodeId) || [];
			const isRouter = n.type === 'router';
			const isSink = n.type === 'sink';

			if (isRouter && edges.length >= 2) {
				const branches = edges.map(e => ({
					name: e.condition,
					condition: e.condition,
					blocks: buildChain(e.to)
				}));
				return [{
					id: n.id, type: 'branch' as const, label: n.label || 'Branch',
					branches, expanded: true
				}];
			}

			const block: Block = {
				id: n.id,
				type: isSink ? 'sink' : 'task',
				label: n.label || n.id,
				agent: n.agent,
				prompt: n.prompt,
				procedure: n.procedure,
				sinks: n.sinks,
				expanded: false
			};

			const rest = edges.length === 1 ? buildChain(edges[0].to) : [];
			return [block, ...rest];
		}

		for (const entryId of entryIds) {
			result.push(...buildChain(entryId));
		}

		// Add any unvisited nodes
		for (const n of def.nodes) {
			if (!visited.has(n.id)) {
				result.push({
					id: n.id,
					type: n.type === 'sink' ? 'sink' : n.type === 'router' ? 'branch' : 'task',
					label: n.label || n.id,
					agent: n.agent, prompt: n.prompt, procedure: n.procedure, sinks: n.sinks,
					expanded: false
				});
			}
		}

		return result;
	}

	function blocksToYaml(blks: Block[]): string {
		const nodes: WfDef['nodes'] = [];
		const edges: WfDef['edges'] = [];
		let trigger: WfDef['trigger'] | undefined;
		let yPos = 0;

		function processBlocks(chain: Block[], prevId?: string) {
			for (let i = 0; i < chain.length; i++) {
				const b = chain[i];
				if (b.type === 'trigger') {
					trigger = { connector: b.connector || '', channel: b.channel || '', event: b.event || '' };
					if (b.filter && Object.keys(b.filter).length) (trigger as any).filter = b.filter;
					continue;
				}

				if (b.type === 'branch') {
					nodes.push({ id: b.id, label: b.label, type: 'router', outputs: b.branches?.map(br => br.name) || [], position: { x: 300, y: yPos } });
					yPos += 120;
					if (prevId) edges.push({ from: prevId, to: b.id, condition: 'completed' });

					for (const br of b.branches || []) {
						if (br.blocks.length > 0) {
							edges.push({ from: b.id, to: br.blocks[0].id, condition: br.condition });
							processBlocks(br.blocks);
						}
					}
					prevId = undefined; // branches break the linear chain
					continue;
				}

				const node: WfDef['nodes'][0] = { id: b.id, label: b.label, position: { x: 300, y: yPos } };
				if (b.type === 'sink') { node.type = 'sink'; if (b.sinks?.length) node.sinks = b.sinks; }
				else { if (b.agent) node.agent = b.agent; if (b.prompt) node.prompt = b.prompt; if (b.procedure) node.procedure = b.procedure; }
				nodes.push(node);
				yPos += 150;

				if (prevId) edges.push({ from: prevId, to: b.id, condition: 'completed' });
				prevId = b.id;
			}
		}

		processBlocks(blks);

		const def: Record<string, unknown> = {
			name: workflowName || 'workflow',
			description: workflow?.description || '',
			version: workflow?.version ?? 1
		};
		if (trigger) def.trigger = trigger;
		def.nodes = nodes;
		def.edges = edges;

		return yaml.dump(def, { lineWidth: -1, noRefs: true, quotingType: '"' });
	}

	// --- Data loading ---

	onMount(async () => {
		const id = $page.params.id!;
		try {
			const [wf, al, cl] = await Promise.all([
				workflows.get(id), agents.list().catch(() => []), connectors.list().catch(() => [])
			]);
			workflow = wf; workflowName = wf.name; yamlContent = wf.yaml_content;
			agentList = al; connectorList = cl;
			blocks = yamlToBlocks(wf.yaml_content);
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
			const y = blocksToYaml(blocks);
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
			const inst = await workflows.run(workflow.id);
			showToast(`Instance started`, 'success');
			showInstances = true;
			await loadInstances();
		} catch (e) { showToast(`Run failed: ${e}`, 'error'); }
		running = false;
	}

	async function loadInstances() {
		if (!workflow) return;
		try { instances = await workflows.instances(workflow.id); } catch { instances = []; }
	}

	function applyYaml() {
		blocks = yamlToBlocks(yamlContent);
		showYaml = false;
	}

	// --- Block manipulation ---

	function addBlock(type: 'task' | 'branch' | 'sink', afterIdx?: number) {
		const id = `${type}_${Date.now().toString(36)}`;
		let newBlock: Block;
		if (type === 'branch') {
			newBlock = { id, type: 'branch', label: 'Branch', expanded: true,
				branches: [
					{ name: 'approved', condition: 'output.verdict == "approved"', blocks: [] },
					{ name: 'rejected', condition: 'output.verdict == "rejected"', blocks: [] }
				]
			};
		} else if (type === 'sink') {
			newBlock = { id, type: 'sink', label: 'Notify', sinks: [{ connector: '', channel: '', template: '' }], expanded: true };
		} else {
			newBlock = { id, type: 'task', label: 'New Step', agent: '', prompt: '', expanded: true };
		}
		const idx = afterIdx !== undefined ? afterIdx + 1 : blocks.length;
		blocks = [...blocks.slice(0, idx), newBlock, ...blocks.slice(idx)];
		selectedBlockId = id;
	}

	function addTrigger() {
		if (blocks.some(b => b.type === 'trigger')) { showToast('Only one trigger allowed', 'error'); return; }
		blocks = [{ id: '__trigger__', type: 'trigger', label: 'Trigger', connector: '', channel: '', event: '', expanded: true }, ...blocks];
		selectedBlockId = '__trigger__';
	}

	function removeBlock(id: string) {
		blocks = blocks.filter(b => b.id !== id);
		if (selectedBlockId === id) selectedBlockId = null;
	}

	function moveBlock(fromIdx: number, toIdx: number) {
		if (fromIdx === toIdx) return;
		const b = [...blocks];
		const [item] = b.splice(fromIdx, 1);
		b.splice(toIdx > fromIdx ? toIdx - 1 : toIdx, 0, item);
		blocks = b;
		dragIdx = null;
		dragOverId = null;
	}

	function updateBlock(id: string, updates: Partial<Block>) {
		blocks = blocks.map(b => b.id === id ? { ...b, ...updates } : b);
	}

	function updateBranch(blockId: string, branchIdx: number, updates: Partial<Block['branches'] extends (infer T)[] | undefined ? T : never>) {
		blocks = blocks.map(b => {
			if (b.id !== blockId || !b.branches) return b;
			const branches = [...b.branches];
			branches[branchIdx] = { ...branches[branchIdx], ...updates };
			return { ...b, branches };
		});
	}

	function addBranchArm(blockId: string) {
		blocks = blocks.map(b => {
			if (b.id !== blockId || !b.branches) return b;
			return { ...b, branches: [...b.branches, { name: `path_${b.branches.length + 1}`, condition: 'default', blocks: [] }] };
		});
	}

	function removeBranchArm(blockId: string, idx: number) {
		blocks = blocks.map(b => {
			if (b.id !== blockId || !b.branches || b.branches.length <= 2) return b;
			return { ...b, branches: b.branches.filter((_, i) => i !== idx) };
		});
	}

	// Block type colors
	function blockAccent(type: string): string {
		switch (type) {
			case 'trigger': return 'border-l-emerald-500';
			case 'task': return 'border-l-blue-500';
			case 'branch': return 'border-l-amber-500';
			case 'sink': return 'border-l-purple-500';
			default: return 'border-l-muted';
		}
	}
	function blockBg(type: string): string {
		switch (type) {
			case 'trigger': return 'bg-emerald-950/30';
			case 'task': return 'bg-card';
			case 'branch': return 'bg-amber-950/20';
			case 'sink': return 'bg-purple-950/20';
			default: return 'bg-card';
		}
	}
	function typeLabel(type: string): string {
		switch (type) {
			case 'trigger': return 'TRIGGER';
			case 'task': return 'STEP';
			case 'branch': return 'BRANCH';
			case 'sink': return 'NOTIFY';
			default: return type.toUpperCase();
		}
	}
	function typeColor(type: string): string {
		switch (type) {
			case 'trigger': return 'text-emerald-400';
			case 'task': return 'text-blue-400';
			case 'branch': return 'text-amber-400';
			case 'sink': return 'text-purple-400';
			default: return 'text-muted-foreground';
		}
	}
</script>

<div class="flex h-full flex-col overflow-hidden">
	<!-- Toolbar -->
	<div class="flex items-center gap-3 border-b border-border bg-card px-4 py-2 flex-shrink-0">
		<a href="/workflows" class="text-muted-foreground hover:text-foreground" title="Back">
			<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" /></svg>
		</a>
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
		<button onclick={() => { showYaml = !showYaml; if (showYaml) yamlContent = blocksToYaml(blocks); }}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {showYaml ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}">YAML</button>
		<button onclick={runWorkflow} disabled={running}
			class="rounded-md bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700 disabled:opacity-50 flex items-center gap-1.5">
			<svg class="h-3 w-3" fill="currentColor" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg>
			{running ? 'Running...' : 'Run'}
		</button>
		<button onclick={save} disabled={saving}
			class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving...' : 'Save'}</button>
	</div>

	<!-- Main content -->
	<div class="flex-1 overflow-y-auto relative">
		{#if showYaml}
			<!-- YAML overlay -->
			<div class="absolute inset-0 z-20 flex flex-col bg-background/95 backdrop-blur-sm">
				<div class="flex items-center justify-between px-4 py-2 border-b border-border">
					<span class="text-xs font-medium text-muted-foreground">YAML Editor</span>
					<div class="flex gap-2">
						<button onclick={applyYaml} class="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90">Apply</button>
						<button onclick={() => (showYaml = false)} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Close</button>
					</div>
				</div>
				<textarea bind:value={yamlContent}
					class="flex-1 w-full bg-transparent p-4 font-mono text-xs text-foreground resize-none focus:outline-none" spellcheck="false"></textarea>
			</div>
		{/if}

		<!-- Block editor -->
		<div class="max-w-2xl mx-auto py-6 px-4 space-y-0">

			{#each blocks as block, idx (block.id)}
				<!-- Connector line between blocks -->
				{#if idx > 0}
					<div class="flex justify-center">
						<div class="w-0.5 h-4 bg-border"></div>
					</div>
				{/if}

				<!-- Block card -->
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="group rounded-lg border border-border/60 {blockBg(block.type)} border-l-[3px] {blockAccent(block.type)} transition-all
						{selectedBlockId === block.id ? 'ring-2 ring-primary/50' : ''}
						{dragOverId === block.id ? 'ring-2 ring-amber-400/50' : ''}"
					draggable={block.type !== 'trigger'}
					ondragstart={(e) => { if (block.type === 'trigger') return; e.dataTransfer?.setData('text/plain', String(idx)); dragIdx = idx; }}
					ondragover={(e) => { e.preventDefault(); dragOverId = block.id; }}
					ondragleave={() => { if (dragOverId === block.id) dragOverId = null; }}
					ondrop={(e) => { e.preventDefault(); dragOverId = null; const from = parseInt(e.dataTransfer?.getData('text/plain') || ''); if (!isNaN(from)) moveBlock(from, idx); }}
					ondragend={() => { dragIdx = null; dragOverId = null; }}
					onclick={() => { selectedBlockId = selectedBlockId === block.id ? null : block.id; }}
				>
					<!-- Header row -->
					<div class="flex items-center gap-2 px-3 py-2">
						{#if block.type !== 'trigger'}
							<div class="cursor-grab active:cursor-grabbing text-muted-foreground/40 hover:text-muted-foreground">
								<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5" /></svg>
							</div>
						{/if}
						<span class="text-[10px] font-bold tracking-wider {typeColor(block.type)}">{typeLabel(block.type)}</span>
						<span class="text-sm font-medium text-foreground flex-1 truncate">{block.label}</span>
						{#if block.type === 'task' && block.agent}
							<span class="text-[10px] text-muted-foreground bg-muted rounded px-1.5 py-0.5">{block.agent}</span>
						{/if}
						<button onclick={(e) => { e.stopPropagation(); updateBlock(block.id, { expanded: !block.expanded }); }}
							class="text-muted-foreground hover:text-foreground transition-colors">
							<svg class="h-3.5 w-3.5 transition-transform {block.expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
						</button>
						<button onclick={(e) => { e.stopPropagation(); removeBlock(block.id); }}
							class="text-muted-foreground/40 hover:text-destructive transition-colors opacity-0 group-hover:opacity-100">
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
						</button>
					</div>

					<!-- Expanded content -->
					{#if block.expanded}
						<div class="border-t border-border/40 px-3 py-3 space-y-3" onclick={(e) => e.stopPropagation()}>
							{#if block.type === 'trigger'}
								<div class="grid grid-cols-2 gap-2">
									<div>
										<label class="block text-[10px] font-medium text-muted-foreground mb-1">Connector</label>
										<select value={block.connector || ''} onchange={(e) => updateBlock(block.id, { connector: e.currentTarget.value })}
											class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
											<option value="">Select...</option>
											{#each connectorList as c}<option value={c.name}>{c.name}</option>{/each}
											<option value="webhook">webhook</option>
											<option value="telegram">telegram</option>
											<option value="file_watcher">file_watcher</option>
										</select>
									</div>
									<div>
										<label class="block text-[10px] font-medium text-muted-foreground mb-1">Event</label>
										<input type="text" value={block.event || ''} oninput={(e) => updateBlock(block.id, { event: e.currentTarget.value })}
											class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="e.g. message_received" />
									</div>
								</div>
								<div>
									<label class="block text-[10px] font-medium text-muted-foreground mb-1">Channel</label>
									<input type="text" value={block.channel || ''} oninput={(e) => updateBlock(block.id, { channel: e.currentTarget.value })}
										class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="channel-name" />
								</div>

							{:else if block.type === 'task'}
								<div class="grid grid-cols-2 gap-2">
									<div>
										<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
										<input type="text" value={block.label} oninput={(e) => updateBlock(block.id, { label: e.currentTarget.value })}
											class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
									</div>
									<div>
										<label class="block text-[10px] font-medium text-muted-foreground mb-1">Agent</label>
										<select value={block.agent || ''} onchange={(e) => updateBlock(block.id, { agent: e.currentTarget.value })}
											class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs">
											<option value="">Select agent...</option>
											{#each agentList as a}<option value={a.name}>{a.config?.display_name || a.name}</option>{/each}
										</select>
									</div>
								</div>
								<div>
									<label class="block text-[10px] font-medium text-muted-foreground mb-1">Prompt</label>
									<textarea value={block.prompt || ''} oninput={(e) => updateBlock(block.id, { prompt: e.currentTarget.value })}
										rows="3" class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs font-mono resize-none"
										placeholder={'What should this agent do?\nUse {{trigger.payload.field}} or {{nodes.step1.output}}'}></textarea>
								</div>
								{#if block.procedure !== undefined && block.procedure !== ''}
									<div>
										<label class="block text-[10px] font-medium text-muted-foreground mb-1">Procedure</label>
										<input type="text" value={block.procedure || ''} oninput={(e) => updateBlock(block.id, { procedure: e.currentTarget.value })}
											class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="procedure-name" />
									</div>
								{/if}

							{:else if block.type === 'branch'}
								<div>
									<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
									<input type="text" value={block.label} oninput={(e) => updateBlock(block.id, { label: e.currentTarget.value })}
										class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
								</div>
								<!-- Branch arms -->
								<div class="flex gap-3 mt-2">
									{#each block.branches || [] as branch, bi}
										<div class="flex-1 space-y-2">
											<div class="flex items-center gap-1.5">
												<div class="h-2 w-2 rounded-full {bi === 0 ? 'bg-emerald-400' : bi === 1 ? 'bg-red-400' : 'bg-amber-400'}"></div>
												<input type="text" value={branch.name}
													oninput={(e) => updateBranch(block.id, bi, { name: e.currentTarget.value })}
													class="flex-1 rounded border border-input bg-background px-2 py-1 text-xs font-semibold" />
												{#if (block.branches?.length || 0) > 2}
													<button onclick={() => removeBranchArm(block.id, bi)} class="text-muted-foreground/40 hover:text-destructive text-xs">x</button>
												{/if}
											</div>
											<input type="text" value={branch.condition}
												oninput={(e) => updateBranch(block.id, bi, { condition: e.currentTarget.value })}
												class="w-full rounded border border-input bg-background px-2 py-1 text-[10px] font-mono"
												placeholder='output.verdict == "value"' />
											<!-- Nested blocks placeholder -->
											<div class="min-h-[40px] rounded border border-dashed border-border/40 bg-background/30 p-2 text-center">
												<span class="text-[10px] text-muted-foreground/50">→ continues to next step</span>
											</div>
										</div>
									{/each}
								</div>
								<button onclick={() => addBranchArm(block.id)}
									class="text-[10px] text-primary hover:underline">+ Add branch</button>

							{:else if block.type === 'sink'}
								<div>
									<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
									<input type="text" value={block.label} oninput={(e) => updateBlock(block.id, { label: e.currentTarget.value })}
										class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
								</div>
								{#each block.sinks || [] as sink, si}
									<div class="rounded border border-border/40 p-2 space-y-1.5 relative">
										<button onclick={() => {
											const sinks = (block.sinks || []).filter((_, i) => i !== si);
											updateBlock(block.id, { sinks });
										}} class="absolute top-1 right-1 text-muted-foreground/40 hover:text-destructive text-xs">x</button>
										<div class="grid grid-cols-2 gap-2">
											<div>
												<label class="block text-[10px] text-muted-foreground mb-0.5">Connector</label>
												<select value={sink.connector} onchange={(e) => {
													const sinks = [...(block.sinks || [])];
													sinks[si] = { ...sinks[si], connector: e.currentTarget.value };
													updateBlock(block.id, { sinks });
												}} class="w-full rounded border border-input bg-background px-2 py-1 text-xs">
													<option value="">Select...</option>
													{#each connectorList as c}<option value={c.name}>{c.name}</option>{/each}
												</select>
											</div>
											<div>
												<label class="block text-[10px] text-muted-foreground mb-0.5">Channel</label>
												<input type="text" value={sink.channel} oninput={(e) => {
													const sinks = [...(block.sinks || [])];
													sinks[si] = { ...sinks[si], channel: e.currentTarget.value };
													updateBlock(block.id, { sinks });
												}} class="w-full rounded border border-input bg-background px-2 py-1 text-xs" placeholder="channel" />
											</div>
										</div>
										<div>
											<label class="block text-[10px] text-muted-foreground mb-0.5">Template</label>
											<textarea value={sink.template || ''} oninput={(e) => {
												const sinks = [...(block.sinks || [])];
												sinks[si] = { ...sinks[si], template: e.currentTarget.value };
												updateBlock(block.id, { sinks });
											}} rows="2" class="w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono resize-none" placeholder="Message template..."></textarea>
										</div>
									</div>
								{/each}
								<button onclick={() => {
									const sinks = [...(block.sinks || []), { connector: '', channel: '', template: '' }];
									updateBlock(block.id, { sinks });
								}} class="text-[10px] text-primary hover:underline">+ Add sink</button>
							{/if}
						</div>
					{/if}
				</div>
			{/each}

			<!-- Add block buttons -->
			{#if blocks.length > 0}
				<div class="flex justify-center">
					<div class="w-0.5 h-4 bg-border"></div>
				</div>
			{/if}
			<div class="flex items-center justify-center gap-2 py-2">
				<button onclick={() => addBlock('task')}
					class="rounded-lg border border-dashed border-border hover:border-blue-500/50 hover:bg-blue-950/20 px-4 py-2.5 text-xs text-muted-foreground hover:text-blue-300 transition-all flex items-center gap-2">
					<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15" /></svg>
					Step
				</button>
				<button onclick={() => addBlock('branch')}
					class="rounded-lg border border-dashed border-border hover:border-amber-500/50 hover:bg-amber-950/20 px-4 py-2.5 text-xs text-muted-foreground hover:text-amber-300 transition-all flex items-center gap-2">
					<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" /></svg>
					Branch
				</button>
				<button onclick={() => addBlock('sink')}
					class="rounded-lg border border-dashed border-border hover:border-purple-500/50 hover:bg-purple-950/20 px-4 py-2.5 text-xs text-muted-foreground hover:text-purple-300 transition-all flex items-center gap-2">
					<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5" /></svg>
					Notify
				</button>
				{#if !blocks.some(b => b.type === 'trigger')}
					<button onclick={addTrigger}
						class="rounded-lg border border-dashed border-border hover:border-emerald-500/50 hover:bg-emerald-950/20 px-4 py-2.5 text-xs text-muted-foreground hover:text-emerald-300 transition-all flex items-center gap-2">
						<svg class="h-3.5 w-3.5" fill="currentColor" viewBox="0 0 24 24"><path d="M13 2L3 14h9l-1 10 10-12h-9l1-10z" /></svg>
						Trigger
					</button>
				{/if}
			</div>
		</div>
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
				<div class="px-4 py-3 text-xs text-muted-foreground">No runs yet. Click "Run" to start one.</div>
			{:else}
				<table class="w-full text-xs">
					<thead>
						<tr class="text-[10px] text-muted-foreground border-b border-border/30">
							<th class="text-left font-medium px-4 py-1.5">Instance</th>
							<th class="text-left font-medium px-4 py-1.5">Status</th>
							<th class="text-left font-medium px-4 py-1.5">Current Node</th>
							<th class="text-left font-medium px-4 py-1.5">Started</th>
							<th class="text-right font-medium px-4 py-1.5"></th>
						</tr>
					</thead>
					<tbody>
						{#each instances as inst}
							<tr class="border-b border-border/20 hover:bg-accent/30">
								<td class="px-4 py-1.5 font-mono text-muted-foreground">{inst.id.slice(0, 8)}...</td>
								<td class="px-4 py-1.5">
									<span class="inline-flex items-center gap-1">
										<span class="h-1.5 w-1.5 rounded-full {
											inst.status === 'running' ? 'bg-blue-400 animate-pulse' :
											inst.status === 'completed' ? 'bg-emerald-400' :
											inst.status === 'failed' ? 'bg-red-400' :
											'bg-muted-foreground'}"></span>
										{inst.status}
									</span>
								</td>
								<td class="px-4 py-1.5 text-muted-foreground">{inst.current_flow ? `${inst.current_flow}[${inst.current_step_index}]` : '—'}</td>
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

	<!-- Toast -->
	{#if toast}
		<div class="fixed bottom-4 right-4 z-50 rounded-lg border px-4 py-2.5 text-xs font-medium shadow-lg
			{toast.type === 'success' ? 'border-emerald-800/60 bg-emerald-950/80 text-emerald-300' : 'border-red-800/60 bg-red-950/80 text-red-300'}">
			{toast.message}
		</div>
	{/if}
</div>
