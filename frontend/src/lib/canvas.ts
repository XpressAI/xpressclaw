import type { Agent, Conversation, Project, Task } from '$lib/api';

export type WorkspaceViewMode = 'tabs' | 'canvas';

export type CanvasNodeKind =
	| 'project'
	| 'task'
	| 'conversation'
	| 'agent'
	| 'editor'
	| 'terminal'
	| 'image'
	| 'app'
	| 'new-work';

export interface CanvasNode {
	id: string;
	kind: CanvasNodeKind;
	title: string;
	x: number;
	y: number;
	width: number;
	height: number;
	zIndex: number;
	collapsed: boolean;
	parentId: string | null;
	edgeLabel: string | null;
	projectId: string | null;
	agentId: string | null;
	taskId: string | null;
	conversationId: string | null;
	filePath: string | null;
	terminalSession: string | null;
	imageUrl: string | null;
	imageSource: 'workspace' | 'url' | null;
	appUrl: string | null;
	appSource: 'port' | 'workspace-html' | 'url' | null;
	appFilePath: string | null;
	route: string | null;
}

export interface CanvasCamera {
	x: number;
	y: number;
	zoom: number;
}

export interface CanvasState {
	nodes: CanvasNode[];
	camera: CanvasCamera;
	focusedNodeId: string | null;
	snapToGrid: boolean;
	showMinimap: boolean;
	autoPinDelegated: boolean;
}

export interface CanvasEdge {
	id: string;
	fromId: string;
	toId: string;
	kind: 'context' | 'delegation' | 'dependency';
	label: string | null;
	status: string | null;
}

export interface CanvasProjectZone {
	projectId: string;
	name: string;
	icon: string | null;
	x: number;
	y: number;
	width: number;
	height: number;
	nodeCount: number;
}

export const CANVAS_GRID_SIZE = 20;
export const CANVAS_MIN_ZOOM = 0.25;
export const CANVAS_MAX_ZOOM = 1.6;
export const CANVAS_COLLAPSED_HEIGHT = 44;

export const CANVAS_DEFAULT_SIZE: Record<CanvasNodeKind, { width: number; height: number; minWidth: number; minHeight: number }> = {
	project: { width: 380, height: 480, minWidth: 300, minHeight: 260 },
	task: { width: 520, height: 600, minWidth: 360, minHeight: 300 },
	conversation: { width: 520, height: 580, minWidth: 360, minHeight: 300 },
	agent: { width: 560, height: 560, minWidth: 380, minHeight: 300 },
	editor: { width: 640, height: 520, minWidth: 380, minHeight: 280 },
	terminal: { width: 560, height: 380, minWidth: 360, minHeight: 240 },
	image: { width: 460, height: 420, minWidth: 280, minHeight: 240 },
	app: { width: 620, height: 520, minWidth: 340, minHeight: 280 },
	'new-work': { width: 520, height: 460, minWidth: 360, minHeight: 320 },
};

const VALID_KINDS = new Set<CanvasNodeKind>([
	'project',
	'task',
	'conversation',
	'agent',
	'editor',
	'terminal',
	'image',
	'app',
	'new-work',
]);

const IMAGE_EXTENSIONS = new Set([
	'png',
	'jpg',
	'jpeg',
	'gif',
	'webp',
	'svg',
	'bmp',
	'ico',
	'avif',
]);

const HTML_EXTENSIONS = new Set(['html', 'htm']);

let canvasIdCounter = 0;

export function createCanvasNodeId(kind: CanvasNodeKind): string {
	canvasIdCounter += 1;
	return `canvas-${kind}-${Date.now().toString(36)}-${canvasIdCounter.toString(36)}`;
}

export function clampZoom(zoom: number): number {
	if (!Number.isFinite(zoom)) return 1;
	return Math.min(CANVAS_MAX_ZOOM, Math.max(CANVAS_MIN_ZOOM, Math.round(zoom * 100) / 100));
}

