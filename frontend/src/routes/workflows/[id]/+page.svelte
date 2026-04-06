<script lang="ts">
	import {
		SvelteFlow,
		Background,
		BackgroundVariant,
		Controls,
		MiniMap,
		type Node,
		type Edge,
		type Connection
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import TaskNode from '$lib/components/flow/TaskNode.svelte';
	import TriggerNode from '$lib/components/flow/TriggerNode.svelte';
	import SinkNode from '$lib/components/flow/SinkNode.svelte';
	import { workflows, agents, connectors } from '$lib/api';
	import type { Workflow, Agent, Connector } from '$lib/api';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import yaml from 'js-yaml';

	const nodeTypes = {
		task: TaskNode,
		trigger: TriggerNode,
		sink: SinkNode
	};

	let nodes = $state.raw<Node[]>([]);
	let edges = $state.raw<Edge[]>([]);
	let workflow = $state<Workflow | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let showYaml = $state(false);
	let yamlContent = $state('');
	let saving = $state(false);
	let running = $state(false);
	let agentList = $state<Agent[]>([]);
	let connectorList = $state<Connector[]>([]);
	let workflowName = $state('');
	let showPanel = $state(true);
	let toast = $state<{ message: string; type: 'success' | 'error' } | null>(null);
	let contextMenu = $state<{ x: number; y: number } | null>(null);

	let selectedNode = $derived(
		selectedNodeId ? nodes.find((n) => n.id === selectedNodeId) ?? null : null
	);

	// --- YAML to Graph conversion ---

	interface WorkflowDef {
		name?: string;
		description?: string;
		version?: number;
		trigger?: { connector: string; channel: string; event: string; filter?: Record<string, unknown> };
		nodes: {
			id: string;
			label?: string;
			type?: string;
			agent?: string;
			prompt?: string;
			procedure?: string;
			sinks?: { connector: string; channel: string; template?: string }[];
			position?: { x: number; y: number };
		}[];
		edges: {
			from: string;
			to: string;
			condition?: string;
			label?: string;
		}[];
	}

	function yamlToGraph(yamlStr: string): { nodes: Node[]; edges: Edge[] } {
		let def: WorkflowDef;
		try {
			def = yaml.load(yamlStr) as WorkflowDef;
		} catch {
			return { nodes: [], edges: [] };
		}
		if (!def || !def.nodes) return { nodes: [], edges: [] };

		const graphNodes: Node[] = [];
		const graphEdges: Edge[] = [];

		// Trigger node
		if (def.trigger) {
			graphNodes.push({
				id: '__trigger__',
				type: 'trigger',
				position: { x: 50, y: 150 },
				data: {
					connector: def.trigger.connector,
					channel: def.trigger.channel,
					event: def.trigger.event,
					filter: def.trigger.filter
				}
			});

			// Connect trigger to entry nodes (nodes with no incoming edges)
			const targets = new Set(def.edges.map((e) => e.to));
			for (const node of def.nodes) {
				if (!targets.has(node.id)) {
					graphEdges.push({
						id: `__trigger__-${node.id}`,
						source: '__trigger__',
						target: node.id,
						type: 'smoothstep',
						animated: true,
						style: 'stroke: hsl(142, 71%, 45%); stroke-width: 2;',
						label: 'trigger'
					});
				}
			}
		}

		// Workflow nodes
		for (let i = 0; i < def.nodes.length; i++) {
			const n = def.nodes[i];
			const isSink = n.type === 'sink';
			const nodeType = isSink ? 'sink' : 'task';

			// Collect distinct outgoing edge conditions for multiple handles
			const outEdges = def.edges.filter((e) => e.from === n.id);
			const outputs =
				outEdges.length > 1 ? outEdges.map((e) => e.condition || 'completed') : [];

			graphNodes.push({
				id: n.id,
				type: nodeType,
				position: n.position ?? { x: 300, y: i * 160 },
				data: {
					label: n.label ?? n.id,
					agent: n.agent,
					prompt: n.prompt,
					procedure: n.procedure,
					sinks: n.sinks ?? [],
					outputs
				}
			});
		}

		// Workflow edges
		for (const e of def.edges) {
			const condLabel = e.label || (e.condition !== 'completed' ? e.condition : undefined);
			// If source node has multiple outgoing edges, use condition as sourceHandle
			const sourceOutEdges = def.edges.filter((ed) => ed.from === e.from);
			const sourceHandle =
				sourceOutEdges.length > 1 ? e.condition || 'completed' : undefined;

			graphEdges.push({
				id: `${e.from}-${e.to}-${e.condition || 'completed'}`,
				source: e.from,
				target: e.to,
				sourceHandle,
				type: 'smoothstep',
				animated: false,
				label: condLabel,
				style: 'stroke: hsl(225, 18%, 30%); stroke-width: 2;',
				labelStyle: 'fill: hsl(225, 15%, 55%); font-size: 10px; background: hsl(228, 22%, 11%);'
			});
		}

		return { nodes: graphNodes, edges: graphEdges };
	}

	// --- Graph to YAML conversion ---

	function graphToYaml(graphNodes: Node[], graphEdges: Edge[]): string {
		const triggerNode = graphNodes.find((n) => n.type === 'trigger');
		const regularNodes = graphNodes.filter((n) => n.type !== 'trigger');

		const def: Record<string, unknown> = {
			name: workflowName || workflow?.name || 'workflow',
			description: workflow?.description || '',
			version: workflow?.version ?? 1
		};

		if (triggerNode) {
			def.trigger = {
				connector: triggerNode.data.connector || '',
				channel: triggerNode.data.channel || '',
				event: triggerNode.data.event || ''
			};
			if (
				triggerNode.data.filter &&
				typeof triggerNode.data.filter === 'object' &&
				Object.keys(triggerNode.data.filter).length > 0
			) {
				(def.trigger as Record<string, unknown>).filter = triggerNode.data.filter;
			}
		}

		def.nodes = regularNodes.map((n) => {
			const node: Record<string, unknown> = {
				id: n.id,
				label: n.data.label || n.id,
				position: { x: Math.round(n.position.x), y: Math.round(n.position.y) }
			};
			if (n.type === 'sink') {
				node.type = 'sink';
				if ((n.data.sinks as any[])?.length) node.sinks = n.data.sinks;
			} else {
				if (n.data.agent) node.agent = n.data.agent;
				if (n.data.prompt) node.prompt = n.data.prompt;
				if (n.data.procedure) node.procedure = n.data.procedure;
			}
			return node;
		});

		// Edges: skip trigger edges
		def.edges = graphEdges
			.filter((e) => e.source !== '__trigger__')
			.map((e) => {
				const edge: Record<string, unknown> = {
					from: e.source,
					to: e.target,
					condition: e.sourceHandle || 'completed'
				};
				if (e.label && e.label !== e.sourceHandle) {
					edge.label = e.label;
				}
				return edge;
			});

		return yaml.dump(def, { lineWidth: -1, noRefs: true, quotingType: '"' });
	}

	// --- Data loading ---

	onMount(async () => {
		const id = $page.params.id!;
		try {
			const [wf, al, cl] = await Promise.all([
				workflows.get(id),
				agents.list().catch(() => []),
				connectors.list().catch(() => [])
			]);
			workflow = wf;
			workflowName = wf.name;
			yamlContent = wf.yaml_content;
			agentList = al;
			connectorList = cl;

			const graph = yamlToGraph(wf.yaml_content);
			nodes = graph.nodes;
			edges = graph.edges;
		} catch (e) {
			showToast(`Failed to load workflow: ${e}`, 'error');
		}
	});

	// --- Actions ---

	function showToast(message: string, type: 'success' | 'error') {
		toast = { message, type };
		setTimeout(() => {
			toast = null;
		}, 3000);
	}

	async function save() {
		if (!workflow) return;
		saving = true;
		try {
			const updatedYaml = graphToYaml(nodes, edges);
			yamlContent = updatedYaml;
			await workflows.update(workflow.id, {
				yaml_content: updatedYaml,
				description: workflow.description ?? undefined
			});
			workflow = await workflows.get(workflow.id);
			showToast('Workflow saved', 'success');
		} catch (e) {
			showToast(`Save failed: ${e}`, 'error');
		}
		saving = false;
	}

	async function toggleEnabled() {
		if (!workflow) return;
		try {
			if (workflow.enabled) {
				workflow = await workflows.disable(workflow.id);
			} else {
				workflow = await workflows.enable(workflow.id);
			}
		} catch (e) {
			showToast(String(e), 'error');
		}
	}

	async function runWorkflow() {
		if (!workflow) return;
		running = true;
		try {
			const instance = await workflows.run(workflow.id);
			showToast(`Started instance: ${instance.id.slice(0, 8)}...`, 'success');
		} catch (e) {
			showToast(`Run failed: ${e}`, 'error');
		}
		running = false;
	}

	function applyYaml() {
		const graph = yamlToGraph(yamlContent);
		nodes = graph.nodes;
		edges = graph.edges;
		showYaml = false;
	}

	// --- Node selection ---

	function handleNodeClick({ node }: { node: Node; event: MouseEvent | TouchEvent }) {
		selectedNodeId = node.id;
	}

	function handlePaneClick() {
		selectedNodeId = null;
		contextMenu = null;
	}

	// --- Connection handling ---

	function handleConnect(connection: Connection) {
		const newEdge: Edge = {
			id: `${connection.source}-${connection.target}-${Date.now()}`,
			source: connection.source,
			target: connection.target,
			sourceHandle: connection.sourceHandle ?? undefined,
			targetHandle: connection.targetHandle ?? undefined,
			type: 'smoothstep',
			style: 'stroke: hsl(225, 18%, 30%); stroke-width: 2;'
		};
		edges = [...edges, newEdge];
	}

	// --- Context menu for adding nodes ---

	function onContextMenu(event: MouseEvent) {
		event.preventDefault();
		contextMenu = { x: event.clientX, y: event.clientY };
	}

	function addNode(type: 'task' | 'sink') {
		const id = `${type}_${Date.now().toString(36)}`;
		const newNode: Node = {
			id,
			type,
			position: { x: 300, y: 200 },
			data:
				type === 'sink'
					? { label: 'New Sink', sinks: [{ connector: '', channel: '', template: '' }] }
					: { label: 'New Task', agent: '', prompt: '', procedure: '' }
		};
		nodes = [...nodes, newNode];
		selectedNodeId = id;
		contextMenu = null;
	}

	function addTrigger() {
		// Only one trigger allowed
		if (nodes.some((n) => n.type === 'trigger')) {
			showToast('Only one trigger node is allowed', 'error');
			contextMenu = null;
			return;
		}
		const newNode: Node = {
			id: '__trigger__',
			type: 'trigger',
			position: { x: 50, y: 150 },
			data: { connector: '', channel: '', event: '' }
		};
		nodes = [...nodes, newNode];
		selectedNodeId = '__trigger__';
		contextMenu = null;
	}

	function deleteSelectedNode() {
		if (!selectedNodeId) return;
		const id = selectedNodeId;
		nodes = nodes.filter((n) => n.id !== id);
		edges = edges.filter((e) => e.source !== id && e.target !== id);
		selectedNodeId = null;
	}

	// --- Property editing ---

	function updateNodeData(nodeId: string, updates: Record<string, unknown>) {
		nodes = nodes.map((n) => {
			if (n.id !== nodeId) return n;
			return { ...n, data: { ...n.data, ...updates } };
		});
	}

	function updateSink(nodeId: string, sinkIndex: number, field: string, value: string) {
		const node = nodes.find((n) => n.id === nodeId);
		if (!node) return;
		const sinks = [...((node.data.sinks as any[]) || [])];
		sinks[sinkIndex] = { ...sinks[sinkIndex], [field]: value };
		updateNodeData(nodeId, { sinks });
	}

	function addSinkEntry(nodeId: string) {
		const node = nodes.find((n) => n.id === nodeId);
		if (!node) return;
		const sinks = [...((node.data.sinks as any[]) || []), { connector: '', channel: '', template: '' }];
		updateNodeData(nodeId, { sinks });
	}

	function removeSinkEntry(nodeId: string, index: number) {
		const node = nodes.find((n) => n.id === nodeId);
		if (!node) return;
		const sinks = ((node.data.sinks as any[]) || []).filter((_: unknown, i: number) => i !== index);
		updateNodeData(nodeId, { sinks });
	}

	// Handle keyboard shortcuts
	function handleKeyDown(event: KeyboardEvent) {
		if (event.key === 'Delete' || event.key === 'Backspace') {
			// Don't delete if focused on an input
			const tag = (event.target as HTMLElement)?.tagName;
			if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
			if (selectedNodeId) deleteSelectedNode();
		}
	}
</script>

<svelte:window onkeydown={handleKeyDown} />

<div class="flex h-full flex-col overflow-hidden">
	<!-- Toolbar -->
	<div class="flex items-center gap-3 border-b border-border bg-card px-4 py-2 flex-shrink-0">
		<a href="/workflows" class="text-muted-foreground hover:text-foreground transition-colors" title="Back">
			<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
				<path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" />
			</svg>
		</a>

		<input
			type="text"
			bind:value={workflowName}
			class="border-none bg-transparent text-sm font-semibold text-foreground focus:outline-none focus:ring-0 w-48"
			placeholder="Workflow name"
		/>

		<div class="flex-1"></div>

		<!-- Enable/Disable toggle -->
		{#if workflow}
			<button
				onclick={toggleEnabled}
				class="relative flex-shrink-0 h-5 w-9 rounded-full transition-colors {workflow.enabled ? 'bg-emerald-600' : 'bg-[hsl(225,18%,25%)]'}"
				title={workflow.enabled ? 'Disable' : 'Enable'}
			>
				<span
					class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform {workflow.enabled ? 'translate-x-4' : 'translate-x-0.5'}"
				></span>
			</button>
		{/if}

		<button
			onclick={() => (showYaml = !showYaml)}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium transition-colors
				{showYaml ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}"
		>
			YAML
		</button>

		<button
			onclick={runWorkflow}
			disabled={running}
			class="rounded-md bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700 disabled:opacity-50 transition-colors flex items-center gap-1.5"
		>
			<svg class="h-3 w-3" fill="currentColor" viewBox="0 0 24 24">
				<path d="M8 5v14l11-7z" />
			</svg>
			{running ? 'Running...' : 'Run'}
		</button>

		<button
			onclick={save}
			disabled={saving}
			class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors"
		>
			{saving ? 'Saving...' : 'Save'}
		</button>
	</div>

	<!-- Main area -->
	<div class="relative flex flex-1 overflow-hidden">
		<!-- Canvas -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="flex-1 relative" oncontextmenu={onContextMenu}>
			<SvelteFlow
				bind:nodes
				bind:edges
				{nodeTypes}
				fitView
				snapGrid={[15, 15]}
				defaultEdgeOptions={{ type: 'smoothstep' }}
				onnodeclick={handleNodeClick}
				onpaneclick={handlePaneClick}
				onconnect={handleConnect}
				oninit={() => {}}
			>
				<Background variant={BackgroundVariant.Dots} gap={20} size={1} />
				<Controls position="bottom-left" />
				<MiniMap
					position="bottom-right"
					nodeColor={(node) => {
						if (node.type === 'trigger') return 'hsl(142, 71%, 45%)';
						if (node.type === 'sink') return 'hsl(217, 91%, 60%)';
						return 'hsl(225, 65%, 55%)';
					}}
				/>
			</SvelteFlow>

			<!-- YAML overlay -->
			{#if showYaml}
				<div class="absolute inset-0 z-20 flex flex-col bg-[hsl(228,25%,8%)]/95 backdrop-blur-sm">
					<div class="flex items-center justify-between px-4 py-2 border-b border-border">
						<span class="text-xs font-medium text-muted-foreground">YAML Editor</span>
						<div class="flex gap-2">
							<button
								onclick={applyYaml}
								class="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
							>
								Apply to Canvas
							</button>
							<button
								onclick={() => (showYaml = false)}
								class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent transition-colors"
							>
								Close
							</button>
						</div>
					</div>
					<textarea
						bind:value={yamlContent}
						class="flex-1 w-full bg-transparent p-4 font-mono text-xs text-foreground resize-none focus:outline-none"
						spellcheck="false"
					></textarea>
				</div>
			{/if}

			<!-- Context menu -->
			{#if contextMenu}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="fixed z-30 rounded-lg border border-border bg-card shadow-xl py-1 min-w-[160px]"
					style="left: {contextMenu.x}px; top: {contextMenu.y}px"
					onclick={(e) => e.stopPropagation()}
				>
					<button
						onclick={() => addNode('task')}
						class="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
					>
						<svg class="h-3.5 w-3.5 text-primary" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
							<path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15" />
						</svg>
						Add Task Node
					</button>
					<button
						onclick={() => addNode('sink')}
						class="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
					>
						<svg class="h-3.5 w-3.5 text-blue-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
							<path stroke-linecap="round" stroke-linejoin="round" d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5" />
						</svg>
						Add Sink Node
					</button>
					<button
						onclick={addTrigger}
						class="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-foreground hover:bg-accent transition-colors"
					>
						<svg class="h-3.5 w-3.5 text-emerald-400" fill="currentColor" viewBox="0 0 24 24">
							<path d="M13 2L3 14h9l-1 10 10-12h-9l1-10z" />
						</svg>
						Add Trigger
					</button>
					<div class="my-1 border-t border-border"></div>
					<button
						onclick={() => (contextMenu = null)}
						class="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-muted-foreground hover:bg-accent transition-colors"
					>
						Cancel
					</button>
				</div>
			{/if}
		</div>

		<!-- Right panel: Node properties -->
		{#if showPanel && selectedNode}
			<div class="w-72 flex-shrink-0 border-l border-border bg-card overflow-y-auto">
				<div class="flex items-center justify-between border-b border-border px-4 py-3">
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						{selectedNode.type === 'trigger' ? 'Trigger' : selectedNode.type === 'sink' ? 'Sink' : 'Task'} Properties
					</h3>
					<div class="flex items-center gap-1">
						<button
							onclick={deleteSelectedNode}
							class="rounded p-1 text-muted-foreground hover:text-destructive hover:bg-destructive/10 transition-colors"
							title="Delete node"
						>
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M14.74 9l-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 01-2.244 2.077H8.084a2.25 2.25 0 01-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 00-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 013.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 00-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 00-7.5 0" />
							</svg>
						</button>
						<button
							onclick={() => (selectedNodeId = null)}
							class="rounded p-1 text-muted-foreground hover:text-foreground transition-colors"
							title="Close"
						>
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
							</svg>
						</button>
					</div>
				</div>

				<div class="p-4 space-y-4">
					{#if selectedNode.type === 'task'}
						<!-- Task node properties -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
							<input
								type="text"
								value={selectedNode.data.label || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { label: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
							/>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Agent</label>
							<select
								value={selectedNode.data.agent || ''}
								onchange={(e) => updateNodeData(selectedNode!.id, { agent: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
							>
								<option value="">Select agent...</option>
								{#each agentList as agent}
									<option value={agent.name}>{agent.config?.display_name || agent.name}</option>
								{/each}
							</select>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Prompt</label>
							<textarea
								value={(selectedNode.data.prompt as string) || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { prompt: e.currentTarget.value })}
								rows="5"
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs font-mono resize-none focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder="Enter the prompt for this step..."
							></textarea>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Procedure (optional)</label>
							<input
								type="text"
								value={selectedNode.data.procedure || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { procedure: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder="procedure-name"
							/>
						</div>

					{:else if selectedNode.type === 'sink'}
						<!-- Sink node properties -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
							<input
								type="text"
								value={selectedNode.data.label || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { label: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
							/>
						</div>

						<div class="space-y-3">
							<div class="flex items-center justify-between">
								<label class="text-[10px] font-medium text-muted-foreground">Sinks</label>
								<button
									onclick={() => addSinkEntry(selectedNode!.id)}
									class="rounded px-1.5 py-0.5 text-[10px] text-primary hover:bg-primary/10 transition-colors"
								>
									+ Add
								</button>
							</div>

							{#each ((selectedNode.data.sinks || []) as any[]) as sink, i}
								<div class="rounded-md border border-border p-2.5 space-y-2 relative">
									<button
										onclick={() => removeSinkEntry(selectedNode!.id, i)}
										class="absolute top-1.5 right-1.5 text-muted-foreground hover:text-destructive text-xs"
									>x</button>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Connector</label>
										<select
											value={sink.connector}
											onchange={(e) => updateSink(selectedNode!.id, i, 'connector', e.currentTarget.value)}
											class="w-full rounded border border-input bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring"
										>
											<option value="">Select...</option>
											{#each connectorList as c}
												<option value={c.name}>{c.name} ({c.connector_type})</option>
											{/each}
											<option value="telegram">telegram</option>
											<option value="email">email</option>
											<option value="slack">slack</option>
											<option value="webhook">webhook</option>
										</select>
									</div>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Channel</label>
										<input
											type="text"
											value={sink.channel}
											oninput={(e) => updateSink(selectedNode!.id, i, 'channel', e.currentTarget.value)}
											class="w-full rounded border border-input bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring"
											placeholder="channel-name"
										/>
									</div>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Template</label>
										<textarea
											value={sink.template || ''}
											oninput={(e) => updateSink(selectedNode!.id, i, 'template', e.currentTarget.value)}
											rows="2"
											class="w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono resize-none focus:outline-none focus:ring-1 focus:ring-ring"
											placeholder="Message template..."
										></textarea>
									</div>
								</div>
							{/each}
						</div>

					{:else if selectedNode.type === 'trigger'}
						<!-- Trigger node properties -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Connector</label>
							<select
								value={selectedNode.data.connector || ''}
								onchange={(e) => updateNodeData(selectedNode!.id, { connector: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
							>
								<option value="">Select...</option>
								{#each connectorList as c}
									<option value={c.name}>{c.name} ({c.connector_type})</option>
								{/each}
								<option value="webhook">webhook</option>
								<option value="jira">jira</option>
								<option value="github">github</option>
								<option value="telegram">telegram</option>
								<option value="file_watcher">file_watcher</option>
							</select>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Channel</label>
							<input
								type="text"
								value={selectedNode.data.channel || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { channel: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder="channel-name"
							/>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Event</label>
							<input
								type="text"
								value={selectedNode.data.event || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { event: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder="e.g. issue_created, message_received"
							/>
						</div>

						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Filter (JSON)</label>
							<textarea
								value={selectedNode.data.filter ? JSON.stringify(selectedNode.data.filter, null, 2) : ''}
								oninput={(e) => {
									try {
										const f = JSON.parse(e.currentTarget.value || '{}');
										updateNodeData(selectedNode!.id, { filter: f });
									} catch {
										// Don't update on invalid JSON
									}
								}}
								rows="3"
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs font-mono resize-none focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder={'{"type": "Story"}'}
							></textarea>
						</div>
					{/if}

					<!-- Node ID (read-only) -->
					<div class="pt-2 border-t border-border">
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">Node ID</label>
						<div class="rounded-md bg-muted px-2.5 py-1.5 text-xs font-mono text-muted-foreground">
							{selectedNode.id}
						</div>
					</div>
				</div>
			</div>
		{/if}
	</div>

	<!-- Toast notification -->
	{#if toast}
		<div
			class="fixed bottom-4 right-4 z-50 rounded-lg border px-4 py-2.5 text-xs font-medium shadow-lg transition-all
				{toast.type === 'success'
					? 'border-emerald-800/60 bg-emerald-950/80 text-emerald-300'
					: 'border-red-800/60 bg-red-950/80 text-red-300'}"
		>
			{toast.message}
		</div>
	{/if}
</div>

<style>
	:global(.svelte-flow) {
		--xy-background-color: hsl(228, 22%, 8%);
		--xy-node-border-radius: 0.5rem;
		--xy-edge-stroke: hsl(225, 18%, 30%);
		--xy-edge-stroke-selected: hsl(225, 65%, 55%);
		--xy-edge-stroke-width: 2;
		--xy-connectionline-stroke: hsl(225, 65%, 55%);
		--xy-connectionline-stroke-width: 2;
		--xy-attribution-background-color: transparent;
	}

	:global(.svelte-flow .svelte-flow__node) {
		border: none;
		background: none;
		padding: 0;
		border-radius: 0;
		box-shadow: none;
	}

	:global(.svelte-flow .svelte-flow__node.selected) {
		outline: 2px solid hsl(225, 65%, 55%);
		outline-offset: 2px;
		border-radius: 0.5rem;
	}

	:global(.svelte-flow .svelte-flow__edge-text) {
		font-size: 10px;
	}

	:global(.svelte-flow .svelte-flow__minimap) {
		background: hsl(228, 22%, 11%);
		border: 1px solid hsl(225, 18%, 18%);
		border-radius: 0.375rem;
	}

	:global(.svelte-flow .svelte-flow__controls) {
		background: hsl(228, 22%, 11%);
		border: 1px solid hsl(225, 18%, 18%);
		border-radius: 0.375rem;
		box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.3);
	}

	:global(.svelte-flow .svelte-flow__controls button) {
		background: hsl(228, 22%, 11%);
		border-color: hsl(225, 18%, 18%);
		color: hsl(225, 15%, 55%);
	}

	:global(.svelte-flow .svelte-flow__controls button:hover) {
		background: hsl(225, 50%, 25%);
		color: hsl(220, 20%, 92%);
	}

	:global(.svelte-flow .svelte-flow__controls button svg) {
		fill: currentColor;
	}

	:global(.svelte-flow .svelte-flow__background) {
		opacity: 0.4;
	}

	:global(.svelte-flow .svelte-flow__edge-path) {
		stroke: hsl(225, 18%, 30%);
	}

	:global(.svelte-flow .svelte-flow__edge.selected .svelte-flow__edge-path) {
		stroke: hsl(225, 65%, 55%);
	}
</style>
