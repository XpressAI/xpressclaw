<script lang="ts">
	import { onDestroy, onMount, untrack } from 'svelte';
	import {
		Bot,
		Code2,
		Download,
		ExternalLink,
		FolderKanban,
		GitBranch,
		Globe,
		Grid3x3,
		GripVertical,
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
	import { workspaces, type Agent, type Conversation, type Project, type Task } from '$lib/api';
	import { notifyLiveUpdatesVisibilityChanged } from '$lib/liveUpdates';
	import { openExternal } from '$lib/utils';
	import {
		bezierEdgePath,
		CANVAS_GRID_SIZE,
		clampZoom,
		classifyCanvasLink,
		computeCenterOnNodeCamera,
		computeFitCamera,
		computeProjectZones,
		createCanvasNode,
		deriveCanvasEdges,
		extractActiveWorkspaceRoots,
		inferTaskProjectId,
		isImageFilePath,
		nodeVisualHeight,
		normalizePersistedCanvasState,
		resolveWorkspaceRelativePath,
		seedCanvasNodes,
		snapCoordinate,
		tidyCanvasLayout,
		tidyFilteredCanvasNodes,
		type CanvasCamera,
		type CanvasLinkPreviewTarget,
		type CanvasNode,
		type CanvasNodeKind,
		type CanvasState,
	} from '$lib/canvas';
	import { createWorkspaceTab, describeWorkspacePath, type WorkspaceTab } from '$lib/workspace';
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
		if (visible) {
			notifyLiveUpdatesVisibilityChanged();
		} else {
			linkPopover = null;
		}
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
	let activeZoneDrag = $state<{ projectId: string; dx: number; dy: number } | null>(null);
	let seenDelegatedTaskIds = new Set<string>();
	let dirtyEditorIds = $state<Record<string, boolean>>({});
	let retainedNodeIds = $state<Record<string, boolean>>({});
	let activeInteractionCleanup: (() => void) | null = null;
	let persistTimer: ReturnType<typeof setTimeout> | null = null;
	let visibilityNotifyTimer: ReturnType<typeof setTimeout> | null = null;
	const workspaceTabCache = new Map<string, WorkspaceTab>();

	interface LinkPopoverState {
		id: number;
		x: number;
		y: number;
		sourceNodeId: string;
		agentId: string | null;
		target: CanvasLinkPreviewTarget;
		resolvingWorkspace: boolean;
		workspaceRelativePath: string | null;
		notice: string | null;
	}

	let linkPopover = $state<LinkPopoverState | null>(null);
	let linkPopoverSequence = 0;
	const workspaceRootsPending = new Map<string, Promise<string[]>>();

	let agentById = $derived(new Map(agents.map((agent) => [agent.id, agent])));
	let projectById = $derived(new Map(projects.map((project) => [project.id, project])));
	let conversationById = $derived(new Map(conversations.map((conversation) => [conversation.id, conversation])));
	let taskById = $derived(new Map(tasks.map((task) => [task.id, task])));
	let nodeById = $derived(new Map(nodes.map((node) => [node.id, node])));

	function resolveNodeProjectId(node: CanvasNode): string | null {
		if (node.projectId) return node.projectId;
		if (node.kind === 'task' && node.taskId) {
			return inferTaskProjectId(taskById.get(node.taskId), agentById, conversationById);
		}
		if (node.kind === 'conversation' && node.conversationId) {
			return conversationById.get(node.conversationId)?.project_id ?? null;
		}
		if (node.agentId) {
			return agentById.get(node.agentId)?.project_id ?? null;
		}
		return null;
	}

	let visibleNodes = $derived(
		activeProjectFilter === null
			? nodes
			: nodes.filter((node) => resolveNodeProjectId(node) === activeProjectFilter)
	);
	// Unmount clean non-selected project windows to free DOM/timers/observers/network,
	// while keeping any window with unsaved local state (dirty editors, attachments,
	// open edit/task forms, elicitation drafts) hidden in the background so work is never lost.
	let renderedNodes = $derived(
		activeProjectFilter === null
			? nodes
			: nodes.filter(
				(node) => resolveNodeProjectId(node) === activeProjectFilter
					|| (node.kind === 'editor' && Boolean(dirtyEditorIds[node.id]))
					|| Boolean(retainedNodeIds[node.id])
			)
	);
	let visibleNodeById = $derived(new Map(visibleNodes.map((node) => [node.id, node])));
	let activeProject = $derived(activeProjectFilter ? projectById.get(activeProjectFilter) ?? null : null);

	let edges = $derived(deriveCanvasEdges(visibleNodes, tasks, agents, conversations));
	let projectZones = $derived(computeProjectZones(visibleNodes, projects));

	let projectNodeCounts = $derived((() => {
		const counts = new Map<string, number>();
		for (const node of nodes) {
			const pid = resolveNodeProjectId(node);
			if (pid) counts.set(pid, (counts.get(pid) ?? 0) + 1);
		}
		return counts;
	})());

	let projectStatusById = $derived((() => {
		const rank: Record<string, number> = { ready: 0, running: 1, waiting_for_input: 2, blocked: 3 };
		const map = new Map<string, string>();
		for (const task of tasks) {
			if (['completed', 'cancelled'].includes(task.status)) continue;
			const pid = inferTaskProjectId(task, agentById, conversationById);
			if (!pid) continue;
			const state = task.activity_status ?? task.status;
			const next = ['failed', 'error', 'blocked'].includes(state)
				? 'blocked'
				: state === 'waiting_for_input'
					? 'waiting_for_input'
					: ['running', 'in_progress', 'preparing'].includes(state)
						? 'running'
						: 'ready';
			const prev = map.get(pid) ?? 'ready';
			if ((rank[next] ?? 0) > (rank[prev] ?? 0)) map.set(pid, next);
		}
		return map;
	})());

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

	let visibleWorldBounds = $derived((() => {
		const margin = 240 / Math.max(0.1, camera.zoom);
		return {
			minX: -camera.x / camera.zoom - margin,
			minY: -camera.y / camera.zoom - margin,
			maxX: (viewportWidth - camera.x) / camera.zoom + margin,
			maxY: (viewportHeight - camera.y) / camera.zoom + margin,
		};
	})());

	function isNodeNearViewport(node: CanvasNode): boolean {
		const height = nodeVisualHeight(node);
		return node.x + node.width >= visibleWorldBounds.minX
			&& node.x <= visibleWorldBounds.maxX
			&& node.y + height >= visibleWorldBounds.minY
			&& node.y <= visibleWorldBounds.maxY;
	}

	function workspaceTabFor(route: string): WorkspaceTab {
		let cached = workspaceTabCache.get(route);
		if (!cached) {
			cached = createWorkspaceTab(route);
			workspaceTabCache.set(route, cached);
		}
		return cached;
	}

	function scheduleVisibilityRefresh(immediate = false) {
		if (visibilityNotifyTimer !== null) {
			clearTimeout(visibilityNotifyTimer);
			visibilityNotifyTimer = null;
		}
		if (immediate) {
			notifyLiveUpdatesVisibilityChanged();
			return;
		}
		visibilityNotifyTimer = setTimeout(() => {
			visibilityNotifyTimer = null;
			notifyLiveUpdatesVisibilityChanged();
		}, 180);
	}

	let activeNodeSignature = $derived(
		!visible
			? ''
			: renderedNodes
				.filter((node) => {
					const nodeHidden = activeProjectFilter !== null && resolveNodeProjectId(node) !== activeProjectFilter;
					return !nodeHidden && !node.collapsed && (focusedNodeId === node.id || isNodeNearViewport(node));
				})
				.map((node) => node.id)
				.join(',')
	);

	$effect(() => {
		// Notify live-update subscribers whenever the active window set changes
		// (pan, zoom, drag, group drag, resize, expand/collapse, tidy, filter, focus, or tab switch).
		void activeNodeSignature;
		scheduleVisibilityRefresh(!visible);
	});

	$effect(() => {
		// Keep memoized WorkspaceTab cache bounded to routes currently present in nodes.
		const activeRoutes = new Set<string>();
		for (const node of nodes) {
			if (node.route) activeRoutes.add(node.route);
		}
		for (const route of workspaceTabCache.keys()) {
			if (!activeRoutes.has(route)) workspaceTabCache.delete(route);
		}
	});

	$effect(() => {
		// Reset stale project filter if the filtered project was deleted.
		if (initialized && activeProjectFilter !== null && projects.length > 0 && !projectById.has(activeProjectFilter)) {
			activeProjectFilter = null;
			persist();
		}
	});

	$effect(() => {
		// Keep seenDelegatedTaskIds bounded over multi-hour sessions.
		// Read reactive props before the size guard so Svelte tracks updates.
		const currentTasks = tasks;
		const currentNodes = nodes;
		if (seenDelegatedTaskIds.size <= 200) return;
		const keep = new Set<string>();
		for (const task of currentTasks) keep.add(task.id);
		for (const node of currentNodes) {
			if (node.taskId) keep.add(node.taskId);
		}
		for (const id of seenDelegatedTaskIds) {
			if (!keep.has(id)) seenDelegatedTaskIds.delete(id);
		}
	});

	let gridPixelSize = $derived(Math.max(12, CANVAS_GRID_SIZE * camera.zoom));
	let filteredAddTasks = $derived(
		tasks
			.filter((task) => !addSearch.trim() || task.title.toLowerCase().includes(addSearch.trim().toLowerCase()))
			.slice(0, 8)
	);

	// Minimap world bounds (scoped to visible nodes when a project filter is active).
	let minimapMetrics = $derived((() => {
		const mapW = 196;
		const mapH = 128;
		let minX = -camera.x / camera.zoom;
		let minY = -camera.y / camera.zoom;
		let maxX = (viewportWidth - camera.x) / camera.zoom;
		let maxY = (viewportHeight - camera.y) / camera.zoom;
		for (const node of visibleNodes) {
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
		try {
			return localStorage;
		} catch (err) {
			console.warn('Canvas storage is unavailable:', err);
			return null;
		}
	}

	function flushPersist() {
		if (persistTimer !== null) {
			clearTimeout(persistTimer);
			persistTimer = null;
			persist();
		}
	}

	function schedulePersist(delayMs = 250) {
		if (!initialized) return;
		if (persistTimer !== null) clearTimeout(persistTimer);
		persistTimer = setTimeout(() => {
			persistTimer = null;
			persist();
		}, delayMs);
	}

	function persist() {
		if (persistTimer !== null) {
			clearTimeout(persistTimer);
			persistTimer = null;
		}
		if (!initialized) return;
		const target = activeStorage();
		if (!target) return;
		const state: CanvasState = {
			nodes,
			camera,
			focusedNodeId,
			activeProjectFilter,
			snapToGrid,
			showMinimap,
			autoPinDelegated,
		};
		try {
			target.setItem(storageKey, JSON.stringify(state));
		} catch (err) {
			console.warn(`Failed to persist canvas state under ${storageKey}:`, err);
		}
	}

	onDestroy(() => {
		activeInteractionCleanup?.();
		activeInteractionCleanup = null;
		if (visibilityNotifyTimer !== null) {
			clearTimeout(visibilityNotifyTimer);
			visibilityNotifyTimer = null;
		}
		flushPersist();
	});

	onMount(() => {
		try {
			const raw = activeStorage()?.getItem(storageKey);
			if (raw) {
				const restored = normalizePersistedCanvasState(JSON.parse(raw));
				if (restored) {
					nodes = restored.nodes;
					camera = restored.camera;
					focusedNodeId = restored.focusedNodeId;
					activeProjectFilter = restored.activeProjectFilter ?? null;
					snapToGrid = restored.snapToGrid;
					showMinimap = restored.showMinimap;
					autoPinDelegated = restored.autoPinDelegated;
					seededOnce = restored.nodes.length > 0;
				}
			}
		} catch (err) {
			console.warn(`Failed to restore canvas state from ${storageKey}:`, err);
		}
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
			if (event.key === 'Escape') {
				if (linkPopover) linkPopover = null;
				if (addMenuOpen) addMenuOpen = false;
			}
		};
		const handleKeyUp = (event: KeyboardEvent) => {
			if (event.code === 'Space') spacePressed = false;
		};
		const handleBeforeUnload = () => flushPersist();
		window.addEventListener('keydown', handleKeyDown);
		window.addEventListener('keyup', handleKeyUp);
		window.addEventListener('beforeunload', handleBeforeUnload);
		window.addEventListener('pagehide', handleBeforeUnload);
		const el = viewportEl;
		el?.addEventListener('click', handleCanvasClick);

		return () => {
			observer.disconnect();
			window.removeEventListener('keydown', handleKeyDown);
			window.removeEventListener('keyup', handleKeyUp);
			window.removeEventListener('beforeunload', handleBeforeUnload);
			window.removeEventListener('pagehide', handleBeforeUnload);
			el?.removeEventListener('click', handleCanvasClick);
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
		camera = computeFitCamera(visibleNodes, viewportWidth, viewportHeight);
		persist();
	}

	function tidyLayout() {
		if (activeProjectFilter === null) {
			nodes = tidyCanvasLayout(nodes, tasks, projects);
			camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		} else {
			const result = tidyFilteredCanvasNodes(nodes, visibleNodes, tasks, projects, snapToGrid);
			nodes = result.nodes;
			camera = computeFitCamera(result.tidiedVisible, viewportWidth, viewportHeight);
		}
		persist();
	}

	function zoomBy(factor: number, clientX?: number, clientY?: number, immediatePersist = true) {
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
		if (immediatePersist) persist();
		else schedulePersist();
	}

	function resetZoom() {
		zoomBy(1 / camera.zoom);
	}

	function startPan(event: PointerEvent) {
		event.preventDefault();
		event.stopPropagation();
		activeInteractionCleanup?.();
		addMenuOpen = false;
		linkPopover = null;
		panning = true;
		const startX = event.clientX;
		const startY = event.clientY;
		const originX = camera.x;
		const originY = camera.y;
		let latestX = originX;
		let latestY = originY;
		let frame = 0;

		const flush = () => {
			frame = 0;
			camera = { ...camera, x: latestX, y: latestY };
		};
		const move = (moveEvent: PointerEvent) => {
			latestX = Math.round(originX + (moveEvent.clientX - startX));
			latestY = Math.round(originY + (moveEvent.clientY - startY));
			if (!frame) frame = requestAnimationFrame(flush);
		};
		const teardown = (commit: boolean) => {
			if (frame) {
				cancelAnimationFrame(frame);
				frame = 0;
			}
			panning = false;
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			if (activeInteractionCleanup === abort) activeInteractionCleanup = null;
			if (commit) {
				camera = { ...camera, x: latestX, y: latestY };
				persist();
			}
		};
		const up = () => teardown(true);
		const abort = () => teardown(false);
		activeInteractionCleanup = abort;
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
	}

	function startZoneDrag(projectId: string, event: PointerEvent) {
		if (event.button !== 0 || spacePressed || panning) return;
		event.preventDefault();
		event.stopPropagation();
		activeInteractionCleanup?.();
		addMenuOpen = false;
		linkPopover = null;

		const movedIds = new Set(
			visibleNodes
				.filter((node) => resolveNodeProjectId(node) === projectId)
				.map((node) => node.id)
		);
		if (movedIds.size === 0) return;

		const startClientX = event.clientX;
		const startClientY = event.clientY;
		const startZoom = camera.zoom;
		let latestDx = 0;
		let latestDy = 0;
		let frame = 0;
		activeZoneDrag = { projectId, dx: 0, dy: 0 };

		const flush = () => {
			frame = 0;
			activeZoneDrag = { projectId, dx: Math.round(latestDx), dy: Math.round(latestDy) };
		};
		const move = (moveEvent: PointerEvent) => {
			latestDx = (moveEvent.clientX - startClientX) / startZoom;
			latestDy = (moveEvent.clientY - startClientY) / startZoom;
			if (!frame) frame = requestAnimationFrame(flush);
		};
		const teardown = (commit: boolean) => {
			if (frame) {
				cancelAnimationFrame(frame);
				frame = 0;
			}
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			if (activeInteractionCleanup === abort) activeInteractionCleanup = null;
			activeZoneDrag = null;
			if (!commit) return;
			const finalDx = snapCoordinate(latestDx, snapToGrid);
			const finalDy = snapCoordinate(latestDy, snapToGrid);
			if (finalDx === 0 && finalDy === 0) return;
			nodes = nodes.map((node) =>
				movedIds.has(node.id)
					? { ...node, x: node.x + finalDx, y: node.y + finalDy }
					: node
			);
			persist();
		};
		const up = () => teardown(true);
		const abort = () => teardown(false);
		activeInteractionCleanup = abort;
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
	}

	function effectiveNodePosition(node: CanvasNode): CanvasNode {
		if (!activeZoneDrag || resolveNodeProjectId(node) !== activeZoneDrag.projectId) return node;
		return { ...node, x: node.x + activeZoneDrag.dx, y: node.y + activeZoneDrag.dy };
	}

	function handleCapturePointerDown(event: PointerEvent) {
		const target = event.target as HTMLElement | null;
		if (linkPopover && !target?.closest('[data-canvas-link-popover]')) {
			linkPopover = null;
		}
		if (target?.closest('[data-canvas-hud]')) return;
		if (event.button === 1 || (event.button === 0 && spacePressed)) {
			startPan(event);
		}
	}

	function handleBackgroundPointerDown(event: PointerEvent) {
		addMenuOpen = false;
		linkPopover = null;
		const isMiddle = event.button === 1;
		const isLeftOnBackground = event.button === 0 && (spacePressed || event.target === event.currentTarget || (event.target as HTMLElement)?.hasAttribute('data-canvas-pan-surface'));
		if (!isMiddle && !isLeftOnBackground) return;
		startPan(event);
	}

	function handleMinimapWheel(event: WheelEvent) {
		event.preventDefault();
		event.stopPropagation();
		linkPopover = null;
		if (event.deltaY === 0) return;
		const factor = event.deltaY < 0 ? 1.1 : 1 / 1.1;
		zoomBy(factor, undefined, undefined, false);
	}

	function handleWheel(event: WheelEvent) {
		// Zoom on Ctrl/Cmd+wheel (and trackpad pinch) anywhere over the canvas.
		if (event.ctrlKey || event.metaKey) {
			event.preventDefault();
			linkPopover = null;
			const factor = event.deltaY < 0 ? 1.1 : 1 / 1.1;
			zoomBy(factor, event.clientX, event.clientY, false);
			return;
		}
		// Only pan with wheel when hovering the empty canvas background so scrolling
		// inside task chats, Monaco editors, and terminals works naturally!
		const target = event.target as HTMLElement | null;
		if (target?.closest('[data-canvas-node], [data-canvas-hud]')) return;
		event.preventDefault();
		linkPopover = null;
		camera = {
			...camera,
			x: Math.round(camera.x - event.deltaX),
			y: Math.round(camera.y - event.deltaY),
		};
		schedulePersist();
	}

	async function loadWorkspaceRoots(agentId: string): Promise<string[]> {
		const inflight = workspaceRootsPending.get(agentId);
		if (inflight) return inflight;
		const req = workspaces.status(agentId)
			.then((status) => extractActiveWorkspaceRoots(status))
			.catch(() => [])
			.finally(() => {
				workspaceRootsPending.delete(agentId);
			});
		workspaceRootsPending.set(agentId, req);
		return req;
	}

	function handleCanvasClick(event: MouseEvent) {
		if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) {
			return;
		}
		const targetEl = event.target as HTMLElement | null;
		if (!targetEl || targetEl.closest('[data-canvas-hud], [data-canvas-link-popover]')) return;
		const link = targetEl.closest<HTMLAnchorElement>('a[href]');
		if (!link || link.hasAttribute('data-canvas-no-preview')) return;
		if (link.closest('header')) return;
		const nodeEl = link.closest<HTMLElement>('[data-canvas-node]');
		const sourceNodeId = nodeEl?.getAttribute('data-canvas-node');
		if (!sourceNodeId) return;
		const sourceNode = nodeById.get(sourceNodeId);
		if (!sourceNode || !['task', 'conversation', 'agent', 'project', 'new-work'].includes(sourceNode.kind)) {
			return;
		}

		const img = link.querySelector('img');
		const classified = classifyCanvasLink(
			{
				href: link.getAttribute('href') || link.href,
				downloadAttr: link.getAttribute('download'),
				text: link.textContent,
				hasImageChild: Boolean(img),
				imageAlt: img?.getAttribute('alt') ?? null,
				attachmentName: link.getAttribute('data-attachment-name'),
				attachmentMimeType: link.getAttribute('data-attachment-mime'),
			},
			window.location.origin,
		);
		if (!classified) return;

		event.preventDefault();
		event.stopPropagation();

		const rect = viewportEl?.getBoundingClientRect();
		const relX = rect ? event.clientX - rect.left : event.clientX;
		const relY = rect ? event.clientY - rect.top : event.clientY;
		const popoverId = ++linkPopoverSequence;
		const agentId = classified.downloadAgentId ?? sourceNode.agentId ?? null;

		let workspaceRelativePath: string | null = null;
		let resolvingWorkspace = false;
		if (classified.downloadAgentId && classified.downloadPath) {
			if (classified.href.startsWith('/api/workspaces/')) {
				workspaceRelativePath = resolveWorkspaceRelativePath(classified.downloadPath, []);
			} else {
				resolvingWorkspace = true;
			}
		}

		linkPopover = {
			id: popoverId,
			x: Math.max(12, Math.min(viewportWidth - 276, Math.round(relX + 8))),
			y: Math.max(64, Math.min(viewportHeight - 210, Math.round(relY + 8))),
			sourceNodeId,
			agentId,
			target: classified,
			resolvingWorkspace,
			workspaceRelativePath,
			notice: null,
		};

		if (resolvingWorkspace && classified.downloadAgentId && classified.downloadPath) {
			const targetAgentId = classified.downloadAgentId;
			const targetPath = classified.downloadPath;
			void loadWorkspaceRoots(targetAgentId).then((roots) => {
				if (linkPopover?.id !== popoverId) return;
				linkPopover = {
					...linkPopover,
					resolvingWorkspace: false,
					workspaceRelativePath: resolveWorkspaceRelativePath(targetPath, roots),
				};
			});
		}
	}

	async function resolvePopoverWorkspacePath(state: LinkPopoverState): Promise<string | null> {
		if (!state.target.downloadAgentId || !state.target.downloadPath) return null;
		if (state.target.href.startsWith('/api/workspaces/')) {
			return resolveWorkspaceRelativePath(state.target.downloadPath, []);
		}
		const roots = await loadWorkspaceRoots(state.target.downloadAgentId);
		return resolveWorkspaceRelativePath(state.target.downloadPath, roots);
	}

	async function previewPopoverAsImage() {
		const state = linkPopover;
		if (!state) return;
		const popoverId = state.id;
		const relPath = await resolvePopoverWorkspacePath(state);
		if (linkPopover?.id !== popoverId) return;
		linkPopover = null;
		if (relPath && state.agentId) {
			spawnToolNode('image', state.agentId, state.sourceNodeId, {
				filePath: relPath,
				imageSource: 'workspace',
				title: state.target.label,
			});
		} else {
			spawnToolNode('image', state.agentId, state.sourceNodeId, {
				imageUrl: state.target.href,
				imageSource: 'url',
				title: state.target.label,
			});
		}
	}

	async function previewPopoverAsApp() {
		const state = linkPopover;
		if (!state) return;
		const popoverId = state.id;
		if (state.target.category === 'html') {
			const relPath = await resolvePopoverWorkspacePath(state);
			if (linkPopover?.id !== popoverId) return;
			linkPopover = null;
			if (relPath && state.agentId) {
				spawnToolNode('app', state.agentId, state.sourceNodeId, {
					appSource: 'workspace-html',
					appFilePath: relPath,
					title: `App · ${state.target.label}`,
				});
			} else {
				spawnToolNode('app', state.agentId, state.sourceNodeId, {
					appSource: 'html-url',
					appUrl: state.target.href,
					title: `App · ${state.target.label}`,
				});
			}
			return;
		}
		linkPopover = null;
		spawnToolNode('app', state.agentId, state.sourceNodeId, {
			appSource: 'url',
			appUrl: state.target.href,
			title: `App · ${state.target.label}`,
		});
	}

	async function previewPopoverInEditor() {
		const state = linkPopover;
		if (!state) return;
		const popoverId = state.id;
		const relPath = await resolvePopoverWorkspacePath(state);
		if (linkPopover?.id !== popoverId) return;
		if (relPath && state.agentId) {
			linkPopover = null;
			spawnToolNode('editor', state.agentId, state.sourceNodeId, {
				filePath: relPath,
				title: state.target.label,
			});
			return;
		}
		linkPopover = {
			...state,
			resolvingWorkspace: false,
			workspaceRelativePath: null,
			notice: 'This file is outside the agent workspace; download it instead.',
		};
	}

	function openPopoverNormally() {
		const state = linkPopover;
		if (!state) return;
		linkPopover = null;
		const { target } = state;
		if (target.href.startsWith('http://') || target.href.startsWith('https://')) {
			void openExternal(target.href);
			return;
		}
		const anchor = document.createElement('a');
		anchor.href = target.href;
		anchor.setAttribute('data-canvas-no-preview', 'true');
		if (target.downloadAttr !== null || target.downloadPath !== null) {
			anchor.download = target.downloadAttr || target.fileName || '';
		} else {
			anchor.target = '_blank';
			anchor.rel = 'noopener noreferrer';
		}
		document.body.appendChild(anchor);
		anchor.click();
		anchor.remove();
	}

	function updateNode(id: string, patch: Partial<CanvasNode>, shouldPersist = true) {
		nodes = nodes.map((node) => node.id === id ? { ...node, ...patch } : node);
		if (shouldPersist) persist();
	}

	function setEditorDirty(id: string, dirty: boolean) {
		untrack(() => {
			if (dirty) {
				if (!dirtyEditorIds[id]) dirtyEditorIds = { ...dirtyEditorIds, [id]: true };
			} else if (dirtyEditorIds[id]) {
				const next = { ...dirtyEditorIds };
				delete next[id];
				dirtyEditorIds = next;
			}
		});
	}

	function setNodeRetained(id: string, retained: boolean) {
		untrack(() => {
			if (retained) {
				if (!retainedNodeIds[id]) retainedNodeIds = { ...retainedNodeIds, [id]: true };
			} else if (retainedNodeIds[id]) {
				const next = { ...retainedNodeIds };
				delete next[id];
				retainedNodeIds = next;
			}
		});
	}

	function ensureFilterShowsProject(projectId: string | null) {
		if (activeProjectFilter === null || projectId === activeProjectFilter) return;
		activeProjectFilter = projectId;
	}

	function closeNode(id: string) {
		if (dirtyEditorIds[id]) {
			const target = nodeById.get(id);
			const label = target?.filePath || target?.title || 'this editor';
			if (!window.confirm(`Discard unsaved changes in ${label}?`)) return;
		}
		setEditorDirty(id, false);
		setNodeRetained(id, false);
		nodes = nodes.filter((node) => node.id !== id).map((node) => node.parentId === id ? { ...node, parentId: null } : node);
		if (focusedNodeId === id) focusedNodeId = visibleNodes.at(-1)?.id ?? nodes.at(-1)?.id ?? null;
		persist();
	}

	function clearCanvas() {
		const targetNodes = activeProjectFilter === null ? nodes : visibleNodes;
		if (targetNodes.length === 0) return;
		const targetIds = new Set(targetNodes.map((node) => node.id));
		const scopeLabel = activeProject ? `${activeProject.name}` : 'the canvas';
		const dirtyEditors = targetNodes.filter((node) => node.kind === 'editor' && Boolean(dirtyEditorIds[node.id]));
		const unsavedOthers = targetNodes.filter((node) => node.kind !== 'editor' && Boolean(retainedNodeIds[node.id]));
		if (dirtyEditors.length > 0) {
			const message = dirtyEditors.length === 1
				? `Discard unsaved changes in ${dirtyEditors[0].filePath || dirtyEditors[0].title || '1 editor'} and clear ${scopeLabel}?`
				: `Discard unsaved changes in ${dirtyEditors.length} editors and clear ${scopeLabel}?`;
			if (!window.confirm(message)) return;
		} else if (unsavedOthers.length > 0) {
			const message = unsavedOthers.length === 1
				? `Discard unsaved form or attachment state in ${unsavedOthers[0].title} and clear ${scopeLabel}?`
				: `Discard unsaved form or attachment state in ${unsavedOthers.length} windows and clear ${scopeLabel}?`;
			if (!window.confirm(message)) return;
		}
		const nextDirty = { ...dirtyEditorIds };
		const nextRetained = { ...retainedNodeIds };
		for (const id of targetIds) {
			delete nextDirty[id];
			delete nextRetained[id];
		}
		dirtyEditorIds = nextDirty;
		retainedNodeIds = nextRetained;
		nodes = nodes
			.filter((node) => !targetIds.has(node.id))
			.map((node) => node.parentId && targetIds.has(node.parentId) ? { ...node, parentId: null } : node);
		if (focusedNodeId && targetIds.has(focusedNodeId)) {
			focusedNodeId = visibleNodes.at(-1)?.id ?? nodes.at(-1)?.id ?? null;
		}
		persist();
	}

	function addOrFocusNode(
		matcher: (node: CanvasNode) => boolean,
		partial: Partial<CanvasNode> & Pick<CanvasNode, 'kind' | 'title'>,
		center = true,
	): CanvasNode {
		const existing = nodes.find(matcher);
		if (existing) {
			if (center) ensureFilterShowsProject(resolveNodeProjectId(existing));
			if (existing.collapsed) updateNode(existing.id, { collapsed: false });
			if (center) centerOnNode(existing);
			else bringToFront(existing.id);
			return existing;
		}
		const created = createCanvasNode(partial, nodes, viewportCenterWorld());
		if (center) ensureFilterShowsProject(resolveNodeProjectId(created));
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
			ensureFilterShowsProject(resolveNodeProjectId(existing));
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
			appSource?: 'port' | 'workspace-html' | 'html-url' | 'url' | null;
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

		// Deduplicate editor/image/app windows targeting the exact same file or URL if already open.
		const focusExisting = (existing: CanvasNode | undefined): boolean => {
			if (!existing) return false;
			ensureFilterShowsProject(resolveNodeProjectId(existing));
			if (existing.collapsed) updateNode(existing.id, { collapsed: false });
			centerOnNode(existing);
			return true;
		};
		if (kind === 'editor' && resolvedAgentId && options?.filePath) {
			if (focusExisting(nodes.find((node) => node.kind === 'editor' && node.agentId === resolvedAgentId && node.filePath === options.filePath))) return;
		}
		if (kind === 'image' && resolvedAgentId && options?.filePath) {
			if (focusExisting(nodes.find((node) => node.kind === 'image' && node.agentId === resolvedAgentId && node.filePath === options.filePath))) return;
		}
		if (kind === 'image' && options?.imageUrl) {
			if (focusExisting(nodes.find((node) => node.kind === 'image' && node.imageUrl === options.imageUrl))) return;
		}
		if (kind === 'app' && resolvedAgentId && options?.appFilePath) {
			if (focusExisting(nodes.find((node) => node.kind === 'app' && node.agentId === resolvedAgentId && node.appFilePath === options.appFilePath))) return;
		}
		if (kind === 'app' && options?.appUrl) {
			if (focusExisting(nodes.find((node) => node.kind === 'app' && node.appUrl === options.appUrl))) return;
		}

		ensureFilterShowsProject(resolvedProjectId);
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
		if (activeProjectFilter === null) {
			nodes = tidyCanvasLayout(nextNodes, tasks, projects);
			camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
		} else {
			const filtered = nextNodes.filter((node) => resolveNodeProjectId(node) === activeProjectFilter);
			const result = tidyFilteredCanvasNodes(nextNodes, filtered, tasks, projects, snapToGrid);
			nodes = result.nodes;
			camera = computeFitCamera(result.tidiedVisible, viewportWidth, viewportHeight);
		}
		persist();
	}

	function jumpToProject(projectId: string | null) {
		activeProjectFilter = projectId;
		linkPopover = null;
		addMenuOpen = false;
		if (!projectId) {
			camera = computeFitCamera(nodes, viewportWidth, viewportHeight);
			persist();
			scheduleVisibilityRefresh(true);
			return;
		}
		const projectNodes = nodes.filter((node) => resolveNodeProjectId(node) === projectId);
		if (projectNodes.length > 0) {
			camera = computeFitCamera(projectNodes, viewportWidth, viewportHeight);
			if (!focusedNodeId || !projectNodes.some((node) => node.id === focusedNodeId)) {
				focusedNodeId = projectNodes.at(-1)?.id ?? null;
			}
			persist();
			scheduleVisibilityRefresh(true);
			return;
		}
		const project = projectById.get(projectId);
		if (project) {
			spawnProjectNode(project);
			scheduleVisibilityRefresh(true);
			return;
		}
		persist();
		scheduleVisibilityRefresh(true);
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
			return projectStatusById.get(node.projectId) ?? 'ready';
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
		if (event.button !== 0) return;
		activeInteractionCleanup?.();
		const el = event.currentTarget as HTMLElement;
		const startRect = el.getBoundingClientRect();
		// Snapshot minimap bounds at pointerdown so panning the viewport does not
		// shift the coordinate system underneath the pointer during click/drag.
		const startMetrics = { ...minimapMetrics };
		let latestClientX = event.clientX;
		let latestClientY = event.clientY;
		let hasPendingMove = false;
		let frame = 0;
		const panTo = (clientX: number, clientY: number) => {
			const mx = clientX - startRect.left - 6;
			const my = clientY - startRect.top - 6;
			const worldX = startMetrics.minX + mx / startMetrics.scale;
			const worldY = startMetrics.minY + my / startMetrics.scale;
			camera = {
				...camera,
				x: Math.round(viewportWidth / 2 - worldX * camera.zoom),
				y: Math.round(viewportHeight / 2 - worldY * camera.zoom),
			};
		};
		panTo(latestClientX, latestClientY);
		const flush = () => {
			frame = 0;
			if (!hasPendingMove) return;
			hasPendingMove = false;
			panTo(latestClientX, latestClientY);
		};
		const move = (moveEvent: PointerEvent) => {
			latestClientX = moveEvent.clientX;
			latestClientY = moveEvent.clientY;
			hasPendingMove = true;
			if (!frame) frame = requestAnimationFrame(flush);
		};
		const teardown = (commit: boolean) => {
			if (frame) {
				cancelAnimationFrame(frame);
				frame = 0;
			}
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			if (activeInteractionCleanup === abort) activeInteractionCleanup = null;
			if (commit) {
				if (hasPendingMove) {
					hasPendingMove = false;
					panTo(latestClientX, latestClientY);
				}
				persist();
			}
		};
		const up = () => teardown(true);
		const abort = () => teardown(false);
		activeInteractionCleanup = abort;
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
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
	data-canvas-project-filter={activeProjectFilter ?? 'all'}
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
		<!-- Project spatial pills (selecting a project isolates and deactivates other projects) -->
		<div class="flex max-w-full items-center gap-1 overflow-x-auto rounded-xl border border-border bg-card/95 p-1 shadow-[var(--shadow-overlay)] backdrop-blur scrollbar-hide">
			<button
				type="button"
				onclick={() => jumpToProject(null)}
				aria-pressed={activeProjectFilter === null}
				title="Show all projects on the canvas"
				class="shrink-0 rounded-lg px-2.5 py-1 text-xs font-medium transition {activeProjectFilter === null ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
			>
				All Projects
			</button>
			{#each projects as project (project.id)}
				{@const count = projectNodeCounts.get(project.id) ?? 0}
				<button
					type="button"
					onclick={() => jumpToProject(project.id)}
					aria-pressed={activeProjectFilter === project.id}
					title="Show only {project.name} ({count} window{count === 1 ? '' : 's'}) and hide other projects"
					class="inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium transition {activeProjectFilter === project.id ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'}"
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

			{#if visibleNodes.length > 0}
				<button
					type="button"
					onclick={clearCanvas}
					class="rounded-lg p-1.5 text-muted-foreground hover:bg-destructive/15 hover:text-destructive"
					title={activeProject ? `Clear ${activeProject.name} windows` : 'Clear canvas'}
					aria-label={activeProject ? `Clear ${activeProject.name} windows` : 'Clear canvas'}
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
			{@const zoneDragging = activeZoneDrag?.projectId === zone.projectId}
			<div
				data-canvas-project-zone={zone.projectId}
				style="left:{zone.x}px;top:{zone.y}px;width:{zone.width}px;height:{zone.height}px;{zoneDragging && activeZoneDrag ? `transform:translate3d(${activeZoneDrag.dx}px,${activeZoneDrag.dy}px,0);` : ''}"
				class="pointer-events-none absolute rounded-2xl border border-dashed {zoneDragging ? 'border-primary/60 bg-primary/[0.06]' : 'border-primary/25 bg-primary/[0.03]'}"
			>
				<div
					role="button"
					tabindex="-1"
					data-canvas-zone-drag-handle={zone.projectId}
					aria-label="Drag {zone.name} group ({zone.nodeCount} windows)"
					title="Drag to move all {zone.nodeCount} windows in {zone.name}"
					onpointerdown={(event) => startZoneDrag(zone.projectId, event)}
					class="pointer-events-auto inline-flex cursor-grab select-none items-center gap-1.5 rounded-br-xl rounded-tl-2xl border-b border-r border-primary/20 bg-card/90 px-3 py-1 text-xs font-semibold text-foreground shadow-sm transition-colors hover:border-primary/50 hover:bg-card active:cursor-grabbing"
				>
					<GripVertical class="h-3.5 w-3.5 shrink-0 text-primary/70" aria-hidden="true" />
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
				{@const rawFrom = visibleNodeById.get(edge.fromId)}
				{@const rawTo = visibleNodeById.get(edge.toId)}
				{#if rawFrom && rawTo}
					{@const from = effectiveNodePosition(rawFrom)}
					{@const to = effectiveNodePosition(rawTo)}
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
			{#each renderedNodes as node (node.id)}
				{@const nodeProjId = resolveNodeProjectId(node)}
				{@const nodeHidden = activeProjectFilter !== null && nodeProjId !== activeProjectFilter}
				{@const nodeDragOffset = !nodeHidden && activeZoneDrag && nodeProjId === activeZoneDrag.projectId ? { x: activeZoneDrag.dx, y: activeZoneDrag.dy } : null}
				{@const nodeActive = !nodeHidden && (focusedNodeId === node.id || isNodeNearViewport(node))}
				{@const delegated = node.kind === 'task' && node.taskId ? (delegatedByParentTask.get(node.taskId)?.length ?? 0) : 0}
				<CanvasWindow
					{node}
					focused={focusedNodeId === node.id}
					{visible}
					active={nodeActive}
					hidden={nodeHidden}
					dragOffset={nodeDragOffset}
					zoom={camera.zoom}
					canvasInteracting={panning || spacePressed || activeZoneDrag !== null}
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
					onRetainChange={(retained) => setNodeRetained(node.id, retained)}
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
						<WorkspaceContent tab={workspaceTabFor(node.route)} compact={true} />
					{/if}
				</CanvasWindow>
			{/each}
		</div>
	</div>

	<!-- Empty canvas onboarding hint -->
	{#if initialized && visibleNodes.length === 0}
		<div
			role="region"
			aria-label="Empty canvas actions"
			data-canvas-empty
			onpointerdown={(event) => event.stopPropagation()}
			class="pointer-events-auto m-auto max-w-md rounded-2xl border border-dashed border-border bg-card/90 p-6 text-center shadow-lg backdrop-blur"
		>
			{#if activeProject}
				<h2 class="text-sm font-semibold text-foreground">No windows open in {activeProject.name}</h2>
				<p class="mt-1.5 text-xs text-muted-foreground">
					Other projects are hidden while filtering to {activeProject.name}. Open this project's hub or return to All Projects.
				</p>
				<div class="mt-4 flex flex-wrap justify-center gap-2">
					<button type="button" onclick={() => spawnProjectNode(activeProject)} class="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
						Open {activeProject.name} Hub
					</button>
					<button type="button" onclick={() => jumpToProject(null)} class="rounded-lg border border-border bg-card px-3 py-1.5 text-xs font-medium hover:bg-accent">
						Show All Projects
					</button>
				</div>
			{:else}
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
			{/if}
		</div>
	{/if}

	<!-- Contextual Link Preview Popover -->
	{#if linkPopover}
		{@const lp = linkPopover}
		{@const isExternal = lp.target.href.startsWith('http://') || lp.target.href.startsWith('https://')}
		{@const isDownload = !isExternal && (lp.target.downloadAttr !== null || lp.target.downloadPath !== null)}
		<div
			data-canvas-link-popover
			data-canvas-hud
			role="menu"
			tabindex="-1"
			aria-label="Link preview actions"
			style="left:{lp.x}px;top:{lp.y}px;"
			onpointerdown={(event) => event.stopPropagation()}
			class="pointer-events-auto absolute z-40 w-64 rounded-xl border border-border bg-card/98 p-1.5 shadow-2xl backdrop-blur"
		>
			<div class="border-b border-border/70 px-2 py-1">
				<div class="truncate text-xs font-medium text-foreground" title={lp.target.label}>{lp.target.label}</div>
				<div class="truncate font-mono text-[10px] text-muted-foreground" title={lp.workspaceRelativePath || lp.target.downloadPath || lp.target.href}>
					{lp.workspaceRelativePath || lp.target.downloadPath || lp.target.href}
				</div>
			</div>

			<div class="mt-1 space-y-0.5">
				{#if lp.target.category === 'image'}
					<button
						type="button"
						role="menuitem"
						onclick={() => void previewPopoverAsImage()}
						class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs font-medium text-foreground hover:bg-accent"
					>
						<ImageIcon class="h-3.5 w-3.5 shrink-0 text-violet-500" />
						<span>Preview in Canvas Image Viewer</span>
					</button>
					{#if isExternal}
						<button
							type="button"
							role="menuitem"
							onclick={() => void previewPopoverAsApp()}
							class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs text-foreground hover:bg-accent"
						>
							<Globe class="h-3.5 w-3.5 shrink-0 text-amber-500" />
							<span>Preview in Canvas App</span>
						</button>
					{/if}
				{:else if lp.target.category === 'html'}
					<button
						type="button"
						role="menuitem"
						onclick={() => void previewPopoverAsApp()}
						class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs font-medium text-foreground hover:bg-accent"
					>
						<Globe class="h-3.5 w-3.5 shrink-0 text-amber-500" />
						<span>Preview in Canvas App</span>
					</button>
					{#if lp.target.downloadAgentId && lp.target.downloadPath && (lp.resolvingWorkspace || lp.workspaceRelativePath)}
						<button
							type="button"
							role="menuitem"
							onclick={() => void previewPopoverInEditor()}
							class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs text-foreground hover:bg-accent"
						>
							<Code2 class="h-3.5 w-3.5 shrink-0 text-sky-500" />
							<span>Open in Canvas Editor</span>
						</button>
					{/if}
				{:else if lp.target.category === 'file'}
					<button
						type="button"
						role="menuitem"
						onclick={() => void previewPopoverInEditor()}
						class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs font-medium text-foreground hover:bg-accent"
					>
						<Code2 class="h-3.5 w-3.5 shrink-0 text-sky-500" />
						<span>Open in Canvas Editor</span>
					</button>
				{:else if lp.target.category === 'web'}
					<button
						type="button"
						role="menuitem"
						onclick={() => void previewPopoverAsApp()}
						class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs font-medium text-foreground hover:bg-accent"
					>
						<Globe class="h-3.5 w-3.5 shrink-0 text-amber-500" />
						<span>Preview in Canvas App</span>
					</button>
				{/if}

				<button
					type="button"
					role="menuitem"
					onclick={openPopoverNormally}
					class="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
				>
					{#if isDownload}
						<Download class="h-3.5 w-3.5 shrink-0" />
						<span>Download file</span>
					{:else}
						<ExternalLink class="h-3.5 w-3.5 shrink-0" />
						<span>{isExternal ? 'Open in Browser' : 'Open in New Tab'}</span>
					{/if}
				</button>
			</div>

			{#if lp.notice}
				<p role="status" class="mt-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-[10px] text-amber-600 dark:text-amber-400">
					{lp.notice}
				</p>
			{/if}
		</div>
	{/if}

	<!-- Bottom-left navigation hint -->
	<div
		data-canvas-hud
		class="pointer-events-none absolute bottom-3 left-3 z-30 hidden rounded-lg border border-border/80 bg-card/85 px-2.5 py-1 text-[10px] text-muted-foreground backdrop-blur md:block"
	>
		Drag background or Space+drag to pan · Ctrl/⌘+scroll or minimap scroll to zoom · {#if activeProjectFilter !== null}{visibleNodes.length} of {nodes.length} windows{#if activeProject} ({activeProject.name}){/if}{:else}{nodes.length} window{nodes.length === 1 ? '' : 's'}{/if}
	</div>

	<!-- Interactive Minimap -->
	{#if showMinimap && visibleNodes.length > 0}
		<div
			data-canvas-minimap
			data-canvas-hud
			role="presentation"
			onpointerdown={(event) => { event.stopPropagation(); minimapNavigate(event); }}
			onwheel={handleMinimapWheel}
			style="width:{minimapMetrics.mapW}px;height:{minimapMetrics.mapH}px;"
			class="pointer-events-auto absolute bottom-3 right-3 z-30 cursor-crosshair overflow-hidden rounded-xl border border-border bg-card/90 shadow-[var(--shadow-overlay)] backdrop-blur"
			title="Click or drag to pan · Scroll to zoom"
		>
			{#each visibleNodes as node (node.id)}
				{@const pos = effectiveNodePosition(node)}
				<div
					style="left:{6 + (pos.x - minimapMetrics.minX) * minimapMetrics.scale}px;top:{6 + (pos.y - minimapMetrics.minY) * minimapMetrics.scale}px;width:{Math.max(6, node.width * minimapMetrics.scale)}px;height:{Math.max(4, nodeVisualHeight(node) * minimapMetrics.scale)}px;"
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