export function snapCoordinate(value: number, enabled: boolean): number {
	return enabled ? Math.round(value / CANVAS_GRID_SIZE) * CANVAS_GRID_SIZE : Math.round(value);
}

export function nodeVisualHeight(node: CanvasNode): number {
	return node.collapsed ? CANVAS_COLLAPSED_HEIGHT : node.height;
}

export function isImageFilePath(path: string | null | undefined): boolean {
	if (!path) return false;
	const clean = path.split('?')[0].split('#')[0];
	const dot = clean.lastIndexOf('.');
	if (dot < 0) return false;
	return IMAGE_EXTENSIONS.has(clean.slice(dot + 1).toLowerCase());
}

export function isHtmlFilePath(path: string | null | undefined): boolean {
	if (!path) return false;
	const clean = path.split('?')[0].split('#')[0];
	const dot = clean.lastIndexOf('.');
	if (dot < 0) return false;
	return HTML_EXTENSIONS.has(clean.slice(dot + 1).toLowerCase());
}

export interface ClassifiedAppUrl {
	valid: boolean;
	normalizedUrl: string;
	sameOrigin: boolean;
	blockedReason: string | null;
	sandbox: string;
}

export function classifyAppUrl(raw: string, currentOrigin: string): ClassifiedAppUrl {
	const trimmed = raw.trim();
	if (!trimmed) {
		return {
			valid: false,
			normalizedUrl: '',
			sameOrigin: false,
			blockedReason: 'Enter an http:// or https:// URL.',
			sandbox: 'allow-scripts allow-forms',
		};
	}
	let parsed: URL;
	try {
		parsed = new URL(trimmed);
	} catch {
		return {
			valid: false,
			normalizedUrl: '',
			sameOrigin: false,
			blockedReason: 'Enter a valid URL (for example http://127.0.0.1:3000).',
			sandbox: 'allow-scripts allow-forms',
		};
	}
	if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
		return {
			valid: false,
			normalizedUrl: '',
			sameOrigin: false,
			blockedReason: 'Only http:// and https:// URLs can be embedded.',
			sandbox: 'allow-scripts allow-forms',
		};
	}
	const sameOrigin = Boolean(currentOrigin) && parsed.origin === currentOrigin;
	if (sameOrigin && (parsed.pathname === '/api' || parsed.pathname.startsWith('/api/'))) {
		return {
			valid: false,
			normalizedUrl: '',
			sameOrigin: true,
			blockedReason: 'Control-plane API endpoints cannot be embedded as an app.',
			sandbox: 'allow-scripts allow-forms',
		};
	}
	// Never combine allow-same-origin and allow-scripts when embedding the same
	// origin as XpressClaw itself; cross-origin local ports (e.g. :3000 vs :8935)
	// live in their own browser origin and need allow-same-origin for SPA routing/storage.
	const sandbox = sameOrigin
		? 'allow-scripts allow-forms'
		: 'allow-scripts allow-forms allow-same-origin allow-popups';
	return {
		valid: true,
		normalizedUrl: parsed.toString(),
		sameOrigin,
		blockedReason: null,
		sandbox,
	};
}

export function createCanvasNode(
	partial: Partial<CanvasNode> & Pick<CanvasNode, 'kind' | 'title'>,
	existingNodes: CanvasNode[],
	anchor?: { x: number; y: number } | null,
): CanvasNode {
	const defaults = CANVAS_DEFAULT_SIZE[partial.kind];
	const width = partial.width ?? defaults.width;
	const height = partial.height ?? defaults.height;
	const parent = partial.parentId ? existingNodes.find((candidate) => candidate.id === partial.parentId) ?? null : null;
	const placed = partial.x !== undefined && partial.y !== undefined
		? { x: partial.x, y: partial.y }
		: findPlacementPosition(existingNodes, width, height, parent, anchor);
	const maxZ = existingNodes.reduce((highest, node) => Math.max(highest, node.zIndex), 0);

	return {
		id: partial.id ?? createCanvasNodeId(partial.kind),
		kind: partial.kind,
		title: partial.title,
		x: placed.x,
		y: placed.y,
		width,
		height,
		zIndex: partial.zIndex ?? maxZ + 1,
		collapsed: partial.collapsed ?? false,
		parentId: partial.parentId ?? null,
		edgeLabel: partial.edgeLabel ?? null,
		projectId: partial.projectId ?? parent?.projectId ?? null,
		agentId: partial.agentId ?? parent?.agentId ?? null,
		taskId: partial.taskId ?? null,
		conversationId: partial.conversationId ?? null,
		filePath: partial.filePath ?? null,
		terminalSession: partial.terminalSession ?? null,
		imageUrl: partial.imageUrl ?? null,
		imageSource: partial.imageSource ?? null,
		appUrl: partial.appUrl ?? null,
		appSource: partial.appSource ?? null,
		appFilePath: partial.appFilePath ?? null,
		route: partial.route ?? null,
	};
}

