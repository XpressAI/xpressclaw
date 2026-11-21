<script lang="ts">
	import { onMount } from 'svelte';
	import {
		Bot,
		Code2,
		FolderKanban,
		GitBranch,
		Globe,
		Grid3x3,
		Image as ImageIcon,
		LayoutGrid,
		Map as MapIcon,
		Maximize2,
		MessageSquare,
		Minus,
		Plus,
		RefreshCw,
		Sparkles,
		SquareTerminal,
		Trash2,
	} from '@lucide/svelte';
	import type { Agent, Conversation, Project, Task } from '$lib/api';
	import { notifyLiveUpdatesVisibilityChanged } from '$lib/liveUpdates';
	import {
		bezierEdgePath,
		CANVAS_GRID_SIZE,
		clampZoom,
		computeCenterOnNodeCamera,
		computeFitCamera,
		computeProjectZones,
		createCanvasNode,
		deriveCanvasEdges,
		inferTaskProjectId,
		isImageFilePath,
		nodeVisualHeight,
		normalizePersistedCanvasState,
		seedCanvasNodes,
		snapCoordinate,
		tidyCanvasLayout,
		type CanvasCamera,
		type CanvasNode,
		type CanvasNodeKind,
		type CanvasState,
	} from '$lib/canvas';
	import { createWorkspaceTab, describeWorkspacePath } from '$lib/workspace';
	import WorkspaceContent from '$lib/components/workspace/WorkspaceContent.svelte';
	import CanvasWindow from './CanvasWindow.svelte';
	import CanvasProjectCard from './CanvasProjectCard.svelte';
	import CanvasEditorCard from './CanvasEditorCard.svelte';
	import CanvasTerminalCard from './CanvasTerminalCard.svelte';
	import CanvasImageCard from './CanvasImageCard.svelte';
	import CanvasAppCard from './CanvasAppCard.svelte';

	interface Props {
		storageKey: string;
		storage?: Storage;
		visible?: boolean;
		projects: Project[];
		agents: Agent[];
		conversations: Conversation[];
		tasks: Task[];
		onOpenInTabs: (route: string) => void;
	}

	let {
		storageKey,
		storage,
		visible = true,
		projects,
		agents,
		conversations,
		tasks,
		onOpenInTabs,
	}: Props = $props();

	$effect(() => {
		if (visible) notifyLiveUpdatesVisibilityChanged();
	});

	let viewportEl = $state<HTMLDivElement>();
	let viewportWidth = $state(1280);
	let viewportHeight = $state(800);

	let nodes = $state<CanvasNode[]>([]);
	let camera = $state<CanvasCamera>({ x: 0, y: 0, zoom: 1 });
	let focusedNodeId = $state<string | null>(null);
	let snapToGrid = $state(true);
	let showMinimap = $state(true);
	let autoPinDelegated = $state(true);
	let initialized = $state(false);
	let seededOnce = $state(false);

	let panning = $state(false);
	let spacePressed = $state(false);
	let addMenuOpen = $state(false);
	let addSearch = $state('');
	let activeProjectFilter = $state<string | null>(null);
	let seenDelegatedTaskIds = new Set<string>();
	const dirtyEditorIds = new Set<string>();

	let agentById = $derived(new Map(agents.map((agent) => [agent.id, agent])));
	let projectById = $derived(new Map(projects.map((project) => [project.id, project])));
	let conversationById = $derived(new Map(conversations.map((conversation) => [conversation.id, conversation])));
	let taskById = $derived(new Map(tasks.map((task) => [task.id, task])));
	let nodeById = $derived(new Map(nodes.map((node) => [node.id, node])));

	let edges = $derived(deriveCanvasEdges(nodes, tasks, agents, conversations));
	let projectZones = $derived(computeProjectZones(nodes, projects));

	let delegatedByParentTask = $derived((() => {
		const map = new Map<string, Task[]>();
		for (const task of tasks) {
			if (!task.parent_task_id) continue;
			const list = map.get(task.parent_task_id) ?? [];
			list.push(task);
			map.set(task.parent_task_id, list);
		}
		return map;
	})());

	let gridPixelSize = $derived(Math.max(12, CANVAS_GRID_SIZE * camera.zoom));
	let filteredAddTasks = $derived(
		tasks
			.filter((task) => !addSearch.trim() || task.title.toLowerCase().includes(addSearch.trim().toLowerCase()))
			.slice(0, 8)
	);

	// Minimap world bounds.
	let minimapMetrics = $derived((() => {
		const mapW = 196;
		const mapH = 128;
		let minX = -camera.x / camera.zoom;
		let minY = -camera.y / camera.zoom;
		let maxX = (viewportWidth - camera.x) / camera.zoom;
		let maxY = (viewportHeight - camera.y) / camera.zoom;
		for (const node of nodes) {
			minX = Math.min(minX, node.x - 120);
			minY = Math.min(minY, node.y - 120);
			maxX = Math.max(maxX, node.x + node.width + 120);
			maxY = Math.max(maxY, node.y + nodeVisualHeight(node) + 120);
		}
		const worldW = Math.max(400, maxX - minX);
		const worldH = Math.max(300, maxY - minY);
		const scale = Math.min((mapW - 12) / worldW, (mapH - 12) / worldH);
		return { mapW, mapH, minX, minY, scale };
	})());

	function activeStorage(): Storage | null {
		if (storage) return storage;
		try { return localStorage; } catch { return null; }
	}

	function persist() {
		if (!initialized) return;
		const target = activeStorage();
		if (!target) return;
		const state: CanvasState = {
			nodes,
			camera,
			focusedNodeId,
			snapToGrid,
			showMinimap,
			autoPinDelegated,
		};
		try {
			target.setItem(storageKey, JSON.stringify(state));
		} catch {}
	}

	onMount(() => {
		try {
			const raw = activeStorage()?.getItem(storageKey);
			if (raw) {
				const restored = normalizePersistedCanvasState(JSON.parse(raw));
				if (restored) {
					nodes = restored.nodes;
					camera = restored.camera;
					focusedNodeId = restored.focusedNodeId;
					snapToGrid = restored.snapToGrid;
					showMinimap = restored.showMinimap;
					autoPinDelegated = restored.autoPinDelegated;
					seededOnce = restored.nodes.length > 0;
				}
			}
		} catch {}
		initialized = true;

		const observer = new ResizeObserver(() => {
			if (!viewportEl) return;
			const rect = viewportEl.getBoundingClientRect();
			if (rect.width > 0 && rect.height > 0) {
				viewportWidth = rect.width;
				viewportHeight = rect.height;
			}
		});
		if (viewportEl) {
			observer.observe(viewportEl);
			const rect = viewportEl.getBoundingClientRect();
			if (rect.width > 0 && rect.height > 0) {
				viewportWidth = rect.width;
				viewportHeight = rect.height;
			}
		}

		const handleKeyDown = (event: KeyboardEvent) => {
			if (!visible) return;
			const target = event.target as HTMLElement | null;
			const typing = Boolean(target?.closest('input, textarea, select, [contenteditable="true"], .monaco-editor, .xterm'));
			if (event.code === 'Space' && !typing) {
				event.preventDefault();
				spacePressed = true;
			}
			if (event.key === 'Escape' && addMenuOpen) {
				addMenuOpen = false;
			}
		};
		const handleKeyUp = (event: KeyboardEvent) => {
			if (event.code === 'Space') spacePressed = false;
		};
		window.addEventListener('keydown', handleKeyDown);
		window.addEventListener('keyup', handleKeyUp);

		return () => {
			observer.disconnect();
			window.removeEventListener('keydown', handleKeyDown);
			window.removeEventListener('keyup', handleKeyUp);
		};
	});

	// Seed the canvas once when data arrives and no saved layout exists yet.
	$effect(() => {
		if (!initialized || seededOnce || nodes.length > 0) return;
		if (projects.length === 0 && tasks.length === 0 && agents.length === 0) return;
		seededOnce = true;
		nodes = seedCanvasNodes(projects, agents, conversations, tasks);
		focusedNodeId = nodes[0]?.id ?? null;
		camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		for (const task of tasks) {
			if (task.parent_task_id) seenDelegatedTaskIds.add(task.id);
		}
		persist();
	});

	// Keep node titles synchronized with live project/task/conversation/agent metadata.
	$effect(() => {
		if (!initialized || nodes.length === 0) return;
		let changed = false;
		const updated = nodes.map((node) => {
			let nextTitle = node.title;
			let nextProjectId = node.projectId;
			let nextAgentId = node.agentId;
			if (node.kind === 'project' && node.projectId) {
				const project = projectById.get(node.projectId);
				if (project && project.name !== node.title) nextTitle = project.name;
			} else if (node.kind === 'task' && node.taskId) {
				const task = taskById.get(node.taskId);
				if (task) {
					if (task.title && task.title !== node.title) nextTitle = task.title;
					const inferredProject = inferTaskProjectId(task, agentById, conversationById);
					if (inferredProject && inferredProject !== node.projectId) nextProjectId = inferredProject;
					if (task.agent_id && task.agent_id !== node.agentId) nextAgentId = task.agent_id;
				}
			} else if (node.kind === 'conversation' && node.conversationId) {
				const conversation = conversationById.get(node.conversationId);
				if (conversation) {
					const title = conversation.title || 'Untitled conversation';
					if (title !== node.title) nextTitle = title;
					if (conversation.project_id && conversation.project_id !== node.projectId) nextProjectId = conversation.project_id;
				}
			} else if (node.kind === 'agent' && node.agentId) {
				const agent = agentById.get(node.agentId);
				if (agent) {
					const title = agent.title || agent.name;
					if (title !== node.title) nextTitle = title;
					if (agent.project_id && agent.project_id !== node.projectId) nextProjectId = agent.project_id;
				}
			}
			if (nextTitle !== node.title || nextProjectId !== node.projectId || nextAgentId !== node.agentId) {
				changed = true;
				return { ...node, title: nextTitle, projectId: nextProjectId, agentId: nextAgentId };
			}
			return node;
		});
		if (changed) {
			nodes = updated;
			persist();
		}
	});

	// Auto-pin newly delegated child tasks when their parent task is open on the canvas.
	$effect(() => {
		if (!initialized || !autoPinDelegated || nodes.length === 0) return;
		const openTaskNodeByTaskId = new Map<string, CanvasNode>();
		for (const node of nodes) {
			if (node.kind === 'task' && node.taskId) openTaskNodeByTaskId.set(node.taskId, node);
		}
		const newChildren: Task[] = [];
		for (const task of tasks) {
			if (!task.parent_task_id || ['completed', 'cancelled'].includes(task.status)) continue;
			if (openTaskNodeByTaskId.has(task.id) || seenDelegatedTaskIds.has(task.id)) continue;
			if (openTaskNodeByTaskId.has(task.parent_task_id)) {
				newChildren.push(task);
			}
		}
		if (newChildren.length === 0) return;
		let nextNodes = [...nodes];
		for (const child of newChildren) {
			seenDelegatedTaskIds.add(child.id);
			const parentNode = nextNodes.find((n) => n.kind === 'task' && n.taskId === child.parent_task_id) ?? null;
			const created = createCanvasNode(
				{
					kind: 'task',
					title: child.title || 'Delegated task',
					taskId: child.id,
					agentId: child.agent_id,
					projectId: inferTaskProjectId(child, agentById, conversationById) ?? parentNode?.projectId ?? null,
					parentId: parentNode?.id ?? null,
					edgeLabel: 'delegated',
					route: `/tasks/${encodeURIComponent(child.id)}`,
				},
				nextNodes,
			);
			nextNodes = [...nextNodes, created];
		}
		nodes = nextNodes;
		persist();
	});

	function viewportCenterWorld(): { x: number; y: number } {
		return {
			x: Math.round((viewportWidth / 2 - camera.x) / camera.zoom - 240),
			y: Math.round((viewportHeight / 2 - camera.y) / camera.zoom - 200),
		};
	}

	function bringToFront(id: string) {
		const target = nodes.find((node) => node.id === id);
		if (!target) return;
		const maxZ = nodes.reduce((highest, node) => Math.max(highest, node.zIndex), 0);
		focusedNodeId = id;
		if (target.zIndex < maxZ) {
			nodes = nodes.map((node) => node.id === id ? { ...node, zIndex: maxZ + 1 } : node);
			persist();
		}
	}

	function centerOnNode(node: CanvasNode) {
		camera = computeCenterOnNodeCamera(node, viewportWidth, viewportHeight, camera.zoom);
		bringToFront(node.id);
		persist();
	}

	function fitAll() {
		camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		persist();
	}

	function tidyLayout() {
		nodes = tidyCanvasLayout(nodes, tasks, projects);
		camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		persist();
	}

	function zoomBy(factor: number, clientX?: number, clientY?: number) {
		const nextZoom = clampZoom(camera.zoom * factor);
		if (nextZoom === camera.zoom) return;
		const rect = viewportEl?.getBoundingClientRect();
		const px = clientX !== undefined && rect ? clientX - rect.left : viewportWidth / 2;
		const py = clientY !== undefined && rect ? clientY - rect.top : viewportHeight / 2;
		const worldX = (px - camera.x) / camera.zoom;
		const worldY = (py - camera.y) / camera.zoom;
		camera = {
			zoom: nextZoom,
			x: Math.round(px - worldX * nextZoom),
			y: Math.round(py - worldY * nextZoom),
		};
		persist();
	}

	function resetZoom() {
		zoomBy(1 / camera.zoom);
	}

	function startPan(event: PointerEvent) {
		event.preventDefault();
		event.stopPropagation();
		addMenuOpen = false;
		panning = true;
		const startX = event.clientX;
		const startY = event.clientY;
		const originX = camera.x;
		const originY = camera.y;

		const move = (moveEvent: PointerEvent) => {
			camera = {
				...camera,
				x: Math.round(originX + (moveEvent.clientX - startX)),
				y: Math.round(originY + (moveEvent.clientY - startY)),
			};
		};
		const up = () => {
			panning = false;
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			persist();
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
	}

	function handleCapturePointerDown(event: PointerEvent) {
		const target = event.target as HTMLElement | null;
		if (target?.closest('[data-canvas-hud]')) return;
		if (event.button === 1 || (event.button === 0 && spacePressed)) {
			startPan(event);
		}
	}

	function handleBackgroundPointerDown(event: PointerEvent) {
		addMenuOpen = false;
		const isMiddle = event.button === 1;
		const isLeftOnBackground = event.button === 0 && (spacePressed || event.target === event.currentTarget || (event.target as HTMLElement)?.hasAttribute('data-canvas-pan-surface'));
		if (!isMiddle && !isLeftOnBackground) return;
		startPan(event);
	}

	function handleWheel(event: WheelEvent) {
		// Zoom on Ctrl/Cmd+wheel (and trackpad pinch) anywhere over the canvas.
		if (event.ctrlKey || event.metaKey) {
			event.preventDefault();
			const factor = event.deltaY < 0 ? 1.1 : 1 / 1.1;
			zoomBy(factor, event.clientX, event.clientY);
			return;
		}
		// Only pan with wheel when hovering the empty canvas background so scrolling
		// inside task chats, Monaco editors, and terminals works naturally!
		const target = event.target as HTMLElement | null;
		if (target?.closest('[data-canvas-node], [data-canvas-hud]')) return;
		event.preventDefault();
		camera = {
			...camera,
			x: Math.round(camera.x - event.deltaX),
			y: Math.round(camera.y - event.deltaY),
		};
		persist();
	}

	function updateNode(id: string, patch: Partial<CanvasNode>, shouldPersist = true) {
		nodes = nodes.map((node) => node.id === id ? { ...node, ...patch } : node);
		if (shouldPersist) persist();
	}

	function setEditorDirty(id: string, dirty: boolean) {
		if (dirty) dirtyEditorIds.add(id);
		else dirtyEditorIds.delete(id);
	}

	function closeNode(id: string) {
		if (dirtyEditorIds.has(id)) {
			const target = nodeById.get(id);
			const label = target?.filePath || target?.title || 'this editor';
			if (!window.confirm(`Discard unsaved changes in ${label}?`)) return;
		}
		dirtyEditorIds.delete(id);
		nodes = nodes.filter((node) => node.id !== id).map((node) => node.parentId === id ? { ...node, parentId: null } : node);
		if (focusedNodeId === id) focusedNodeId = nodes.at(-1)?.id ?? null;
		persist();
	}

	function clearCanvas() {
		const dirtyNodes = nodes.filter((node) => node.kind === 'editor' && dirtyEditorIds.has(node.id));
		if (dirtyNodes.length > 0) {
			const message = dirtyNodes.length === 1
				? `Discard unsaved changes in ${dirtyNodes[0].filePath || dirtyNodes[0].title || '1 editor'} and clear the canvas?`
				: `Discard unsaved changes in ${dirtyNodes.length} editors and clear the canvas?`;
			if (!window.confirm(message)) return;
		}
		dirtyEditorIds.clear();
		nodes = [];
		focusedNodeId = null;
		persist();
	}

	function addOrFocusNode(
		matcher: (node: CanvasNode) => boolean,
		partial: Partial<CanvasNode> & Pick<CanvasNode, 'kind' | 'title'>,
		center = true,
	): CanvasNode {
		const existing = nodes.find(matcher);
		if (existing) {
			if (existing.collapsed) updateNode(existing.id, { collapsed: false });
			if (center) centerOnNode(existing);
			else bringToFront(existing.id);
			return existing;
		}
		const created = createCanvasNode(partial, nodes, viewportCenterWorld());
		nodes = [...nodes, created];
		focusedNodeId = created.id;
		if (center) {
			camera = computeCenterOnNodeCamera(created, viewportWidth, viewportHeight, camera.zoom);
		}
		persist();
		return created;
	}

	function spawnProjectNode(project: Project) {
		addMenuOpen = false;
		activeProjectFilter = project.id;
		addOrFocusNode(
			(node) => node.kind === 'project' && node.projectId === project.id,
			{
				kind: 'project',
				title: project.name,
				projectId: project.id,
				route: `/projects/${encodeURIComponent(project.id)}`,
			},
		);
	}

	function spawnTaskNode(task: Task, parentNodeId: string | null = null, center = true) {
		addMenuOpen = false;
		seenDelegatedTaskIds.add(task.id);
		const projectId = inferTaskProjectId(task, agentById, conversationById);
		const parentTaskNode = task.parent_task_id
			? nodes.find((node) => node.kind === 'task' && node.taskId === task.parent_task_id)
			: null;
		const resolvedParentId = parentTaskNode?.id ?? parentNodeId ?? (projectId ? nodes.find((node) => node.kind === 'project' && node.projectId === projectId)?.id ?? null : null);
		addOrFocusNode(
			(node) => node.kind === 'task' && node.taskId === task.id,
			{
				kind: 'task',
				title: task.title || 'Task',
				taskId: task.id,
				agentId: task.agent_id,
				projectId,
				parentId: resolvedParentId,
				edgeLabel: parentTaskNode ? 'delegated' : 'task',
				route: `/tasks/${encodeURIComponent(task.id)}`,
			},
			center,
		);
	}

	function spawnConversationNode(conversation: Conversation, parentNodeId: string | null = null) {
		addMenuOpen = false;
		const projectId = conversation.project_id;
		const resolvedParentId = parentNodeId ?? (projectId ? nodes.find((node) => node.kind === 'project' && node.projectId === projectId)?.id ?? null : null);
		const participantAgentId = conversation.participants.find((participant) => participant.participant_type === 'agent')?.participant_id ?? null;
		addOrFocusNode(
			(node) => node.kind === 'conversation' && node.conversationId === conversation.id,
			{
				kind: 'conversation',
				title: conversation.title || 'Untitled conversation',
				conversationId: conversation.id,
				projectId,
				agentId: participantAgentId,
				parentId: resolvedParentId,
				edgeLabel: 'chat',
				route: `/conversations/${encodeURIComponent(conversation.id)}`,
			},
		);
	}

	function spawnAgentNode(agent: Agent, parentNodeId: string | null = null, route?: string) {
		addMenuOpen = false;
		const resolvedParentId = parentNodeId ?? (agent.project_id ? nodes.find((node) => node.kind === 'project' && node.projectId === agent.project_id)?.id ?? null : null);
		const targetRoute = route || `/agents/${encodeURIComponent(agent.id)}`;
		const existing = nodes.find((node) => node.kind === 'agent' && node.agentId === agent.id);
		if (existing) {
			const patch: Partial<CanvasNode> = {};
			if (existing.collapsed) patch.collapsed = false;
			if (route && existing.route !== targetRoute) patch.route = targetRoute;
			if (Object.keys(patch).length > 0) updateNode(existing.id, patch);
			if (focusedNodeId === existing.id && route) {
				bringToFront(existing.id);
			} else {
				centerOnNode(existing);
			}
			return;
		}
		addOrFocusNode(
			(node) => node.kind === 'agent' && node.agentId === agent.id,
			{
				kind: 'agent',
				title: agent.title || agent.name,
				agentId: agent.id,
				projectId: agent.project_id,
				parentId: resolvedParentId,
				edgeLabel: 'agent',
				route: targetRoute,
			},
		);
	}

	function spawnToolNode(
		kind: 'editor' | 'terminal' | 'image' | 'app',
		agentId: string | null,
		parentNodeId: string | null = null,
		options?: {
			filePath?: string | null;
			terminalSession?: string | null;
			imageUrl?: string | null;
			imageSource?: 'workspace' | 'url' | null;
			appUrl?: string | null;
			appSource?: 'port' | 'workspace-html' | 'url' | null;
			appFilePath?: string | null;
			title?: string;
		},
	) {
		addMenuOpen = false;
		const parent = parentNodeId ? nodeById.get(parentNodeId) ?? null : null;
		const resolvedAgentId = agentId ?? parent?.agentId ?? agents[0]?.id ?? null;
		const agent = resolvedAgentId ? agentById.get(resolvedAgentId) ?? null : null;
		const resolvedProjectId = parent?.projectId ?? agent?.project_id ?? activeProjectFilter ?? projects[0]?.id ?? null;

		const defaultTitle = options?.title ?? (
			kind === 'editor'
				? (options?.filePath ? options.filePath.split('/').pop() || 'Editor' : `Editor${agent ? ` · ${agent.title || agent.name}` : ''}`)
				: kind === 'terminal'
					? `Terminal${agent ? ` · ${agent.title || agent.name}` : ''}`
					: kind === 'image'
						? (options?.filePath ? options.filePath.split('/').pop() || 'Image' : `Image${agent ? ` · ${agent.title || agent.name}` : ''}`)
						: (options?.appFilePath ? `App · ${options.appFilePath.split('/').pop()}` : `App${agent ? ` · ${agent.title || agent.name}` : ''}`)
		);

		// Deduplicate editor/image/html-app for the exact same agent+filePath if already open.
		if (kind === 'editor' && resolvedAgentId && options?.filePath) {
			const existing = nodes.find((node) => node.kind === 'editor' && node.agentId === resolvedAgentId && node.filePath === options.filePath);
			if (existing) {
				centerOnNode(existing);
				return;
			}
		}
		if (kind === 'image' && resolvedAgentId && options?.filePath) {
			const existing = nodes.find((node) => node.kind === 'image' && node.agentId === resolvedAgentId && node.filePath === options.filePath);
			if (existing) {
				centerOnNode(existing);
				return;
			}
		}

		const created = createCanvasNode(
			{
				kind,
				title: defaultTitle,
				parentId: parentNodeId,
				edgeLabel: kind,
				projectId: resolvedProjectId,
				agentId: resolvedAgentId,
				filePath: options?.filePath ?? null,
				terminalSession: options?.terminalSession ?? null,
				imageUrl: options?.imageUrl ?? null,
				imageSource: options?.imageSource ?? (options?.filePath ? 'workspace' : null),
				appUrl: options?.appUrl ?? null,
				appSource: options?.appSource ?? null,
				appFilePath: options?.appFilePath ?? null,
			},
			nodes,
			viewportCenterWorld(),
		);
		nodes = [...nodes, created];
		centerOnNode(created);
	}

	function expandDelegatedForTask(taskId: string, parentNodeId: string) {
		const children = delegatedByParentTask.get(taskId) ?? [];
		if (children.length === 0) return;
		for (const child of children) {
			spawnTaskNode(child, parentNodeId, false);
		}
		persist();
	}

	function expandActiveTasksForProject(projectId: string, parentNodeId: string) {
		const projectAgentIds = new Set(agents.filter((agent) => agent.project_id === projectId).map((agent) => agent.id));
		const matching = tasks.filter((task) => {
			if (['completed', 'cancelled'].includes(task.status)) return false;
			const resolved = inferTaskProjectId(task, agentById, conversationById);
			return resolved === projectId || Boolean(task.agent_id && projectAgentIds.has(task.agent_id));
		});
		const matchingIds = new Set(matching.map((task) => task.id));
		const ordered = [...matching].sort((a, b) => (a.parent_task_id && matchingIds.has(a.parent_task_id) ? 1 : 0) - (b.parent_task_id && matchingIds.has(b.parent_task_id) ? 1 : 0));
		for (const task of ordered.slice(0, 10)) {
			spawnTaskNode(task, parentNodeId, false);
		}
		persist();
	}

	function syncActiveWork() {
		const seeded = seedCanvasNodes(projects, agents, conversations, tasks);
		const existingKeys = new Set(nodes.map((node) => `${node.kind}:${node.projectId ?? ''}:${node.taskId ?? ''}:${node.conversationId ?? ''}`));
		let nextNodes = [...nodes];
		for (const candidate of seeded) {
			const key = `${candidate.kind}:${candidate.projectId ?? ''}:${candidate.taskId ?? ''}:${candidate.conversationId ?? ''}`;
			if (!existingKeys.has(key)) {
				nextNodes.push(createCanvasNode(candidate, nextNodes, viewportCenterWorld()));
				existingKeys.add(key);
			}
		}
		nodes = tidyCanvasLayout(nextNodes, tasks, projects);
		camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		persist();
	}

	function jumpToProject(projectId: string | null) {
		activeProjectFilter = projectId;
		if (!projectId) {
			fitAll();
			return;
		}
		const projectNodes = nodes.filter((node) => node.projectId === projectId);
		if (projectNodes.length > 0) {
			camera = computeFitCamera(projectNodes, viewportWidth, viewportHeight);
			persist();
			return;
		}
		const project = projectById.get(projectId);
		if (project) spawnProjectNode(project);
	}

	/** Called by WorkspaceShell when links or sidebar items are activated in Canvas mode. */
	export function openRoute(route: string, preferredParentId: string | null = null): boolean {
		const desc = describeWorkspacePath(route);
		const parentId = preferredParentId ?? focusedNodeId;

		if (desc.kind === 'project' && desc.resourceId) {
			const project = projectById.get(desc.resourceId);
			if (project) {
				spawnProjectNode(project);
				return true;
			}
		}
		if (desc.kind === 'task' && desc.resourceId) {
			const task = taskById.get(desc.resourceId);
			if (task) {
				spawnTaskNode(task, parentId);
				return true;
			}
			addOrFocusNode(
				(node) => node.kind === 'task' && node.taskId === desc.resourceId,
				{
					kind: 'task',
					title: desc.title,
					taskId: desc.resourceId,
					parentId,
					edgeLabel: 'task',
					route,
				},
			);
			return true;
		}
		if (desc.kind === 'conversation' && desc.resourceId) {
			const conversation = conversationById.get(desc.resourceId);
			if (conversation) {
				spawnConversationNode(conversation, parentId);
				return true;
			}
			addOrFocusNode(
				(node) => node.kind === 'conversation' && node.conversationId === desc.resourceId,
				{
					kind: 'conversation',
					title: desc.title,
					conversationId: desc.resourceId,
					parentId,
					edgeLabel: 'chat',
					route,
				},
			);
			return true;
		}
		if (desc.kind === 'agent' && desc.resourceId) {
			const url = new URL(route, 'http://xpressclaw.local');
			const tabParam = url.searchParams.get('tab');
			const filePath = url.searchParams.get('path');
			const terminal = url.searchParams.get('terminal');
			if (tabParam === 'files' && terminal) {
				spawnToolNode('terminal', desc.resourceId, parentId, { terminalSession: terminal });
				return true;
			}
			if (tabParam === 'files' && filePath) {
				if (isImageFilePath(filePath)) {
					spawnToolNode('image', desc.resourceId, parentId, { filePath, imageSource: 'workspace' });
				} else {
					spawnToolNode('editor', desc.resourceId, parentId, { filePath });
				}
				return true;
			}
			const agent = agentById.get(desc.resourceId);
			if (agent) {
				spawnAgentNode(agent, parentId, route);
				return true;
			}
		}
		if (desc.kind === 'home') {
			addOrFocusNode(
				(node) => node.kind === 'new-work',
				{
					kind: 'new-work',
					title: 'New work',
					route: '/',
				},
			);
			return true;
		}
		return false;
	}

	function nodeStatus(node: CanvasNode): string | null {
		if (node.kind === 'task' && node.taskId) {
			const task = taskById.get(node.taskId);
			return task ? (task.activity_status ?? task.status) : null;
		}
		if (node.kind === 'agent' && node.agentId) {
			return agentById.get(node.agentId)?.status ?? null;
		}
		if (node.kind === 'project' && node.projectId) {
			const projectTasks = tasks.filter((task) => inferTaskProjectId(task, agentById, conversationById) === node.projectId && !['completed', 'cancelled'].includes(task.status));
			if (projectTasks.some((task) => ['failed', 'error', 'blocked'].includes(task.activity_status ?? task.status))) return 'blocked';
			if (projectTasks.some((task) => (task.activity_status ?? task.status) === 'waiting_for_input')) return 'waiting_for_input';
			if (projectTasks.some((task) => ['running', 'in_progress', 'preparing'].includes(task.activity_status ?? task.status))) return 'running';
			return 'ready';
		}
		return null;
	}

	function nodeContextLabel(node: CanvasNode): string | null {
		const parts: string[] = [];
		if (node.kind !== 'project' && node.projectId) {
			const project = projectById.get(node.projectId);
			if (project) parts.push(project.name);
		}
		if (node.kind !== 'agent' && node.agentId) {
			const agent = agentById.get(node.agentId);
			if (agent) parts.push(agent.title || agent.name);
		}
		if (node.kind === 'editor' && node.filePath) {
			parts.push(node.filePath);
		} else if (node.kind === 'app' && (node.appFilePath || node.appUrl)) {
			parts.push(node.appFilePath || node.appUrl || '');
		}
		return parts.length > 0 ? parts.join(' · ') : null;
	}

	function edgeStroke(kind: 'context' | 'delegation' | 'dependency', status: string | null): string {
		if (kind === 'delegation') {
			if (status && ['failed', 'error', 'blocked'].includes(status)) return '#ef4444';
			if (status === 'waiting_for_input') return '#f97316';
			if (status && ['running', 'in_progress', 'preparing'].includes(status)) return '#3b82f6';
			if (status === 'completed') return '#10b981';
			return '#6366f1';
		}
		if (kind === 'dependency') return '#f59e0b';
		return 'hsl(var(--border-strong))';
	}

	function minimapNavigate(event: PointerEvent) {
		const el = event.currentTarget as HTMLElement;
		const panTo = (clientX: number, clientY: number) => {
			const rect = el.getBoundingClientRect();
			const mx = clientX - rect.left - 6;
			const my = clientY - rect.top - 6;
			const worldX = minimapMetrics.minX + mx / minimapMetrics.scale;
			const worldY = minimapMetrics.minY + my / minimapMetrics.scale;
			camera = {
				...camera,
				x: Math.round(viewportWidth / 2 - worldX * camera.zoom),
				y: Math.round(viewportHeight / 2 - worldY * camera.zoom),
			};
		};
		panTo(event.clientX, event.clientY);
		const move = (moveEvent: PointerEvent) => panTo(moveEvent.clientX, moveEvent.clientY);
		const up = () => {
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			persist();
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
	}

	function minimapNodeColor(kind: CanvasNodeKind): string {
		switch (kind) {
			case 'project': return 'bg-indigo-500/80';
			case 'task': return 'bg-blue-500/80';
			case 'conversation': return 'bg-teal-500/80';
			case 'agent': return 'bg-cyan-500/80';
			case 'editor': return 'bg-sky-500/80';
			case 'terminal': return 'bg-emerald-500/80';
			case 'image': return 'bg-violet-500/80';
			case 'app': return 'bg-amber-500/80';
			default: return 'bg-primary/80';
		}
	}
</script>

<div
	bind:this={viewportEl}
	data-workspace-canvas
	data-canvas-zoom={camera.zoom}
	role="region"
	aria-label="Spatial workspace canvas"
	onpointerdowncapture={handleCapturePointerDown}
	onpointerdown={handleBackgroundPointerDown}
	onwheel={handleWheel}
	class="relative flex h-full w-full flex-1 select-none overflow-hidden bg-[hsl(var(--canvas))] {panning || spacePressed ? 'cursor-grabbing' : 'cursor-grab'}"
	style="background-image: radial-gradient(circle, hsl(var(--border-strong) / 0.85) 1px, transparent 1px); background-size: {gridPixelSize}px {gridPixelSize}px; background-position: {camera.x}px {camera.y}px;"
>
	<!-- Floating Top HUD -->
	<div
		role="toolbar"
		aria-label="Canvas controls"
		tabindex="-1"
		data-canvas-hud
		onpointerdown={(event) => event.stopPropagation()}
		class="pointer-events-auto absolute left-3 right-3 top-3 z-40 flex flex-wrap items-center gap-2"
	>
		<!-- Project spatial pills -->
		<div class="flex max-w-full items-center gap-1 overflow-x-auto rounded-xl border border-border bg-card/95 p-1 shadow-[var(--shadow-overlay)] backdrop-blur scrollbar-hide">
			<button
				type="button"
				onclick={() => jumpToProject(null)}
				class="shrink-0 rounded-lg px-2.5 py-1 text-xs font-medium transition {activeProjectFilter === null ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
			>
				All Projects
			</button>
			{#each projects as project (project.id)}
				<button
					type="button"
					onclick={() => jumpToProject(project.id)}
					class="inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium transition {activeProjectFilter === project.id ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'}"
				>
					<span>{project.icon || project.name.slice(0, 1).toUpperCase()}</span>
					<span class="max-w-28 truncate">{project.name}</span>
				</button>
			{/each}
		</div>

		<div class="flex-1"></div>

		<!-- Action toolbar -->
		<div class="flex flex-wrap items-center gap-1 rounded-xl border border-border bg-card/95 p-1 shadow-[var(--shadow-overlay)] backdrop-blur">
			<div class="relative">
				<button
					type="button"
					onclick={() => (addMenuOpen = !addMenuOpen)}
					data-canvas-add-button
					class="inline-flex items-center gap-1 rounded-lg bg-primary px-2.5 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90"
				>
					<Plus class="h-3.5 w-3.5" />
					<span>Add</span>
				</button>
				{#if addMenuOpen}
					<div
						data-canvas-add-menu
						class="absolute right-0 top-10 z-50 w-72 rounded-xl border border-border bg-card p-2 shadow-2xl"
					>
						<div class="mb-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Contextual Tools</div>
						<div class="grid grid-cols-2 gap-1">
							<button type="button" onclick={() => spawnToolNode('editor', null, focusedNodeId)} class="flex items-center gap-1.5 rounded-lg border border-border/80 px-2 py-1.5 text-left text-xs hover:bg-accent">
								<Code2 class="h-3.5 w-3.5 text-sky-500" /><span>Code Editor</span>
							</button>
							<button type="button" onclick={() => spawnToolNode('terminal', null, focusedNodeId)} class="flex items-center gap-1.5 rounded-lg border border-border/80 px-2 py-1.5 text-left text-xs hover:bg-accent">
								<SquareTerminal class="h-3.5 w-3.5 text-emerald-500" /><span>Terminal</span>
							</button>
							<button type="button" onclick={() => spawnToolNode('image', null, focusedNodeId)} class="flex items-center gap-1.5 rounded-lg border border-border/80 px-2 py-1.5 text-left text-xs hover:bg-accent">
								<ImageIcon class="h-3.5 w-3.5 text-violet-500" /><span>Image Viewer</span>
							</button>
							<button type="button" onclick={() => spawnToolNode('app', null, focusedNodeId)} class="flex items-center gap-1.5 rounded-lg border border-border/80 px-2 py-1.5 text-left text-xs hover:bg-accent">
								<Globe class="h-3.5 w-3.5 text-amber-500" /><span>Web App</span>
							</button>
						</div>

						<button
							type="button"
							onclick={() => { addMenuOpen = false; openRoute('/'); }}
							class="mt-1.5 flex w-full items-center gap-1.5 rounded-lg border border-border/80 px-2 py-1.5 text-left text-xs hover:bg-accent"
						>
							<Sparkles class="h-3.5 w-3.5 text-primary" /><span>New Work Composer</span>
						</button>

						{#if projects.length > 0}
							<div class="mb-1 mt-3 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Projects</div>
							<div class="max-h-28 space-y-0.5 overflow-y-auto">
								{#each projects as project (project.id)}
									<button type="button" onclick={() => spawnProjectNode(project)} class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-accent">
										<FolderKanban class="h-3.5 w-3.5 shrink-0 text-indigo-500" />
										<span class="truncate">{project.name}</span>
									</button>
								{/each}
							</div>
						{/if}

						{#if conversations.length > 0}
							<div class="mb-1 mt-3 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Conversations</div>
							<div class="max-h-28 space-y-0.5 overflow-y-auto">
								{#each conversations.slice(0, 6) as conversation (conversation.id)}
									<button type="button" onclick={() => spawnConversationNode(conversation)} class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-accent">
										<MessageSquare class="h-3.5 w-3.5 shrink-0 text-teal-500" />
										<span class="truncate">{conversation.title || 'Untitled conversation'}</span>
									</button>
								{/each}
							</div>
						{/if}

						{#if tasks.length > 0}
							<div class="mb-1 mt-3 flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
								<span>Tasks</span>
							</div>
							<input
								bind:value={addSearch}
								placeholder="Filter tasks…"
								class="mb-1 w-full rounded-md border border-input bg-background px-2 py-1 text-xs"
							/>
							<div class="max-h-36 space-y-0.5 overflow-y-auto">
								{#each filteredAddTasks as task (task.id)}
									<button type="button" onclick={() => spawnTaskNode(task)} class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-accent">
										<GitBranch class="h-3.5 w-3.5 shrink-0 text-blue-500" />
										<span class="min-w-0 flex-1 truncate">{task.title}</span>
									</button>
								{/each}
							</div>
						{/if}

						{#if agents.length > 0}
							<div class="mb-1 mt-3 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Agents</div>
							<div class="max-h-24 space-y-0.5 overflow-y-auto">
								{#each agents as agent (agent.id)}
									<button type="button" onclick={() => spawnAgentNode(agent)} class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-accent">
										<Bot class="h-3.5 w-3.5 shrink-0 text-cyan-500" />
										<span class="truncate">{agent.title || agent.name}</span>
									</button>
								{/each}
							</div>
						{/if}
					</div>
				{/if}
			</div>

			<button type="button" onclick={syncActiveWork} class="inline-flex items-center gap-1 rounded-lg px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground" title="Pull active & delegated tasks onto the canvas">
				<RefreshCw class="h-3.5 w-3.5" /><span class="hidden sm:inline">Sync active</span>
			</button>

			<button type="button" onclick={tidyLayout} data-canvas-tidy class="inline-flex items-center gap-1 rounded-lg px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground" title="Auto-arrange windows by project and delegation hierarchy">
				<LayoutGrid class="h-3.5 w-3.5" /><span class="hidden sm:inline">Tidy</span>
			</button>

			<button type="button" onclick={fitAll} data-canvas-fit class="inline-flex items-center gap-1 rounded-lg px-2 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground" title="Fit all windows in view">
				<Maximize2 class="h-3.5 w-3.5" /><span class="hidden sm:inline">Fit</span>
			</button>

			<span class="mx-0.5 h-4 w-px bg-border" aria-hidden="true"></span>

			<button type="button" onclick={() => zoomBy(1 / 1.15)} class="rounded-lg p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground" title="Zoom out" aria-label="Zoom out canvas">
				<Minus class="h-3.5 w-3.5" />
			</button>
			<button type="button" onclick={resetZoom} class="min-w-11 rounded-lg px-1.5 py-1 font-mono text-[11px] text-foreground hover:bg-accent" title="Reset zoom to 100%">
				{Math.round(camera.zoom * 100)}%
			</button>
			<button type="button" onclick={() => zoomBy(1.15)} class="rounded-lg p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground" title="Zoom in" aria-label="Zoom in canvas">
				<Plus class="h-3.5 w-3.5" />
			</button>

			<span class="mx-0.5 h-4 w-px bg-border" aria-hidden="true"></span>

			<button
				type="button"
				onclick={() => { snapToGrid = !snapToGrid; persist(); }}
				class="rounded-lg p-1.5 {snapToGrid ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent'}"
				title={snapToGrid ? 'Snap to grid: On' : 'Snap to grid: Off'}
				aria-label="Toggle grid snap"
				aria-pressed={snapToGrid}
			>
				<Grid3x3 class="h-3.5 w-3.5" />
			</button>

			<button
				type="button"
				onclick={() => { autoPinDelegated = !autoPinDelegated; persist(); }}
				class="rounded-lg p-1.5 {autoPinDelegated ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent'}"
				title={autoPinDelegated ? 'Auto-pin delegated tasks: On' : 'Auto-pin delegated tasks: Off'}
				aria-label="Toggle auto-pin delegated tasks"
				aria-pressed={autoPinDelegated}
			>
				<GitBranch class="h-3.5 w-3.5" />
			</button>

			<button
				type="button"
				onclick={() => { showMinimap = !showMinimap; persist(); }}
				class="rounded-lg p-1.5 {showMinimap ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent'}"
				title={showMinimap ? 'Hide minimap' : 'Show minimap'}
				aria-label="Toggle minimap"
				aria-pressed={showMinimap}
			>
				<MapIcon class="h-3.5 w-3.5" />
			</button>

			{#if nodes.length > 0}
				<button
					type="button"
					onclick={clearCanvas}
					class="rounded-lg p-1.5 text-muted-foreground hover:bg-destructive/15 hover:text-destructive"
					title="Clear canvas"
					aria-label="Clear canvas"
				>
					<Trash2 class="h-3.5 w-3.5" />
				</button>
			{/if}
		</div>
	</div>

	<!-- Transformed Spatial World Layer -->
	<div
		data-canvas-pan-surface
		class="pointer-events-none absolute inset-0 origin-top-left"
		style="transform: translate({camera.x}px, {camera.y}px) scale({camera.zoom});"
	>
		<!-- Project spatial zone frames -->
		{#each projectZones as zone (zone.projectId)}
			<div
				data-canvas-project-zone={zone.projectId}
				style="left:{zone.x}px;top:{zone.y}px;width:{zone.width}px;height:{zone.height}px;"
				class="pointer-events-none absolute rounded-2xl border border-dashed border-primary/25 bg-primary/[0.03]"
			>
				<div class="inline-flex items-center gap-1.5 rounded-br-xl rounded-tl-2xl border-b border-r border-primary/20 bg-card/85 px-3 py-1 text-xs font-semibold text-foreground shadow-sm">
					<span>{zone.icon || zone.name.slice(0, 1).toUpperCase()}</span>
					<span>{zone.name}</span>
					<span class="text-[10px] font-normal text-muted-foreground">{zone.nodeCount} windows</span>
				</div>
			</div>
		{/each}

		<!-- SVG Bezier Connection Wires -->
		<svg
			data-canvas-edges
			class="pointer-events-none absolute left-0 top-0 overflow-visible"
			width="1"
			height="1"
		>
			<defs>
				<marker id="canvas-arrow-context" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
					<path d="M 0 1 L 10 5 L 0 9 z" fill="hsl(var(--border-strong))" />
				</marker>
				<marker id="canvas-arrow-delegation" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
					<path d="M 0 1 L 10 5 L 0 9 z" fill="#3b82f6" />
				</marker>
				<marker id="canvas-arrow-dependency" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
					<path d="M 0 1 L 10 5 L 0 9 z" fill="#f59e0b" />
				</marker>
			</defs>
			{#each edges as edge (edge.id)}
				{@const from = nodeById.get(edge.fromId)}
				{@const to = nodeById.get(edge.toId)}
				{#if from && to}
					{@const geometry = bezierEdgePath(from, to)}
					{@const stroke = edgeStroke(edge.kind, edge.status)}
					{@const active = edge.kind === 'delegation' && Boolean(edge.status && ['running', 'in_progress', 'preparing'].includes(edge.status))}
					<g data-canvas-edge={edge.id} data-canvas-edge-kind={edge.kind}>
						<path
							d={geometry.path}
							fill="none"
							stroke={stroke}
							stroke-width={edge.kind === 'delegation' ? 2.5 : 2}
							stroke-dasharray={edge.kind === 'dependency' ? '6 4' : active ? '8 5' : undefined}
							class={active ? 'canvas-edge-animated' : ''}
							marker-end={edge.kind === 'delegation' ? 'url(#canvas-arrow-delegation)' : edge.kind === 'dependency' ? 'url(#canvas-arrow-dependency)' : 'url(#canvas-arrow-context)'}
						/>
						{#if edge.label}
							<g transform="translate({geometry.midX}, {geometry.midY})">
								<rect x="-44" y="-10" width="88" height="20" rx="10" fill="hsl(var(--card))" stroke={stroke} stroke-width="1" />
								<text text-anchor="middle" dominant-baseline="central" font-size="10" fill="hsl(var(--foreground-secondary))" font-weight="500">
									{edge.label}
								</text>
							</g>
						{/if}
					</g>
				{/if}
			{/each}
		</svg>

		<!-- Floating Spatial Windows -->
		<div class="pointer-events-auto">
			{#each nodes as node (node.id)}
				{@const delegated = node.kind === 'task' && node.taskId ? (delegatedByParentTask.get(node.taskId)?.length ?? 0) : 0}
				<CanvasWindow
					{node}
					focused={focusedNodeId === node.id}
					{visible}
					zoom={camera.zoom}
					canvasInteracting={panning || spacePressed}
					status={nodeStatus(node)}
					contextLabel={nodeContextLabel(node)}
					delegatedCount={delegated}
					canSpawnTools={node.kind === 'project' || node.kind === 'task' || node.kind === 'conversation' || node.kind === 'agent'}
					hasRoute={Boolean(node.route)}
					onFocus={() => bringToFront(node.id)}
					onMove={(x, y, commit) => updateNode(node.id, {
						x: commit ? snapCoordinate(x, snapToGrid) : Math.round(x),
						y: commit ? snapCoordinate(y, snapToGrid) : Math.round(y),
					}, commit)}
					onResize={(width, height, commit) => updateNode(node.id, {
						width: commit ? snapCoordinate(width, snapToGrid) : width,
						height: commit ? snapCoordinate(height, snapToGrid) : height,
					}, commit)}
					onToggleCollapse={() => updateNode(node.id, { collapsed: !node.collapsed })}
					onCenter={() => centerOnNode(node)}
					onOpenInTabs={node.route ? () => onOpenInTabs(node.route!) : undefined}
					onClose={() => closeNode(node.id)}
					onSpawnTool={(kind) => spawnToolNode(kind, node.agentId, node.id)}
					onExpandDelegated={node.taskId ? () => expandDelegatedForTask(node.taskId!, node.id) : undefined}
				>
					{#if node.kind === 'project'}
						<CanvasProjectCard
							nodeId={node.id}
							project={node.projectId ? projectById.get(node.projectId) ?? null : null}
							{agents}
							{conversations}
							{tasks}
							onSpawnTask={(task, parentId) => spawnTaskNode(task, parentId)}
							onSpawnConversation={(conversation, parentId) => spawnConversationNode(conversation, parentId)}
							onSpawnAgent={(agent, parentId) => spawnAgentNode(agent, parentId)}
							onSpawnTool={(kind, agentId, parentId) => spawnToolNode(kind, agentId, parentId)}
							onExpandActiveTasks={expandActiveTasksForProject}
						/>
					{:else if node.kind === 'editor'}
						<CanvasEditorCard
							nodeId={node.id}
							agentId={node.agentId}
							initialPath={node.filePath}
							{agents}
							onUpdateNode={(patch) => updateNode(node.id, patch)}
							onDirtyChange={(dirty) => setEditorDirty(node.id, dirty)}
							onOpenImage={(agentId, filePath, parentId) => spawnToolNode('image', agentId, parentId, { filePath, imageSource: 'workspace' })}
							onOpenHtmlApp={(agentId, filePath, parentId) => spawnToolNode('app', agentId, parentId, { appSource: 'workspace-html', appFilePath: filePath })}
						/>
					{:else if node.kind === 'terminal'}
						<CanvasTerminalCard
							agentId={node.agentId}
							initialSession={node.terminalSession}
							{agents}
							onUpdateNode={(patch) => updateNode(node.id, patch)}
						/>
					{:else if node.kind === 'image'}
						<CanvasImageCard
							agentId={node.agentId}
							initialPath={node.filePath}
							initialUrl={node.imageUrl}
							initialSource={node.imageSource}
							{agents}
							onUpdateNode={(patch) => updateNode(node.id, patch)}
						/>
					{:else if node.kind === 'app'}
						<CanvasAppCard
							nodeId={node.id}
							agentId={node.agentId}
							initialSource={node.appSource}
							initialUrl={node.appUrl}
							initialFilePath={node.appFilePath}
							{agents}
							onUpdateNode={(patch) => updateNode(node.id, patch)}
							onOpenEditor={(agentId, filePath, parentId) => spawnToolNode('editor', agentId, parentId, { filePath })}
						/>
					{:else if node.route}
						<WorkspaceContent tab={createWorkspaceTab(node.route)} compact={true} />
					{/if}
				</CanvasWindow>
			{/each}
		</div>
	</div>

	<!-- Empty canvas onboarding hint -->
	{#if initialized && nodes.length === 0}
		<div
			role="region"
			aria-label="Empty canvas actions"
			data-canvas-empty
			onpointerdown={(event) => event.stopPropagation()}
			class="pointer-events-auto m-auto max-w-md rounded-2xl border border-dashed border-border bg-card/90 p-6 text-center shadow-lg backdrop-blur"
		>
			<h2 class="text-sm font-semibold text-foreground">Spatial Canvas Workspace</h2>
			<p class="mt-1.5 text-xs text-muted-foreground">
				Arrange Projects, parallel Task chats, Conversations, Code Editors, Terminals, Images, and live Web Apps spatially with visual context links.
			</p>
			<div class="mt-4 flex flex-wrap justify-center gap-2">
				<button type="button" onclick={syncActiveWork} class="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
					Load Projects & Active Work
				</button>
				<button type="button" onclick={() => (addMenuOpen = true)} class="rounded-lg border border-border bg-card px-3 py-1.5 text-xs font-medium hover:bg-accent">
					Add Window…
				</button>
			</div>
		</div>
	{/if}

	<!-- Bottom-left navigation hint -->
	<div
		data-canvas-hud
		class="pointer-events-none absolute bottom-3 left-3 z-30 hidden rounded-lg border border-border/80 bg-card/85 px-2.5 py-1 text-[10px] text-muted-foreground backdrop-blur md:block"
	>
		Drag background or Space+drag to pan · Ctrl/⌘+scroll to zoom · {nodes.length} window{nodes.length === 1 ? '' : 's'}
	</div>

	<!-- Interactive Minimap -->
	{#if showMinimap && nodes.length > 0}
		<div
			data-canvas-minimap
			data-canvas-hud
			role="presentation"
			onpointerdown={(event) => { event.stopPropagation(); minimapNavigate(event); }}
			style="width:{minimapMetrics.mapW}px;height:{minimapMetrics.mapH}px;"
			class="pointer-events-auto absolute bottom-3 right-3 z-30 cursor-crosshair overflow-hidden rounded-xl border border-border bg-card/90 shadow-[var(--shadow-overlay)] backdrop-blur"
			title="Click or drag to pan canvas"
		>
			{#each nodes as node (node.id)}
				<div
					style="left:{6 + (node.x - minimapMetrics.minX) * minimapMetrics.scale}px;top:{6 + (node.y - minimapMetrics.minY) * minimapMetrics.scale}px;width:{Math.max(6, node.width * minimapMetrics.scale)}px;height:{Math.max(4, nodeVisualHeight(node) * minimapMetrics.scale)}px;"
					class="absolute rounded-[2px] {minimapNodeColor(node.kind)} {focusedNodeId === node.id ? 'ring-1 ring-foreground' : ''}"
				></div>
			{/each}
			<div
				style="left:{6 + (-camera.x / camera.zoom - minimapMetrics.minX) * minimapMetrics.scale}px;top:{6 + (-camera.y / camera.zoom - minimapMetrics.minY) * minimapMetrics.scale}px;width:{Math.max(12, (viewportWidth / camera.zoom) * minimapMetrics.scale)}px;height:{Math.max(10, (viewportHeight / camera.zoom) * minimapMetrics.scale)}px;"
				class="pointer-events-none absolute rounded border border-primary bg-primary/10"
			></div>
		</div>
	{/if}
</div>

<style>
	.canvas-edge-animated {
		animation: canvas-dash-flow 1.2s linear infinite;
	}
	@keyframes canvas-dash-flow {
		from {
			stroke-dashoffset: 26;
		}
		to {
			stroke-dashoffset: 0;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.canvas-edge-animated {
			animation: none;
		}
	}
</style>
