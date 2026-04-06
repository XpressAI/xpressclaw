<script lang="ts">
	import {
		SvelteFlow,
		Background,
		BackgroundVariant,
		Controls,
		MiniMap,
		useSvelteFlow,
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

	const nodeTypes = { task: TaskNode, trigger: TriggerNode, sink: SinkNode };

	let nodes = $state.raw<Node[]>([]);
	let edges = $state.raw<Edge[]>([]);
	let workflow = $state<Workflow | null>(null);
	let selectedNodeId = $state<string | null>(null);
	let selectedEdgeId = $state<string | null>(null);
	let showYaml = $state(false);
	let yamlContent = $state('');
	let saving = $state(false);
	let running = $state(false);
	let agentList = $state<Agent[]>([]);
	let connectorList = $state<Connector[]>([]);
	let workflowName = $state('');
	let toast = $state<{ message: string; type: 'success' | 'error' } | null>(null);
	let dragType = $state<string | null>(null);

	let selectedNode = $derived(
		selectedNodeId ? nodes.find((n) => n.id === selectedNodeId) ?? null : null
	);
	let selectedEdge = $derived(
		selectedEdgeId ? edges.find((e) => e.id === selectedEdgeId) ?? null : null
	);

	// --- YAML ↔ Graph ---

	interface WfDef {
		name?: string; description?: string; version?: number;
		trigger?: { connector: string; channel: string; event: string; filter?: Record<string, unknown> };
		nodes: { id: string; label?: string; type?: string; agent?: string; prompt?: string; procedure?: string; sinks?: { connector: string; channel: string; template?: string }[]; position?: { x: number; y: number } }[];
		edges: { from: string; to: string; condition?: string; label?: string }[];
	}

	function yamlToGraph(yamlStr: string): { nodes: Node[]; edges: Edge[] } {
		let def: WfDef;
		try { def = yaml.load(yamlStr) as WfDef; } catch { return { nodes: [], edges: [] }; }
		if (!def?.nodes) return { nodes: [], edges: [] };

		const gn: Node[] = [];
		const ge: Edge[] = [];

		if (def.trigger) {
			gn.push({ id: '__trigger__', type: 'trigger', position: { x: 50, y: 150 },
				data: { connector: def.trigger.connector, channel: def.trigger.channel, event: def.trigger.event, filter: def.trigger.filter }
			});
			const targets = new Set(def.edges.map(e => e.to));
			for (const node of def.nodes) {
				if (!targets.has(node.id)) {
					ge.push({ id: `__trigger__-${node.id}`, source: '__trigger__', target: node.id, type: 'smoothstep', animated: true, style: edgeStyle('trigger') });
				}
			}
		}

		for (let i = 0; i < def.nodes.length; i++) {
			const n = def.nodes[i];
			const isSink = n.type === 'sink';
			const outEdges = def.edges.filter(e => e.from === n.id);
			const outputs = outEdges.length > 1 ? outEdges.map(e => e.condition || 'completed') : [];
			gn.push({ id: n.id, type: isSink ? 'sink' : 'task', position: n.position ?? { x: 300, y: i * 180 },
				data: { label: n.label ?? n.id, agent: n.agent, prompt: n.prompt, procedure: n.procedure, sinks: n.sinks ?? [], outputs }
			});
		}

		for (const e of def.edges) {
			const condLabel = e.label || (e.condition && e.condition !== 'completed' ? e.condition : undefined);
			const sourceOutEdges = def.edges.filter(ed => ed.from === e.from);
			const sourceHandle = sourceOutEdges.length > 1 ? e.condition || 'completed' : undefined;
			ge.push({ id: `${e.from}-${e.to}-${e.condition || 'completed'}`, source: e.from, target: e.to, sourceHandle, type: 'smoothstep',
				label: condLabel, style: edgeStyle(), data: { condition: e.condition || 'completed' }
			});
		}
		return { nodes: gn, edges: ge };
	}

	function edgeStyle(type?: string) {
		if (type === 'trigger') return 'stroke: hsl(142, 71%, 45%); stroke-width: 2;';
		return 'stroke: hsl(225, 25%, 35%); stroke-width: 2;';
	}

	function graphToYaml(gn: Node[], ge: Edge[]): string {
		const triggerNode = gn.find(n => n.type === 'trigger');
		const regularNodes = gn.filter(n => n.type !== 'trigger');
		const def: Record<string, unknown> = { name: workflowName || 'workflow', description: workflow?.description || '', version: workflow?.version ?? 1 };

		if (triggerNode) {
			const t: Record<string, unknown> = { connector: triggerNode.data.connector || '', channel: triggerNode.data.channel || '', event: triggerNode.data.event || '' };
			if (triggerNode.data.filter && typeof triggerNode.data.filter === 'object' && Object.keys(triggerNode.data.filter as object).length > 0) t.filter = triggerNode.data.filter;
			def.trigger = t;
		}

		def.nodes = regularNodes.map(n => {
			const node: Record<string, unknown> = { id: n.id, label: n.data.label || n.id, position: { x: Math.round(n.position.x), y: Math.round(n.position.y) } };
			if (n.type === 'sink') { node.type = 'sink'; if ((n.data.sinks as any[])?.length) node.sinks = n.data.sinks; }
			else { if (n.data.agent) node.agent = n.data.agent; if (n.data.prompt) node.prompt = n.data.prompt; if (n.data.procedure) node.procedure = n.data.procedure; }
			return node;
		});

		def.edges = ge.filter(e => e.source !== '__trigger__').map(e => {
			const edge: Record<string, unknown> = { from: e.source, to: e.target, condition: e.data?.condition || e.sourceHandle || 'completed' };
			if (e.label && e.label !== edge.condition) edge.label = e.label;
			return edge;
		});

		return yaml.dump(def, { lineWidth: -1, noRefs: true, quotingType: '"' });
	}

	// --- Data loading ---

	onMount(async () => {
		const id = $page.params.id!;
		try {
			const [wf, al, cl] = await Promise.all([
				workflows.get(id), agents.list().catch(() => []), connectors.list().catch(() => [])
			]);
			workflow = wf; workflowName = wf.name; yamlContent = wf.yaml_content; agentList = al; connectorList = cl;
			const graph = yamlToGraph(wf.yaml_content);
			nodes = graph.nodes; edges = graph.edges;
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
			const updatedYaml = graphToYaml(nodes, edges);
			yamlContent = updatedYaml;
			await workflows.update(workflow.id, { yaml_content: updatedYaml, description: workflow.description ?? undefined });
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
		try { const inst = await workflows.run(workflow.id); showToast(`Instance started: ${inst.id.slice(0, 8)}...`, 'success'); }
		catch (e) { showToast(`Run failed: ${e}`, 'error'); }
		running = false;
	}

	function applyYaml() {
		const graph = yamlToGraph(yamlContent);
		nodes = graph.nodes; edges = graph.edges; showYaml = false;
	}

	// --- Node/Edge events ---

	function handleNodeClick({ node }: { node: Node }) { selectedNodeId = node.id; selectedEdgeId = null; }
	function handleEdgeClick({ edge }: { edge: Edge }) { selectedEdgeId = edge.id; selectedNodeId = null; }
	function handlePaneClick() { selectedNodeId = null; selectedEdgeId = null; }

	/** When an existing node is dropped after dragging, check if it landed on an edge. */
	function handleNodeDragStop({ targetNode }: { targetNode: Node | null; nodes: Node[]; event: MouseEvent | TouchEvent }) {
		if (!targetNode) return;
		// Only insert if the node has no connections (both incoming and outgoing)
		const hasConnections = edges.some(e => e.source === targetNode.id || e.target === targetNode.id);
		if (!hasConnections) {
			insertNodeOnNearestEdge(targetNode.id);
		}
	}

	function handleConnect(connection: Connection) {
		const id = `${connection.source}-${connection.target}-${Date.now()}`;
		edges = [...edges, {
			id, source: connection.source, target: connection.target,
			sourceHandle: connection.sourceHandle ?? undefined, targetHandle: connection.targetHandle ?? undefined,
			type: 'smoothstep', style: edgeStyle(), data: { condition: 'completed' }, label: undefined
		}];
		selectedEdgeId = id; selectedNodeId = null;
	}

	// --- Drag-and-drop from sidebar ---

	let canvasEl: HTMLDivElement;
	let dragging = $state(false);

	function onDragStart(event: DragEvent, type: string) {
		if (!event.dataTransfer) return;
		event.dataTransfer.setData('text/plain', type);
		event.dataTransfer.effectAllowed = 'move';
		dragType = type;
		// Show overlay on next tick so it doesn't interfere with drag start
		requestAnimationFrame(() => { dragging = true; });
	}

	function onDragEnd() {
		dragging = false;
		dragType = null;
	}

	function onOverlayDragOver(event: DragEvent) {
		event.preventDefault();
		event.stopPropagation();
		if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
	}

	function onOverlayDragEnter(event: DragEvent) {
		event.preventDefault();
		event.stopPropagation();
	}

	function onOverlayDragLeave() {
		// Keep overlay visible — user might re-enter
	}

	function onOverlayDrop(event: DragEvent) {
		event.preventDefault();
		event.stopPropagation();
		dragging = false;

		const type = event.dataTransfer?.getData('text/plain') || dragType;
		if (!type) return;
		dragType = null;

		// Convert screen position to flow position using the canvas container
		const bounds = canvasEl.getBoundingClientRect();
		const x = event.clientX - bounds.left - 100;
		const y = event.clientY - bounds.top - 30;

		const id = `${type}_${Date.now().toString(36)}`;
		let newNode: Node;

		if (type === 'trigger') {
			if (nodes.some(n => n.type === 'trigger')) { showToast('Only one trigger allowed', 'error'); return; }
			newNode = { id: '__trigger__', type: 'trigger', position: { x, y }, data: { connector: '', channel: '', event: '' } };
		} else if (type === 'sink') {
			newNode = { id, type: 'sink', position: { x, y }, data: { label: 'Send Notification', sinks: [{ connector: '', channel: '', template: '' }] } };
		} else if (type === 'condition') {
			newNode = { id, type: 'task', position: { x, y }, data: { label: 'Decision', agent: '', prompt: 'Evaluate and decide: approve or reject.' } };
		} else if (type === 'human') {
			newNode = { id, type: 'task', position: { x, y }, data: { label: 'Human Review', agent: '', prompt: 'A human needs to review and decide.' } };
		} else {
			newNode = { id, type: 'task', position: { x, y }, data: { label: 'New Task', agent: '', prompt: '' } };
		}
		nodes = [...nodes, newNode];
		selectedNodeId = newNode.id; selectedEdgeId = null;

		// Try to insert into an edge if dropped near one
		if (type !== 'trigger') {
			insertNodeOnNearestEdge(newNode.id);
		}
	}

	/** Get the center point of a node. */
	function nodeCenter(n: Node): { x: number; y: number } {
		const w = 220; // all nodes are 220px wide
		const h = n.measured?.height ?? 100;
		return { x: n.position.x + w / 2, y: n.position.y + h / 2 };
	}

	/** Distance from point P to line segment AB. */
	function pointToSegmentDist(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
		const dx = bx - ax, dy = by - ay;
		const lenSq = dx * dx + dy * dy;
		if (lenSq === 0) return Math.hypot(px - ax, py - ay);
		let t = ((px - ax) * dx + (py - ay) * dy) / lenSq;
		t = Math.max(0, Math.min(1, t));
		return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
	}

	/** If the node is near an edge line, split that edge and insert the node. */
	function insertNodeOnNearestEdge(nodeId: string): boolean {
		const THRESHOLD = 80;
		const theNode = nodes.find(n => n.id === nodeId);
		if (!theNode) return false;

		// Don't insert if node already has connections
		if (edges.some(e => e.source === nodeId || e.target === nodeId)) return false;

		const nc = nodeCenter(theNode);

		let bestEdge: Edge | null = null;
		let bestDist = THRESHOLD;

		for (const edge of edges) {
			const src = nodes.find(n => n.id === edge.source);
			const tgt = nodes.find(n => n.id === edge.target);
			if (!src || !tgt) continue;

			const sc = nodeCenter(src);
			const tc = nodeCenter(tgt);
			const dist = pointToSegmentDist(nc.x, nc.y, sc.x, sc.y, tc.x, tc.y);
			if (dist < bestDist) {
				bestDist = dist;
				bestEdge = edge;
			}
		}

		if (!bestEdge) return false;

		// Capture values before mutating
		const srcId = bestEdge.source;
		const tgtId = bestEdge.target;
		const oldCondition = bestEdge.data?.condition || 'completed';
		const oldSourceHandle = bestEdge.sourceHandle;
		const edgeToRemove = bestEdge.id;
		const now = Date.now();

		// Remove ALL edges between source and target (not just by ID — handles
		// cases where edge IDs changed due to reconnection)
		const newEdgeId1 = `${srcId}-${nodeId}-${now}`;
		const newEdgeId2 = `${nodeId}-${tgtId}-${now + 1}`;
		edges = [
			...edges.filter(e => !(e.source === srcId && e.target === tgtId)),
			{
				id: newEdgeId1,
				source: srcId, target: nodeId,
				sourceHandle: oldSourceHandle,
				type: 'smoothstep', style: edgeStyle(),
				data: { condition: oldCondition }
			},
			{
				id: newEdgeId2,
				source: nodeId, target: tgtId,
				type: 'smoothstep', style: edgeStyle(),
				data: { condition: 'completed' }
			}
		];

		// Snap the node to the source node's x for clean alignment
		const srcNode = nodes.find(n => n.id === srcId);
		if (srcNode) {
			nodes = nodes.map(n => n.id !== nodeId ? n : { ...n, position: { ...n.position, x: srcNode!.position.x } });
		}
		return true;
	}

	// --- Edit helpers ---

	function updateNodeData(nodeId: string, updates: Record<string, unknown>) {
		nodes = nodes.map(n => n.id !== nodeId ? n : { ...n, data: { ...n.data, ...updates } });
	}

	function updateEdgeData(edgeId: string, updates: Partial<Edge>) {
		edges = edges.map(e => {
			if (e.id !== edgeId) return e;
			const updated = { ...e, ...updates };
			if (updates.data) updated.data = { ...e.data, ...updates.data };
			return updated;
		});
	}

	function updateSink(nodeId: string, idx: number, field: string, value: string) {
		const node = nodes.find(n => n.id === nodeId); if (!node) return;
		const sinks = [...((node.data.sinks as any[]) || [])];
		sinks[idx] = { ...sinks[idx], [field]: value };
		updateNodeData(nodeId, { sinks });
	}
	function addSinkEntry(nodeId: string) {
		const node = nodes.find(n => n.id === nodeId); if (!node) return;
		updateNodeData(nodeId, { sinks: [...((node.data.sinks as any[]) || []), { connector: '', channel: '', template: '' }] });
	}
	function removeSinkEntry(nodeId: string, idx: number) {
		const node = nodes.find(n => n.id === nodeId); if (!node) return;
		updateNodeData(nodeId, { sinks: ((node.data.sinks as any[]) || []).filter((_: any, i: number) => i !== idx) });
	}

	function deleteSelected() {
		if (selectedEdgeId) {
			edges = edges.filter(e => e.id !== selectedEdgeId);
			selectedEdgeId = null;
		} else if (selectedNodeId) {
			nodes = nodes.filter(n => n.id !== selectedNodeId);
			edges = edges.filter(e => e.source !== selectedNodeId && e.target !== selectedNodeId);
			selectedNodeId = null;
		}
	}

	function handleKeyDown(event: KeyboardEvent) {
		if (event.key === 'Delete' || event.key === 'Backspace') {
			const tag = (event.target as HTMLElement)?.tagName;
			if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
			deleteSelected();
		}
	}
</script>

<svelte:window onkeydown={handleKeyDown} />

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

		<button onclick={() => (showYaml = !showYaml)}
			class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {showYaml ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}">YAML</button>
		<button onclick={runWorkflow} disabled={running}
			class="rounded-md bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-700 disabled:opacity-50 flex items-center gap-1.5">
			<svg class="h-3 w-3" fill="currentColor" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg>
			{running ? 'Running...' : 'Run'}
		</button>
		<button onclick={save} disabled={saving}
			class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving...' : 'Save'}</button>
	</div>

	<!-- Main area -->
	<div class="flex flex-1 overflow-hidden">
		<!-- Left sidebar: Node palette -->
		<div class="w-52 flex-shrink-0 border-r border-border bg-card overflow-y-auto">
			<div class="px-3 pt-3 pb-2">
				<div class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground mb-2">Drag to add</div>
			</div>

			<div class="px-2 pb-3 space-y-1.5">
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div draggable="true" ondragstart={(e) => onDragStart(e, 'task')} ondragend={onDragEnd}
					class="flex items-center gap-2.5 rounded-lg border border-border/50 bg-background px-3 py-2.5 cursor-grab active:cursor-grabbing hover:border-primary/40 transition-colors">
					<div class="flex h-7 w-7 items-center justify-center rounded-full bg-[hsl(225,50%,25%)] text-[10px] font-bold text-[hsl(220,20%,92%)]">T</div>
					<div>
						<div class="text-xs font-medium text-foreground">Task</div>
						<div class="text-[10px] text-muted-foreground">Agent performs work</div>
					</div>
				</div>

				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div draggable="true" ondragstart={(e) => onDragStart(e, 'trigger')} ondragend={onDragEnd}
					class="flex items-center gap-2.5 rounded-lg border border-emerald-800/30 bg-emerald-950/20 px-3 py-2.5 cursor-grab active:cursor-grabbing hover:border-emerald-600/40 transition-colors">
					<div class="flex h-7 w-7 items-center justify-center rounded-lg bg-emerald-600/20">
						<svg class="h-3.5 w-3.5 text-emerald-400" fill="currentColor" viewBox="0 0 24 24"><path d="M13 2L3 14h9l-1 10 10-12h-9l1-10z" /></svg>
					</div>
					<div>
						<div class="text-xs font-medium text-emerald-300">Trigger</div>
						<div class="text-[10px] text-muted-foreground">Start from event</div>
					</div>
				</div>

				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div draggable="true" ondragstart={(e) => onDragStart(e, 'sink')} ondragend={onDragEnd}
					class="flex items-center gap-2.5 rounded-lg border border-blue-800/30 bg-blue-950/20 px-3 py-2.5 cursor-grab active:cursor-grabbing hover:border-blue-600/40 transition-colors">
					<div class="flex h-7 w-7 items-center justify-center rounded-lg bg-blue-600/20">
						<svg class="h-3.5 w-3.5 text-blue-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 12L3.269 3.126A59.768 59.768 0 0121.485 12 59.77 59.77 0 013.27 20.876L5.999 12zm0 0h7.5" /></svg>
					</div>
					<div>
						<div class="text-xs font-medium text-blue-300">Sink</div>
						<div class="text-[10px] text-muted-foreground">Send notification</div>
					</div>
				</div>

				<div class="pt-2 pb-1">
					<div class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Templates</div>
				</div>

				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div draggable="true" ondragstart={(e) => onDragStart(e, 'condition')} ondragend={onDragEnd}
					class="flex items-center gap-2.5 rounded-lg border border-amber-800/30 bg-amber-950/20 px-3 py-2.5 cursor-grab active:cursor-grabbing hover:border-amber-600/40 transition-colors">
					<div class="flex h-7 w-7 items-center justify-center rounded-lg bg-amber-600/20">
						<svg class="h-3.5 w-3.5 text-amber-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M9.879 7.519c1.171-1.025 3.071-1.025 4.242 0 1.172 1.025 1.172 2.687 0 3.712-.203.179-.43.326-.67.442-.745.361-1.45.999-1.45 1.827v.75M21 12a9 9 0 11-18 0 9 9 0 0118 0zm-9 5.25h.008v.008H12v-.008z" /></svg>
					</div>
					<div>
						<div class="text-xs font-medium text-amber-300">Decision</div>
						<div class="text-[10px] text-muted-foreground">Approve/reject branch</div>
					</div>
				</div>

				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div draggable="true" ondragstart={(e) => onDragStart(e, 'human')} ondragend={onDragEnd}
					class="flex items-center gap-2.5 rounded-lg border border-purple-800/30 bg-purple-950/20 px-3 py-2.5 cursor-grab active:cursor-grabbing hover:border-purple-600/40 transition-colors">
					<div class="flex h-7 w-7 items-center justify-center rounded-lg bg-purple-600/20">
						<svg class="h-3.5 w-3.5 text-purple-400" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 6a3.75 3.75 0 11-7.5 0 3.75 3.75 0 017.5 0zM4.501 20.118a7.5 7.5 0 0114.998 0" /></svg>
					</div>
					<div>
						<div class="text-xs font-medium text-purple-300">Human</div>
						<div class="text-[10px] text-muted-foreground">Human review step</div>
					</div>
				</div>
			</div>

			{#if nodes.length > 0}
				<div class="border-t border-border px-3 py-3">
					<div class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground mb-2">Nodes ({nodes.length})</div>
					<div class="space-y-0.5">
						{#each nodes as n}
							<button onclick={() => { selectedNodeId = n.id; selectedEdgeId = null; }}
								class="w-full text-left rounded px-2 py-1 text-xs transition-colors truncate
									{selectedNodeId === n.id ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:text-foreground hover:bg-accent'}">
								{n.data.label || n.id}
							</button>
						{/each}
					</div>
				</div>
			{/if}
		</div>

		<!-- Canvas -->
		<div class="flex-1 relative" bind:this={canvasEl}>
			<!-- Drop overlay: sits above SvelteFlow during drag so WebKit drop events fire -->
			{#if dragging}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div class="absolute inset-0 z-10"
					ondragover={onOverlayDragOver}
					ondragenter={onOverlayDragEnter}
					ondragleave={onOverlayDragLeave}
					ondrop={onOverlayDrop}></div>
			{/if}
			<SvelteFlow bind:nodes bind:edges {nodeTypes} fitView snapGrid={[15, 15]}
				defaultEdgeOptions={{ type: 'smoothstep' }}
				onnodeclick={handleNodeClick} onedgeclick={handleEdgeClick}
				onpaneclick={handlePaneClick} onconnect={handleConnect}
				onnodedragstop={handleNodeDragStop}>
				<Background variant={BackgroundVariant.Dots} gap={20} size={1} />
				<Controls position="bottom-left" />
				<MiniMap position="bottom-right"
					nodeColor={(node) => {
						if (node.type === 'trigger') return 'hsl(142, 71%, 45%)';
						if (node.type === 'sink') return 'hsl(217, 91%, 60%)';
						return 'hsl(225, 65%, 55%)';
					}} />
			</SvelteFlow>

			<!-- YAML overlay -->
			{#if showYaml}
				<div class="absolute inset-0 z-20 flex flex-col bg-background/95 backdrop-blur-sm">
					<div class="flex items-center justify-between px-4 py-2 border-b border-border">
						<span class="text-xs font-medium text-muted-foreground">YAML Editor</span>
						<div class="flex gap-2">
							<button onclick={applyYaml} class="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90">Apply</button>
							<button onclick={() => { yamlContent = graphToYaml(nodes, edges); }} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Refresh from Canvas</button>
							<button onclick={() => (showYaml = false)} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Close</button>
						</div>
					</div>
					<textarea bind:value={yamlContent}
						class="flex-1 w-full bg-transparent p-4 font-mono text-xs text-foreground resize-none focus:outline-none" spellcheck="false"></textarea>
				</div>
			{/if}

			<!-- Empty state hint -->
			{#if nodes.length === 0}
				<div class="absolute inset-0 flex items-center justify-center pointer-events-none z-10">
					<div class="text-center text-muted-foreground/50 space-y-2">
						<svg class="h-12 w-12 mx-auto" fill="none" stroke="currentColor" stroke-width="1" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" /></svg>
						<div class="text-sm">Drag nodes from the sidebar to build your workflow</div>
					</div>
				</div>
			{/if}
		</div>

		<!-- Right panel: Properties -->
		{#if selectedNode || selectedEdge}
			<div class="w-72 flex-shrink-0 border-l border-border bg-card overflow-y-auto">
				<div class="flex items-center justify-between border-b border-border px-4 py-3">
					<h3 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						{#if selectedNode}
							{selectedNode.type === 'trigger' ? 'Trigger' : selectedNode.type === 'sink' ? 'Sink' : 'Task'} Properties
						{:else}
							Edge Properties
						{/if}
					</h3>
					<div class="flex items-center gap-1">
						<button onclick={deleteSelected} class="rounded p-1 text-muted-foreground hover:text-destructive hover:bg-destructive/10" title="Delete">
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M14.74 9l-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 01-2.244 2.077H8.084a2.25 2.25 0 01-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 00-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 013.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 00-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 00-7.5 0" /></svg>
						</button>
						<button onclick={() => { selectedNodeId = null; selectedEdgeId = null; }} class="rounded p-1 text-muted-foreground hover:text-foreground" title="Close">
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
						</button>
					</div>
				</div>

				<div class="p-4 space-y-4">
					{#if selectedEdge}
						<!-- Edge properties -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Condition</label>
							<select value={selectedEdge.data?.condition || 'completed'}
								onchange={(e) => updateEdgeData(selectedEdge!.id, { data: { condition: e.currentTarget.value }, label: e.currentTarget.value !== 'completed' ? e.currentTarget.value : undefined })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring">
								<option value="completed">completed</option>
								<option value="failed">failed</option>
								<option value="default">default (always)</option>
							</select>
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Custom Condition</label>
							<input type="text" value={selectedEdge.data?.condition || ''}
								oninput={(e) => updateEdgeData(selectedEdge!.id, { data: { condition: e.currentTarget.value }, label: e.currentTarget.value !== 'completed' ? e.currentTarget.value : undefined })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder='output.verdict == "pass"' />
							<div class="mt-1.5 text-[10px] text-muted-foreground/70 space-y-0.5">
								<div><code class="bg-muted px-1 rounded">completed</code> — task finished</div>
								<div><code class="bg-muted px-1 rounded">failed</code> — task failed</div>
								<div><code class="bg-muted px-1 rounded">output.field == "val"</code></div>
								<div><code class="bg-muted px-1 rounded">output contains "text"</code></div>
								<div><code class="bg-muted px-1 rounded">default</code> — fallback</div>
							</div>
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
							<input type="text" value={selectedEdge.label ?? ''}
								oninput={(e) => updateEdgeData(selectedEdge!.id, { label: e.currentTarget.value || undefined })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder="Edge label (optional)" />
						</div>
						<div class="text-[10px] text-muted-foreground">
							{edges.find(e => e.id === selectedEdgeId)?.source} &rarr; {edges.find(e => e.id === selectedEdgeId)?.target}
						</div>

					{:else if selectedNode?.type === 'task'}
						<!-- Task node -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
							<input type="text" value={selectedNode.data.label || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { label: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring" />
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Agent</label>
							<select value={selectedNode.data.agent || ''}
								onchange={(e) => updateNodeData(selectedNode!.id, { agent: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring">
								<option value="">Select agent...</option>
								{#each agentList as agent}
									<option value={agent.name}>{agent.config?.display_name || agent.name}</option>
								{/each}
							</select>
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Prompt</label>
							<textarea value={(selectedNode.data.prompt as string) || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { prompt: e.currentTarget.value })}
								rows="6" class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs font-mono resize-none focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder={'What should this agent do?\n\nUse {{trigger.payload.field}} for trigger data\nUse {{nodes.step1.output}} for previous output'}></textarea>
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Procedure (optional)</label>
							<input type="text" value={selectedNode.data.procedure || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { procedure: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring" placeholder="procedure-name" />
						</div>

					{:else if selectedNode?.type === 'sink'}
						<!-- Sink node -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Label</label>
							<input type="text" value={selectedNode.data.label || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { label: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring" />
						</div>
						<div class="space-y-3">
							<div class="flex items-center justify-between">
								<label class="text-[10px] font-medium text-muted-foreground">Sinks</label>
								<button onclick={() => addSinkEntry(selectedNode!.id)} class="rounded px-1.5 py-0.5 text-[10px] text-primary hover:bg-primary/10">+ Add</button>
							</div>
							{#each ((selectedNode.data.sinks || []) as any[]) as sink, i}
								<div class="rounded-md border border-border p-2.5 space-y-2 relative">
									<button onclick={() => removeSinkEntry(selectedNode!.id, i)} class="absolute top-1.5 right-1.5 text-muted-foreground hover:text-destructive text-xs">x</button>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Connector</label>
										<select value={sink.connector} onchange={(e) => updateSink(selectedNode!.id, i, 'connector', e.currentTarget.value)}
											class="w-full rounded border border-input bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring">
											<option value="">Select...</option>
											{#each connectorList as c}<option value={c.name}>{c.name} ({c.connector_type})</option>{/each}
										</select>
									</div>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Channel</label>
										<input type="text" value={sink.channel} oninput={(e) => updateSink(selectedNode!.id, i, 'channel', e.currentTarget.value)}
											class="w-full rounded border border-input bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring" placeholder="channel-name" />
									</div>
									<div>
										<label class="block text-[10px] text-muted-foreground mb-0.5">Template</label>
										<textarea value={sink.template || ''} oninput={(e) => updateSink(selectedNode!.id, i, 'template', e.currentTarget.value)}
											rows="2" class="w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono resize-none focus:outline-none focus:ring-1 focus:ring-ring" placeholder="Message template..."></textarea>
									</div>
								</div>
							{/each}
						</div>

					{:else if selectedNode?.type === 'trigger'}
						<!-- Trigger node -->
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Connector</label>
							<select value={selectedNode.data.connector || ''}
								onchange={(e) => updateNodeData(selectedNode!.id, { connector: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring">
								<option value="">Select...</option>
								{#each connectorList as c}<option value={c.name}>{c.name} ({c.connector_type})</option>{/each}
								<option value="webhook">webhook</option>
								<option value="jira">jira</option>
								<option value="github">github</option>
								<option value="telegram">telegram</option>
								<option value="file_watcher">file_watcher</option>
							</select>
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Channel</label>
							<input type="text" value={selectedNode.data.channel || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { channel: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring" placeholder="channel-name" />
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Event</label>
							<input type="text" value={selectedNode.data.event || ''}
								oninput={(e) => updateNodeData(selectedNode!.id, { event: e.currentTarget.value })}
								class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs focus:outline-none focus:ring-2 focus:ring-ring" placeholder="e.g. issue_created" />
						</div>
						<div>
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Filter (JSON)</label>
							<textarea value={selectedNode.data.filter ? JSON.stringify(selectedNode.data.filter, null, 2) : ''}
								oninput={(e) => { try { updateNodeData(selectedNode!.id, { filter: JSON.parse(e.currentTarget.value || '{}') }); } catch {} }}
								rows="3" class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-xs font-mono resize-none focus:outline-none focus:ring-2 focus:ring-ring"
								placeholder={'{"type": "Story"}'}></textarea>
						</div>
					{/if}

					<!-- Node ID -->
					{#if selectedNode}
						<div class="pt-2 border-t border-border">
							<label class="block text-[10px] font-medium text-muted-foreground mb-1">Node ID</label>
							<div class="rounded-md bg-muted px-2.5 py-1.5 text-xs font-mono text-muted-foreground">{selectedNode.id}</div>
						</div>
					{/if}
				</div>
			</div>
		{/if}
	</div>

	<!-- Toast -->
	{#if toast}
		<div class="fixed bottom-4 right-4 z-50 rounded-lg border px-4 py-2.5 text-xs font-medium shadow-lg
			{toast.type === 'success' ? 'border-emerald-800/60 bg-emerald-950/80 text-emerald-300' : 'border-red-800/60 bg-red-950/80 text-red-300'}">
			{toast.message}
		</div>
	{/if}
</div>

<style>
	/* WebKit drag-and-drop compatibility */
	[draggable="true"] {
		-webkit-user-drag: element;
		user-select: none;
	}
	:global(.svelte-flow) {
		--xy-background-color: hsl(228, 22%, 8%) !important;
		--xy-node-border-radius: 0.5rem;
		--xy-edge-stroke: hsl(225, 25%, 35%);
		--xy-edge-stroke-selected: hsl(225, 65%, 55%);
		--xy-edge-stroke-width: 2;
		--xy-connectionline-stroke: hsl(225, 65%, 55%);
		--xy-connectionline-stroke-width: 2;
		--xy-attribution-background-color: transparent;
	}
	:global(.svelte-flow__background) {
		background-color: hsl(228, 22%, 8%) !important;
	}
	:global(.svelte-flow__renderer) {
		background-color: hsl(228, 22%, 8%) !important;
	}
	:global(.svelte-flow .svelte-flow__node) { border: none; background: none; padding: 0; border-radius: 0; box-shadow: none; }
	:global(.svelte-flow .svelte-flow__node.selected) { outline: 2px solid hsl(225, 65%, 55%); outline-offset: 2px; border-radius: 0.5rem; }
	:global(.svelte-flow .svelte-flow__edge.selected .svelte-flow__edge-path) { stroke: hsl(225, 65%, 55%); }
	:global(.svelte-flow .svelte-flow__edge-text) { font-size: 10px; }
	:global(.svelte-flow .svelte-flow__minimap) { background: hsl(228, 22%, 11%); border: 1px solid hsl(225, 18%, 18%); border-radius: 0.375rem; }
	:global(.svelte-flow .svelte-flow__controls) { background: hsl(228, 22%, 11%); border: 1px solid hsl(225, 18%, 18%); border-radius: 0.375rem; box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.3); }
	:global(.svelte-flow .svelte-flow__controls button) { background: hsl(228, 22%, 11%); border-color: hsl(225, 18%, 18%); color: hsl(225, 15%, 55%); }
	:global(.svelte-flow .svelte-flow__controls button:hover) { background: hsl(225, 50%, 25%); color: hsl(220, 20%, 92%); }
	:global(.svelte-flow .svelte-flow__controls button svg) { fill: currentColor; }
	:global(.svelte-flow .svelte-flow__background) { opacity: 0.4; }
	:global(.svelte-flow .svelte-flow__edge-path) { stroke: hsl(225, 25%, 35%); }
	:global(.svelte-flow .svelte-flow__pane) { background-color: hsl(228, 22%, 8%) !important; }
</style>