function rectsOverlap(
	ax: number,
	ay: number,
	aw: number,
	ah: number,
	bx: number,
	by: number,
	bw: number,
	bh: number,
	gap = 24,
): boolean {
	return ax < bx + bw + gap
		&& ax + aw + gap > bx
		&& ay < by + bh + gap
		&& ay + ah + gap > by;
}

export function findPlacementPosition(
	existingNodes: CanvasNode[],
	width: number,
	height: number,
	parent: CanvasNode | null,
	anchor?: { x: number; y: number } | null,
): { x: number; y: number } {
	const horizontalGap = 72;
	const verticalGap = 36;
	const startX = parent
		? Math.round(parent.x + parent.width + horizontalGap)
		: Math.round(anchor?.x ?? 80);
	const startY = parent
		? Math.round(parent.y)
		: Math.round(anchor?.y ?? 80);

	const collides = (x: number, y: number) =>
		existingNodes.some((node) =>
			rectsOverlap(x, y, width, height, node.x, node.y, node.width, nodeVisualHeight(node), 20)
		);

	if (!collides(startX, startY)) {
		return { x: snapCoordinate(startX, true), y: snapCoordinate(startY, true) };
	}

	// GameMaker-style fan: scan vertically in the column beside the parent first,
	// then step right into another column if the lane is packed.
	for (let col = 0; col < 6; col += 1) {
		const candidateX = startX + col * (width + horizontalGap);
		for (let row = 0; row < 12; row += 1) {
			const offset = row === 0 ? 0 : row % 2 === 1 ? Math.ceil(row / 2) : -Math.ceil(row / 2);
			const candidateY = startY + offset * (Math.min(height, 420) + verticalGap);
			if (candidateY < -400) continue;
			if (!collides(candidateX, candidateY)) {
				return { x: snapCoordinate(candidateX, true), y: snapCoordinate(candidateY, true) };
			}
		}
	}

	return {
		x: snapCoordinate(startX + existingNodes.length * 40, true),
		y: snapCoordinate(startY + existingNodes.length * 40, true),
	};
}

