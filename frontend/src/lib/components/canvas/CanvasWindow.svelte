<script lang="ts">
	import type { Snippet } from 'svelte';
	import {
		Bot,
		ChevronDown,
		ChevronRight,
		Code2,
		Crosshair,
		ExternalLink,
		FolderKanban,
		GitBranch,
		Globe,
		Image as ImageIcon,
		MessageSquare,
		Plus,
		Sparkles,
		SquareTerminal,
		X,
	} from '@lucide/svelte';
	import { providePaneActive } from '$lib/liveUpdates';
	import { measurePane } from '$lib/workspaceLayout';
	import { CANVAS_COLLAPSED_HEIGHT, CANVAS_DEFAULT_SIZE, type CanvasNode, type CanvasNodeKind } from '$lib/canvas';

	interface Props {
		node: CanvasNode;
		focused: boolean;
		visible?: boolean;
		zoom: number;
		canvasInteracting: boolean;
		status?: string | null;
		contextLabel?: string | null;
		delegatedCount?: number;
		canSpawnTools?: boolean;
		hasRoute?: boolean;
		onFocus: () => void;
		onMove: (x: number, y: number, commit: boolean) => void;
		onResize: (width: number, height: number, commit: boolean) => void;
		onToggleCollapse: () => void;
		onCenter: () => void;
		onOpenInTabs?: () => void;
		onClose: () => void;
		onSpawnTool?: (kind: 'editor' | 'terminal' | 'image' | 'app') => void;
		onExpandDelegated?: () => void;
		children: Snippet;
	}

	let {
		node,
		focused,
		visible = true,
		zoom,
		canvasInteracting,
		status = null,
		contextLabel = null,
		delegatedCount = 0,
		canSpawnTools = false,
		hasRoute = false,
		onFocus,
		onMove,
		onResize,
		onToggleCollapse,
		onCenter,
		onOpenInTabs,
		onClose,
		onSpawnTool,
		onExpandDelegated,
		children,
	}: Props = $props();

	providePaneActive(() => visible && !node.collapsed);

	let dragging = $state(false);
	let resizing = $state(false);
	let spawnMenuOpen = $state(false);

	let shieldActive = $derived(dragging || resizing || canvasInteracting);
	let limits = $derived(CANVAS_DEFAULT_SIZE[node.kind]);

	function kindMeta(kind: CanvasNodeKind): { label: string; accent: string; badge: string } {
		switch (kind) {
			case 'project':
				return { label: 'Project', accent: 'bg-indigo-500', badge: 'bg-indigo-500/12 text-indigo-600 dark:text-indigo-300' };
			case 'task':
				return { label: 'Task', accent: 'bg-blue-500', badge: 'bg-blue-500/12 text-blue-600 dark:text-blue-300' };
			case 'conversation':
				return { label: 'Chat', accent: 'bg-teal-500', badge: 'bg-teal-500/12 text-teal-600 dark:text-teal-300' };
			case 'agent':
				return { label: 'Agent', accent: 'bg-cyan-500', badge: 'bg-cyan-500/12 text-cyan-600 dark:text-cyan-300' };
			case 'editor':
				return { label: 'Editor', accent: 'bg-sky-500', badge: 'bg-sky-500/12 text-sky-600 dark:text-sky-300' };
			case 'terminal':
				return { label: 'Terminal', accent: 'bg-emerald-500', badge: 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-300' };
			case 'image':
				return { label: 'Image', accent: 'bg-violet-500', badge: 'bg-violet-500/12 text-violet-600 dark:text-violet-300' };
			case 'app':
				return { label: 'App', accent: 'bg-amber-500', badge: 'bg-amber-500/12 text-amber-600 dark:text-amber-300' };
			case 'new-work':
				return { label: 'New work', accent: 'bg-primary', badge: 'bg-primary/12 text-primary' };
		}
	}

	function statusDot(value: string | null): string {
		if (!value) return '';
		if (['failed', 'error', 'blocked'].includes(value)) return 'bg-red-500';
		if (value === 'waiting_for_input') return 'bg-orange-500 animate-pulse';
		if (['running', 'in_progress', 'preparing', 'review'].includes(value)) return 'bg-blue-500 animate-pulse';
		if (['queued', 'pending', 'waiting_for_subtasks', 'waiting_for_dependencies'].includes(value)) return 'bg-amber-400';
		if (value === 'completed' || value === 'idle' || value === 'ready') return 'bg-emerald-500';
		return 'bg-muted-foreground';
	}

	function startDrag(event: PointerEvent) {
		if (event.button !== 0) return;
		const target = event.target as HTMLElement | null;
		if (target?.closest('button, a, input, select')) return;
		event.preventDefault();
		event.stopPropagation();
		onFocus();
		spawnMenuOpen = false;
		dragging = true;
		const startClientX = event.clientX;
		const startClientY = event.clientY;
		const originX = node.x;
		const originY = node.y;
		let latestX = originX;
		let latestY = originY;

		const move = (moveEvent: PointerEvent) => {
			latestX = originX + (moveEvent.clientX - startClientX) / zoom;
			latestY = originY + (moveEvent.clientY - startClientY) / zoom;
			onMove(latestX, latestY, false);
		};
		const up = () => {
			dragging = false;
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			onMove(latestX, latestY, true);
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
	}

	function startResize(axis: 'e' | 's' | 'se', event: PointerEvent) {
		if (event.button !== 0 || node.collapsed) return;
		event.preventDefault();
		event.stopPropagation();
		onFocus();
		resizing = true;
		const startClientX = event.clientX;
		const startClientY = event.clientY;
		const originW = node.width;
		const originH = node.height;
		let latestW = originW;
		let latestH = originH;

		const move = (moveEvent: PointerEvent) => {
			const dx = (moveEvent.clientX - startClientX) / zoom;
			const dy = (moveEvent.clientY - startClientY) / zoom;
			if (axis === 'e' || axis === 'se') {
				latestW = Math.max(limits.minWidth, Math.round(originW + dx));
			}
			if (axis === 's' || axis === 'se') {
				latestH = Math.max(limits.minHeight, Math.round(originH + dy));
			}
			onResize(latestW, latestH, false);
		};
		const up = () => {
			resizing = false;
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', up);
			onResize(latestW, latestH, true);
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', up);
	}

	let meta = $derived(kindMeta(node.kind));
</script>

<article
	data-canvas-node={node.id}
	data-canvas-node-kind={node.kind}
	data-canvas-node-focused={focused || undefined}
	data-canvas-node-collapsed={node.collapsed || undefined}
	style="left:{node.x}px;top:{node.y}px;width:{node.width}px;height:{node.collapsed ? CANVAS_COLLAPSED_HEIGHT : node.height}px;z-index:{node.zIndex};"
	class="canvas-window absolute flex flex-col overflow-hidden rounded-xl border bg-card transition-shadow {focused ? 'border-primary/60 shadow-[0_12px_36px_rgb(0_0_0/18%)] ring-1 ring-primary/35 dark:shadow-[0_14px_42px_rgb(0_0_0/48%)]' : 'border-border/90 shadow-[var(--shadow-overlay)] hover:border-border'}"
	onpointerdown={(event) => {
		event.stopPropagation();
		onFocus();
	}}
>
	<div class="h-0.5 w-full shrink-0 {meta.accent}"></div>

	<header
		role="toolbar"
		aria-label="{node.title} window controls"
		tabindex="-1"
		data-canvas-drag-handle
		onpointerdown={startDrag}
		ondblclick={onToggleCollapse}
		class="flex h-10 shrink-0 cursor-grab select-none items-center gap-1.5 border-b border-border/80 bg-[hsl(var(--inset))] px-2 active:cursor-grabbing"
	>
		<button
			type="button"
			onclick={onToggleCollapse}
			class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
			title={node.collapsed ? 'Expand window' : 'Collapse window'}
			aria-label={node.collapsed ? `Expand ${node.title}` : `Collapse ${node.title}`}
		>
			{#if node.collapsed}<ChevronRight class="h-3.5 w-3.5" />{:else}<ChevronDown class="h-3.5 w-3.5" />{/if}
		</button>

		<span class="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold {meta.badge}">
			{#if node.kind === 'project'}<FolderKanban class="h-3 w-3" />
			{:else if node.kind === 'task'}<GitBranch class="h-3 w-3" />
			{:else if node.kind === 'conversation'}<MessageSquare class="h-3 w-3" />
			{:else if node.kind === 'agent'}<Bot class="h-3 w-3" />
			{:else if node.kind === 'editor'}<Code2 class="h-3 w-3" />
			{:else if node.kind === 'terminal'}<SquareTerminal class="h-3 w-3" />
			{:else if node.kind === 'image'}<ImageIcon class="h-3 w-3" />
			{:else if node.kind === 'app'}<Globe class="h-3 w-3" />
			{:else}<Sparkles class="h-3 w-3" />
			{/if}
			<span>{meta.label}</span>
		</span>

		{#if status}
			<span class="h-2 w-2 shrink-0 rounded-full {statusDot(status)}" title={status.replaceAll('_', ' ')}></span>
		{/if}

		<div class="min-w-0 flex-1">
			<div class="truncate text-xs font-semibold text-foreground" title={node.title}>{node.title}</div>
			{#if contextLabel && !node.collapsed}
				<div class="truncate text-[9px] text-muted-foreground">{contextLabel}</div>
			{/if}
		</div>

		{#if delegatedCount > 0 && onExpandDelegated}
			<button
				type="button"
				onclick={onExpandDelegated}
				class="inline-flex shrink-0 items-center gap-1 rounded-full border border-blue-500/35 bg-blue-500/10 px-2 py-0.5 text-[10px] font-medium text-blue-600 hover:bg-blue-500/20 dark:text-blue-300"
				title="Open all delegated subtasks connected to this task"
				data-canvas-expand-delegated
			>
				<GitBranch class="h-3 w-3" />
				<span>{delegatedCount}</span>
			</button>
		{/if}

		{#if canSpawnTools && onSpawnTool}
			<div class="relative shrink-0">
				<button
					type="button"
					onclick={() => (spawnMenuOpen = !spawnMenuOpen)}
					class="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
					title="Open connected tool in context"
					aria-label="Open connected tool for {node.title}"
					data-canvas-node-spawn-trigger
				>
					<Plus class="h-3.5 w-3.5" />
				</button>
				{#if spawnMenuOpen}
					<div
						role="menu"
						tabindex="-1"
						class="absolute right-0 top-7 z-50 w-44 rounded-lg border border-border bg-card p-1 shadow-xl"
						onpointerdown={(event) => event.stopPropagation()}
					>
						<button type="button" onclick={() => { spawnMenuOpen = false; onSpawnTool('editor'); }} class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs hover:bg-accent">
							<Code2 class="h-3.5 w-3.5 text-sky-500" /><span>Open Code Editor</span>
						</button>
						<button type="button" onclick={() => { spawnMenuOpen = false; onSpawnTool('terminal'); }} class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs hover:bg-accent">
							<SquareTerminal class="h-3.5 w-3.5 text-emerald-500" /><span>Open Terminal</span>
						</button>
						<button type="button" onclick={() => { spawnMenuOpen = false; onSpawnTool('image'); }} class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs hover:bg-accent">
							<ImageIcon class="h-3.5 w-3.5 text-violet-500" /><span>Open Image Viewer</span>
						</button>
						<button type="button" onclick={() => { spawnMenuOpen = false; onSpawnTool('app'); }} class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs hover:bg-accent">
							<Globe class="h-3.5 w-3.5 text-amber-500" /><span>Open App Preview</span>
						</button>
					</div>
				{/if}
			</div>
		{/if}

		<button
			type="button"
			onclick={onCenter}
			class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
			title="Center view on window"
			aria-label="Center on {node.title}"
		>
			<Crosshair class="h-3.5 w-3.5" />
		</button>

		{#if hasRoute && onOpenInTabs}
			<button
				type="button"
				onclick={onOpenInTabs}
				class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
				title="Open in Tabs view"
				aria-label="Open {node.title} in Tabs view"
			>
				<ExternalLink class="h-3.5 w-3.5" />
			</button>
		{/if}

		<button
			type="button"
			onclick={onClose}
			class="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-destructive/15 hover:text-destructive"
			title="Remove from canvas"
			aria-label="Close {node.title}"
		>
			<X class="h-3.5 w-3.5" />
		</button>
	</header>

	<div
		use:measurePane
		style:display={node.collapsed ? 'none' : 'flex'}
		class="relative min-h-0 flex-1 flex-col overflow-hidden bg-background"
	>
		{@render children()}
		{#if shieldActive}
			<div class="absolute inset-0 z-40 cursor-grabbing bg-transparent" aria-hidden="true"></div>
		{/if}
	</div>

	{#if !node.collapsed}
		<div
			role="presentation"
			onpointerdown={(event) => startResize('e', event)}
			class="absolute bottom-3 right-0 top-10 z-30 w-1.5 cursor-ew-resize hover:bg-primary/30"
		></div>
		<div
			role="presentation"
			onpointerdown={(event) => startResize('s', event)}
			class="absolute bottom-0 left-3 right-3 z-30 h-1.5 cursor-ns-resize hover:bg-primary/30"
		></div>
		<div
			role="presentation"
			data-canvas-resize-handle
			onpointerdown={(event) => startResize('se', event)}
			class="absolute bottom-0 right-0 z-30 h-3.5 w-3.5 cursor-nwse-resize rounded-tl bg-transparent hover:bg-primary/35"
			title="Resize window"
		></div>
	{/if}
</article>
