<script lang="ts">
	import { onMount } from 'svelte';
	import { Folder, FolderOpen, Image as ImageIcon, RefreshCw, ZoomIn, ZoomOut, Grid2x2 } from '@lucide/svelte';
	import { workspaces, type Agent, type WorkspaceEntry } from '$lib/api';
	import { isImageFilePath } from '$lib/canvas';

	interface Props {
		agentId: string | null;
		initialPath: string | null;
		initialUrl: string | null;
		initialSource: 'workspace' | 'url' | null;
		agents: Agent[];
		onUpdateNode: (patch: {
			agentId?: string | null;
			filePath?: string | null;
			imageUrl?: string | null;
			imageSource?: 'workspace' | 'url' | null;
			title?: string;
		}) => void;
	}

	let {
		agentId,
		initialPath,
		initialUrl,
		initialSource,
		agents,
		onUpdateNode,
	}: Props = $props();

	let activeAgentId = $state('');
	let sourceMode = $state<'workspace' | 'url'>('workspace');
	let directories = $state<Record<string, WorkspaceEntry[]>>({});
	let expanded = $state<string[]>(['']);
	let showPicker = $state(true);
	let selectedPath = $state('');
	let urlInput = $state('');
	let activeUrl = $state('');
	let zoomPercent = $state<number | 'fit'>('fit');
	let checkerboard = $state(true);
	let reloadNonce = $state(0);
	let loadingTree = $state(false);
	let imageError = $state('');
	let naturalSize = $state<{ width: number; height: number } | null>(null);
	let treeSequence = 0;

	let visibleEntries = $derived(flattenEntries());
	let resolvedSrc = $derived((() => {
		if (sourceMode === 'url') {
			return activeUrl || '';
		}
		if (!activeAgentId || !selectedPath) return '';
		const base = workspaces.rawUrl(activeAgentId, selectedPath);
		return reloadNonce > 0 ? `${base}&_r=${reloadNonce}` : base;
	})());

	onMount(() => {
		activeAgentId = agentId || agents[0]?.id || '';
		sourceMode = initialSource ?? (initialUrl && !initialPath ? 'url' : 'workspace');
		if (initialPath) {
			selectedPath = initialPath;
			showPicker = false;
		}
		if (initialUrl) {
			urlInput = initialUrl;
			activeUrl = initialUrl;
			showPicker = false;
		}
		if (activeAgentId) {
			void loadRoot(activeAgentId);
		}
	});

	async function loadRoot(targetAgentId: string) {
		const req = ++treeSequence;
		loadingTree = true;
		try {
			const dir = await workspaces.tree(targetAgentId);
			if (req !== treeSequence || activeAgentId !== targetAgentId) return;
			directories = { '': dir.entries };
			expanded = [''];
		} catch (cause) {
			if (req === treeSequence && activeAgentId === targetAgentId) {
				imageError = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (req === treeSequence && activeAgentId === targetAgentId) {
				loadingTree = false;
			}
		}
	}

	function flattenEntries(): { entry: WorkspaceEntry; depth: number }[] {
		const rows: { entry: WorkspaceEntry; depth: number }[] = [];
		const visit = (dir: string, depth: number) => {
			for (const entry of directories[dir] ?? []) {
				if (entry.path === '.git') continue;
				if (entry.kind === 'directory' || (entry.kind === 'file' && isImageFilePath(entry.path))) {
					rows.push({ entry, depth });
					if (entry.kind === 'directory' && expanded.includes(entry.path)) {
						visit(entry.path, depth + 1);
					}
				}
			}
		};
		visit('', 0);
		return rows;
	}

	async function toggleDirectory(path: string) {
		const targetAgentId = activeAgentId;
		if (!targetAgentId) return;
		if (expanded.includes(path)) {
			expanded = expanded.filter((item) => item !== path);
			return;
		}
		expanded = [...expanded, path];
		if (directories[path]) return;
		const req = treeSequence;
		try {
			const dir = await workspaces.tree(targetAgentId, path);
			if (req !== treeSequence || activeAgentId !== targetAgentId) return;
			directories = { ...directories, [path]: dir.entries };
		} catch {}
	}

	function selectWorkspaceImage(path: string) {
		sourceMode = 'workspace';
		selectedPath = path;
		imageError = '';
		naturalSize = null;
		showPicker = false;
		onUpdateNode({
			agentId: activeAgentId || null,
			filePath: path,
			imageUrl: null,
			imageSource: 'workspace',
			title: path.split('/').pop() || 'Image',
		});
	}

	function applyImageUrl() {
		const trimmed = urlInput.trim();
		if (!trimmed) return;
		if (!trimmed.startsWith('http://') && !trimmed.startsWith('https://') && !trimmed.startsWith('/api/') && !trimmed.startsWith('data:image/')) {
			imageError = 'Enter an http(s) URL, XpressClaw attachment path, or image data URL.';
			return;
		}
		sourceMode = 'url';
		activeUrl = trimmed;
		imageError = '';
		naturalSize = null;
		showPicker = false;
		onUpdateNode({
			imageSource: 'url',
			imageUrl: trimmed,
			filePath: null,
			title: trimmed.split('/').pop()?.split('?')[0] || 'Image',
		});
	}

	function zoomIn() {
		zoomPercent = zoomPercent === 'fit' ? 125 : Math.min(400, zoomPercent + 25);
	}

	function zoomOut() {
		zoomPercent = zoomPercent === 'fit' ? 75 : Math.max(25, zoomPercent - 25);
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background text-xs" data-canvas-image-card>
	<div class="flex flex-wrap items-center gap-1.5 border-b border-border bg-[hsl(var(--inset))] px-2 py-1.5">
		<button
			type="button"
			onclick={() => (showPicker = !showPicker)}
			class="rounded border border-border bg-card px-2 py-0.5 text-[11px] hover:bg-accent"
		>
			{showPicker ? 'Hide picker' : 'Browse'}
		</button>
		<div class="inline-flex rounded border border-border bg-background p-0.5 text-[10px]">
			<button type="button" onclick={() => { sourceMode = 'workspace'; showPicker = true; }} class="rounded px-1.5 py-0.5 {sourceMode === 'workspace' ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground'}">Workspace</button>
			<button type="button" onclick={() => { sourceMode = 'url'; showPicker = true; }} class="rounded px-1.5 py-0.5 {sourceMode === 'url' ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground'}">URL</button>
		</div>
		<div class="flex-1"></div>
		{#if naturalSize}
			<span class="font-mono text-[10px] text-muted-foreground">{naturalSize.width}×{naturalSize.height}</span>
		{/if}
		<button type="button" onclick={zoomOut} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground" title="Zoom out" aria-label="Zoom out image"><ZoomOut class="h-3.5 w-3.5" /></button>
		<button type="button" onclick={() => (zoomPercent = zoomPercent === 'fit' ? 100 : 'fit')} class="rounded border border-border px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground">
			{zoomPercent === 'fit' ? 'Fit' : `${zoomPercent}%`}
		</button>
		<button type="button" onclick={zoomIn} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground" title="Zoom in" aria-label="Zoom in image"><ZoomIn class="h-3.5 w-3.5" /></button>
		<button type="button" onclick={() => (checkerboard = !checkerboard)} class="rounded p-1 {checkerboard ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-accent'}" title="Toggle transparency grid" aria-label="Toggle transparency grid"><Grid2x2 class="h-3.5 w-3.5" /></button>
		<button type="button" onclick={() => { reloadNonce += 1; imageError = ''; }} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground" title="Reload image" aria-label="Reload image"><RefreshCw class="h-3.5 w-3.5" /></button>
	</div>

	<div class="flex min-h-0 flex-1 overflow-hidden">
		{#if showPicker}
			<div class="workspace-scroll-y w-52 shrink-0 border-r border-border bg-[hsl(var(--inset))] p-2">
				{#if sourceMode === 'workspace'}
					<select
						aria-label="Image workspace agent"
						value={activeAgentId}
						onchange={(event) => {
							treeSequence += 1;
							loadingTree = false;
							activeAgentId = event.currentTarget.value;
							directories = {};
							expanded = [''];
							selectedPath = '';
							imageError = '';
							naturalSize = null;
							onUpdateNode({ agentId: activeAgentId || null, filePath: null });
							if (activeAgentId) void loadRoot(activeAgentId);
						}}
						class="mb-2 w-full rounded border border-input bg-background px-1.5 py-1 text-[11px]"
					>
						{#each agents as agent (agent.id)}
							<option value={agent.id}>{agent.title || agent.name}</option>
						{/each}
					</select>
					{#if loadingTree}
						<p class="p-1 text-[11px] text-muted-foreground">Scanning workspace…</p>
					{:else if visibleEntries.length === 0}
						<p class="p-1 text-[11px] text-muted-foreground">No image files found in root.</p>
					{:else}
						{#each visibleEntries as row (row.entry.path)}
							<button
								type="button"
								onclick={() => row.entry.kind === 'directory' ? void toggleDirectory(row.entry.path) : selectWorkspaceImage(row.entry.path)}
								style="padding-left: {row.depth * 10 + 6}px"
								class="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] hover:bg-accent {selectedPath === row.entry.path ? 'bg-accent font-medium text-accent-foreground' : 'text-foreground'}"
								title={row.entry.path}
							>
								{#if row.entry.kind === 'directory'}
									{#if expanded.includes(row.entry.path)}<FolderOpen class="h-3.5 w-3.5 shrink-0 text-amber-500" />{:else}<Folder class="h-3.5 w-3.5 shrink-0 text-amber-500" />{/if}
								{:else}
									<ImageIcon class="h-3.5 w-3.5 shrink-0 text-violet-500" />
								{/if}
								<span class="min-w-0 flex-1 truncate">{row.entry.name}</span>
							</button>
						{/each}
					{/if}
				{:else}
					<form class="space-y-2" onsubmit={(event) => { event.preventDefault(); applyImageUrl(); }}>
						<label class="block text-[11px] text-muted-foreground">
							Image URL or attachment path
							<input
								aria-label="Image URL"
								bind:value={urlInput}
								placeholder="https://… or /api/tasks/…"
								class="mt-1 w-full rounded border border-input bg-background px-2 py-1 font-mono text-[11px] text-foreground"
							/>
						</label>
						<button class="w-full rounded bg-primary px-2 py-1 text-[11px] font-medium text-primary-foreground hover:bg-primary/90">Load image</button>
					</form>
				{/if}
			</div>
		{/if}

		<div
			class="relative flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-auto p-3"
			style={checkerboard ? 'background-image: linear-gradient(45deg, hsl(var(--muted)) 25%, transparent 25%), linear-gradient(-45deg, hsl(var(--muted)) 25%, transparent 25%), linear-gradient(45deg, transparent 75%, hsl(var(--muted)) 75%), linear-gradient(-45deg, transparent 75%, hsl(var(--muted)) 75%); background-size: 16px 16px; background-position: 0 0, 0 8px, 8px -8px, -8px 0px;' : ''}
		>
			{#if imageError}
				<div role="alert" class="rounded-lg border border-destructive/30 bg-card px-3 py-2 text-xs text-destructive">{imageError}</div>
			{:else if resolvedSrc}
				<img
					src={resolvedSrc}
					alt={selectedPath || 'Canvas image preview'}
					onload={(event) => {
						const img = event.currentTarget as HTMLImageElement;
						naturalSize = { width: img.naturalWidth, height: img.naturalHeight };
						imageError = '';
					}}
					onerror={() => { imageError = 'Could not load image preview.'; }}
					style={zoomPercent === 'fit'
						? 'max-width: 100%; max-height: 100%; object-fit: contain;'
						: naturalSize
							? `width: ${Math.round(naturalSize.width * (zoomPercent / 100))}px; max-width: none;`
							: `width: ${zoomPercent}%; max-width: none;`}
					class="rounded shadow-sm"
				/>
			{:else}
				<div class="rounded-xl border border-dashed border-border bg-card/85 px-4 py-6 text-center text-muted-foreground">
					Pick an image from the Agent's workspace or paste an image URL to view it in spatial context.
				</div>
			{/if}
		</div>
	</div>
</div>