export function deriveCanvasEdges(
	nodes: CanvasNode[],
	tasks: Task[],
	agents: Agent[],
	conversations: Conversation[],
): CanvasEdge[] {
	const nodeById = new Map(nodes.map((node) => [node.id, node]));
	const taskNodeByTaskId = new Map<string, CanvasNode>();
	const projectNodeByProjectId = new Map<string, CanvasNode>();
	const agentNodeByAgentId = new Map<string, CanvasNode>();
	const taskById = new Map(tasks.map((task) => [task.id, task]));
	const agentById = new Map(agents.map((agent) => [agent.id, agent]));
	const conversationById = new Map(conversations.map((conversation) => [conversation.id, conversation]));

	for (const node of nodes) {
		if (node.kind === 'task' && node.taskId) taskNodeByTaskId.set(node.taskId, node);
		if (node.kind === 'project' && node.projectId) projectNodeByProjectId.set(node.projectId, node);
		if (node.kind === 'agent' && node.agentId) agentNodeByAgentId.set(node.agentId, node);
	}

	const edges: CanvasEdge[] = [];
	const seen = new Set<string>();
	const pushEdge = (edge: CanvasEdge) => {
		const key = `${edge.fromId}->${edge.toId}:${edge.kind}`;
		if (edge.fromId === edge.toId || seen.has(key)) return;
		seen.add(key);
		edges.push(edge);
	};

	// 1. Live task delegation (parent_task_id) & dependencies (depends_on).
	const linkedTaskTargets = new Set<string>();
	for (const node of nodes) {
		if (node.kind !== 'task' || !node.taskId) continue;
		const task = taskById.get(node.taskId);
		if (!task) continue;
		if (task.parent_task_id) {
			const parentNode = taskNodeByTaskId.get(task.parent_task_id);
			if (parentNode) {
				linkedTaskTargets.add(node.id);
				pushEdge({
					id: `delegation:${parentNode.id}->${node.id}`,
					fromId: parentNode.id,
					toId: node.id,
					kind: 'delegation',
					label: task.blocks_parent ? 'delegated · blocking' : 'delegated',
					status: task.activity_status ?? task.status,
				});
			}
		}
		for (const depId of task.depends_on ?? []) {
			const depNode = taskNodeByTaskId.get(depId);
			if (depNode) {
				pushEdge({
					id: `dependency:${depNode.id}->${node.id}`,
					fromId: depNode.id,
					toId: node.id,
					kind: 'dependency',
					label: 'depends on',
					status: task.activity_status ?? task.status,
				});
			}
		}
	}

	// 2. Explicit parentId contextual tethers (editors, terminals, images, apps, spawned views).
	for (const node of nodes) {
		if (node.parentId && nodeById.has(node.parentId)) {
			if (node.kind === 'task' && linkedTaskTargets.has(node.id)) {
				const parent = nodeById.get(node.parentId)!;
				const task = node.taskId ? taskById.get(node.taskId) : null;
				if (parent.kind === 'task' && parent.taskId && parent.taskId === task?.parent_task_id) {
					continue;
				}
			}
			const task = node.taskId ? taskById.get(node.taskId) : null;
			pushEdge({
				id: `context:${node.parentId}->${node.id}`,
				fromId: node.parentId,
				toId: node.id,
				kind: 'context',
				label: node.edgeLabel ?? defaultEdgeLabel(node.kind),
				status: task ? (task.activity_status ?? task.status) : null,
			});
			continue;
		}

		// 3. Implicit project/agent context tethers when both are visible on the canvas.
		if (node.kind === 'project') continue;
		if (node.kind === 'task' && linkedTaskTargets.has(node.id)) continue;

		const inferredProjectId = node.projectId
			?? (node.taskId ? inferTaskProjectId(taskById.get(node.taskId), agentById, conversationById) : null)
			?? (node.conversationId ? conversationById.get(node.conversationId)?.project_id ?? null : null)
			?? (node.agentId ? agentById.get(node.agentId)?.project_id ?? null : null);

		if ((node.kind === 'editor' || node.kind === 'terminal' || node.kind === 'image' || node.kind === 'app') && node.agentId) {
			const agentNode = agentNodeByAgentId.get(node.agentId);
			if (agentNode) {
				pushEdge({
					id: `context:${agentNode.id}->${node.id}`,
					fromId: agentNode.id,
					toId: node.id,
					kind: 'context',
					label: node.edgeLabel ?? defaultEdgeLabel(node.kind),
					status: null,
				});
				continue;
			}
		}

		if (inferredProjectId) {
			const projectNode = projectNodeByProjectId.get(inferredProjectId);
			if (projectNode) {
				const task = node.taskId ? taskById.get(node.taskId) : null;
				pushEdge({
					id: `context:${projectNode.id}->${node.id}`,
					fromId: projectNode.id,
					toId: node.id,
					kind: 'context',
					label: node.edgeLabel ?? defaultEdgeLabel(node.kind),
					status: task ? (task.activity_status ?? task.status) : null,
				});
			}
		}
	}

	return edges;
}

function defaultEdgeLabel(kind: CanvasNodeKind): string | null {
	if (kind === 'editor') return 'editor';
	if (kind === 'terminal') return 'terminal';
	if (kind === 'image') return 'image';
	if (kind === 'app') return 'app';
	if (kind === 'conversation') return 'chat';
	if (kind === 'agent') return 'agent';
	return null;
}

export function inferTaskProjectId(
	task: Task | undefined,
	agentById: Map<string, Agent>,
	conversationById: Map<string, Conversation>,
): string | null {
	if (!task) return null;
	if (task.project_id) return task.project_id;
	if (task.conversation_id) {
		const conv = conversationById.get(task.conversation_id);
		if (conv?.project_id) return conv.project_id;
	}
	if (task.agent_id) {
		return agentById.get(task.agent_id)?.project_id ?? null;
	}
	return null;
}

export function computeProjectZones(
	nodes: CanvasNode[],
	projects: Project[],
): CanvasProjectZone[] {
	const projectById = new Map(projects.map((project) => [project.id, project]));
	const grouped = new Map<string, CanvasNode[]>();
	for (const node of nodes) {
		if (!node.projectId) continue;
		const list = grouped.get(node.projectId) ?? [];
		list.push(node);
		grouped.set(node.projectId, list);
	}
	const zones: CanvasProjectZone[] = [];
	const padX = 36;
	const padTop = 52;
	const padBottom = 32;
	for (const [projectId, group] of grouped) {
		if (group.length < 2) continue;
		const project = projectById.get(projectId);
		if (!project) continue;
		let minX = Infinity;
		let minY = Infinity;
		let maxX = -Infinity;
		let maxY = -Infinity;
		for (const node of group) {
			minX = Math.min(minX, node.x);
			minY = Math.min(minY, node.y);
			maxX = Math.max(maxX, node.x + node.width);
			maxY = Math.max(maxY, node.y + nodeVisualHeight(node));
		}
		zones.push({
			projectId,
			name: project.name,
			icon: project.icon,
			x: minX - padX,
			y: minY - padTop,
			width: maxX - minX + padX * 2,
			height: maxY - minY + padTop + padBottom,
			nodeCount: group.length,
		});
	}
	return zones;
}

export function tidyCanvasLayout(
	nodes: CanvasNode[],
	tasks: Task[],
	projects: Project[],
): CanvasNode[] {
	if (nodes.length === 0) return nodes;
	const taskById = new Map(tasks.map((task) => [task.id, task]));
	const taskNodeIdByTaskId = new Map<string, string>();
	for (const node of nodes) {
		if (node.kind === 'task' && node.taskId) taskNodeIdByTaskId.set(node.taskId, node.id);
	}
	const projectOrder = new Map(projects.map((project, index) => [project.id, index]));

	// Group nodes into spatial project lanes.
	const groups = new Map<string, CanvasNode[]>();
	for (const node of nodes) {
		const key = node.projectId ?? '__general__';
		const list = groups.get(key) ?? [];
		list.push(node);
		groups.set(key, list);
	}

	const sortedGroupKeys = [...groups.keys()].sort((a, b) => {
		if (a === '__general__') return 1;
		if (b === '__general__') return -1;
		return (projectOrder.get(a) ?? 999) - (projectOrder.get(b) ?? 999);
	});

	const positioned = new Map<string, { x: number; y: number }>();
	let currentLaneY = 80;
	const colGap = 80;
	const rowGap = 36;
	const laneGap = 130;

	for (const groupKey of sortedGroupKeys) {
		const group = groups.get(groupKey) ?? [];
		const groupIds = new Set(group.map((node) => node.id));
		const nodeMap = new Map(group.map((node) => [node.id, node]));

		const depthCache = new Map<string, number>();
		const depthOf = (id: string, visiting = new Set<string>()): number => {
			if (depthCache.has(id)) return depthCache.get(id)!;
			if (visiting.has(id)) return 1;
			visiting.add(id);
			const node = nodeMap.get(id);
			if (!node) return 0;
			let depth = 1;
			if (node.kind === 'project') {
				depth = 0;
			} else if (node.kind === 'task' && node.taskId) {
				const task = taskById.get(node.taskId);
				const parentNodeId = task?.parent_task_id ? taskNodeIdByTaskId.get(task.parent_task_id) : null;
				if (parentNodeId && groupIds.has(parentNodeId)) {
					depth = depthOf(parentNodeId, visiting) + 1;
				} else if (node.parentId && groupIds.has(node.parentId)) {
					depth = depthOf(node.parentId, visiting) + 1;
				} else {
					depth = group.some((candidate) => candidate.kind === 'project') ? 1 : 0;
				}
			} else if (node.parentId && groupIds.has(node.parentId)) {
				depth = depthOf(node.parentId, visiting) + 1;
			} else {
				const hasProjectHub = group.some((candidate) => candidate.kind === 'project');
				if (node.kind === 'editor' || node.kind === 'terminal' || node.kind === 'image' || node.kind === 'app') {
					depth = hasProjectHub ? 2 : 1;
				} else {
					depth = hasProjectHub ? 1 : 0;
				}
			}
			depthCache.set(id, depth);
			return depth;
		};

		const columns = new Map<number, CanvasNode[]>();
		for (const node of group) {
			const d = depthOf(node.id);
			const list = columns.get(d) ?? [];
			list.push(node);
			columns.set(d, list);
		}

		const sortedCols = [...columns.keys()].sort((a, b) => a - b);
		let colX = 80;
		let laneHeight = 0;

		for (const colIndex of sortedCols) {
			const colNodes = columns.get(colIndex) ?? [];
			colNodes.sort((left, right) => {
				const kindOrder: Record<CanvasNodeKind, number> = {
					project: 0,
					conversation: 1,
					task: 2,
					agent: 3,
					editor: 4,
					terminal: 5,
					image: 6,
					app: 7,
					'new-work': 8,
				};
				return kindOrder[left.kind] - kindOrder[right.kind] || left.title.localeCompare(right.title);
			});

			let colY = currentLaneY;
			let maxColWidth = 0;
			for (const node of colNodes) {
				positioned.set(node.id, {
					x: snapCoordinate(colX, true),
					y: snapCoordinate(colY, true),
				});
				maxColWidth = Math.max(maxColWidth, node.width);
				colY += nodeVisualHeight(node) + rowGap;
			}
			laneHeight = Math.max(laneHeight, colY - currentLaneY - rowGap);
			colX += maxColWidth + colGap;
		}

		currentLaneY += Math.max(laneHeight, 320) + laneGap;
	}

	return nodes.map((node) => {
		const next = positioned.get(node.id);
		return next ? { ...node, x: next.x, y: next.y } : node;
	});
}

export function seedCanvasNodes(
	projects: Project[],
	agents: Agent[],
	conversations: Conversation[],
	tasks: Task[],
): CanvasNode[] {
	const nodes: CanvasNode[] = [];
	const agentById = new Map(agents.map((agent) => [agent.id, agent]));
	const conversationById = new Map(conversations.map((conversation) => [conversation.id, conversation]));

	const projectNodeById = new Map<string, CanvasNode>();
	for (const project of projects.slice(0, 6)) {
		const node = createCanvasNode(
			{
				kind: 'project',
				title: project.name,
				projectId: project.id,
				route: `/projects/${encodeURIComponent(project.id)}`,
			},
			nodes,
		);
		nodes.push(node);
		projectNodeById.set(project.id, node);
	}

	const activeTasks = tasks
		.filter((task) => !['completed', 'cancelled'].includes(task.status))
		.slice(0, 10);

	// Order parents before children so delegation chains link cleanly.
	const activeById = new Map(activeTasks.map((task) => [task.id, task]));
	const orderedTasks = [...activeTasks].sort((left, right) => {
		const leftIsChild = left.parent_task_id && activeById.has(left.parent_task_id) ? 1 : 0;
		const rightIsChild = right.parent_task_id && activeById.has(right.parent_task_id) ? 1 : 0;
		return leftIsChild - rightIsChild;
	});

	const taskNodeById = new Map<string, CanvasNode>();
	for (const task of orderedTasks) {
		const projectId = inferTaskProjectId(task, agentById, conversationById);
		const parentTaskNode = task.parent_task_id ? taskNodeById.get(task.parent_task_id) ?? null : null;
		const parentProjectNode = projectId ? projectNodeById.get(projectId) ?? null : null;
		const node = createCanvasNode(
			{
				kind: 'task',
				title: task.title || 'Task',
				taskId: task.id,
				agentId: task.agent_id,
				projectId,
				parentId: parentTaskNode?.id ?? parentProjectNode?.id ?? null,
				edgeLabel: parentTaskNode ? 'delegated' : 'task',
				route: `/tasks/${encodeURIComponent(task.id)}`,
			},
			nodes,
		);
		nodes.push(node);
		taskNodeById.set(task.id, node);
	}

	if (nodes.length === 0) {
		nodes.push(
			createCanvasNode(
				{
					kind: 'new-work',
					title: 'New work',
					route: '/',
				},
				nodes,
			),
		);
	}

	return tidyCanvasLayout(nodes, tasks, projects);
}

export function computeFitCamera(
	nodes: CanvasNode[],
	viewportWidth: number,
	viewportHeight: number,
	padding = 72,
): CanvasCamera {
	if (nodes.length === 0 || viewportWidth <= 0 || viewportHeight <= 0) {
		return { x: 0, y: 0, zoom: 1 };
	}
	let minX = Infinity;
	let minY = Infinity;
	let maxX = -Infinity;
	let maxY = -Infinity;
	for (const node of nodes) {
		minX = Math.min(minX, node.x);
		minY = Math.min(minY, node.y);
		maxX = Math.max(maxX, node.x + node.width);
		maxY = Math.max(maxY, node.y + nodeVisualHeight(node));
	}
	const contentWidth = Math.max(1, maxX - minX);
	const contentHeight = Math.max(1, maxY - minY);
	const availableWidth = Math.max(120, viewportWidth - padding * 2);
	const availableHeight = Math.max(120, viewportHeight - padding * 2);
	const zoom = clampZoom(Math.min(1, availableWidth / contentWidth, availableHeight / contentHeight));
	const x = Math.round((viewportWidth - contentWidth * zoom) / 2 - minX * zoom);
	const y = Math.round((viewportHeight - contentHeight * zoom) / 2 - minY * zoom);
	return { x, y, zoom };
}

export function computeCenterOnNodeCamera(
	node: CanvasNode,
	viewportWidth: number,
	viewportHeight: number,
	currentZoom: number,
): CanvasCamera {
	const zoom = clampZoom(Math.max(currentZoom, 0.85));
	const centerX = node.x + node.width / 2;
	const centerY = node.y + nodeVisualHeight(node) / 2;
	return {
		x: Math.round(viewportWidth / 2 - centerX * zoom),
		y: Math.round(viewportHeight / 2 - centerY * zoom),
		zoom,
	};
}

export function validCanvasNode(value: unknown): value is CanvasNode {
	if (!value || typeof value !== 'object') return false;
	const node = value as Partial<CanvasNode>;
	return typeof node.id === 'string'
		&& node.id.length > 0
		&& typeof node.kind === 'string'
		&& VALID_KINDS.has(node.kind as CanvasNodeKind)
		&& typeof node.title === 'string'
		&& typeof node.x === 'number'
		&& Number.isFinite(node.x)
		&& typeof node.y === 'number'
		&& Number.isFinite(node.y)
		&& typeof node.width === 'number'
		&& Number.isFinite(node.width)
		&& node.width >= 200
		&& typeof node.height === 'number'
		&& Number.isFinite(node.height)
		&& node.height >= 160;
}

export function normalizePersistedCanvasState(raw: unknown): CanvasState | null {
	if (!raw || typeof raw !== 'object') return null;
	const parsed = raw as Partial<CanvasState>;
	if (!Array.isArray(parsed.nodes)) return null;
	const nodes = parsed.nodes
		.filter(validCanvasNode)
		.slice(0, 60)
		.map((node, index) => ({
			id: node.id,
			kind: node.kind,
			title: node.title,
			x: node.x,
			y: node.y,
			width: node.width,
			height: node.height,
			zIndex: typeof node.zIndex === 'number' && Number.isFinite(node.zIndex) ? node.zIndex : index + 1,
			collapsed: Boolean(node.collapsed),
			parentId: typeof node.parentId === 'string' ? node.parentId : null,
			edgeLabel: typeof node.edgeLabel === 'string' ? node.edgeLabel : null,
			projectId: typeof node.projectId === 'string' ? node.projectId : null,
			agentId: typeof node.agentId === 'string' ? node.agentId : null,
			taskId: typeof node.taskId === 'string' ? node.taskId : null,
			conversationId: typeof node.conversationId === 'string' ? node.conversationId : null,
			filePath: typeof node.filePath === 'string' ? node.filePath : null,
			terminalSession: typeof node.terminalSession === 'string' ? node.terminalSession : null,
			imageUrl: typeof node.imageUrl === 'string' ? node.imageUrl : null,
			imageSource: node.imageSource === 'workspace' || node.imageSource === 'url' ? node.imageSource : null,
			appUrl: typeof node.appUrl === 'string' ? node.appUrl : null,
			appSource: node.appSource === 'port' || node.appSource === 'workspace-html' || node.appSource === 'url'
				? node.appSource
				: null,
			appFilePath: typeof node.appFilePath === 'string' ? node.appFilePath : null,
			route: typeof node.route === 'string' ? node.route : null,
		}));
	const camera: CanvasCamera = parsed.camera
		&& typeof parsed.camera.x === 'number'
		&& Number.isFinite(parsed.camera.x)
		&& typeof parsed.camera.y === 'number'
		&& Number.isFinite(parsed.camera.y)
		&& typeof parsed.camera.zoom === 'number'
		&& Number.isFinite(parsed.camera.zoom)
		? { x: parsed.camera.x, y: parsed.camera.y, zoom: clampZoom(parsed.camera.zoom) }
		: { x: 0, y: 0, zoom: 1 };
	return {
		nodes,
		camera,
		focusedNodeId: typeof parsed.focusedNodeId === 'string' && nodes.some((n) => n.id === parsed.focusedNodeId)
			? parsed.focusedNodeId
			: nodes.at(-1)?.id ?? null,
		snapToGrid: parsed.snapToGrid !== false,
		showMinimap: parsed.showMinimap !== false,
		autoPinDelegated: parsed.autoPinDelegated !== false,
	};
}

export function bezierEdgePath(from: CanvasNode, to: CanvasNode): { path: string; midX: number; midY: number } {
	const fromH = nodeVisualHeight(from);
	const toH = nodeVisualHeight(to);
	const fromCenterX = from.x + from.width / 2;
	const toCenterX = to.x + to.width / 2;

	let startX: number;
	let endX: number;
	const startY = from.y + Math.min(fromH / 2, 120);
	const endY = to.y + Math.min(toH / 2, 120);

	if (toCenterX >= fromCenterX) {
		startX = from.x + from.width;
		endX = to.x;
	} else {
		startX = from.x;
		endX = to.x + to.width;
	}

	const dx = Math.max(60, Math.abs(endX - startX) * 0.45);
	const c1x = toCenterX >= fromCenterX ? startX + dx : startX - dx;
	const c2x = toCenterX >= fromCenterX ? endX - dx : endX + dx;
	const path = `M ${startX} ${startY} C ${c1x} ${startY}, ${c2x} ${endY}, ${endX} ${endY}`;
	return {
		path,
		midX: (startX + endX) / 2,
		midY: (startY + endY) / 2,
	};
}
