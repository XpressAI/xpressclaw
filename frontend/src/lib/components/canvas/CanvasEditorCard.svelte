<script lang="ts">
	import { onMount } from 'svelte';
	import { Folder, FolderOpen, FileCode2, Image as ImageIcon, PanelLeft, RefreshCw, Save, Globe } from '@lucide/svelte';
	import { workspaces, type Agent, type GitChange, type WorkspaceEntry, type WorkspaceFile, type WorkspaceGitDiff, type WorkspaceGitStatus } from '$lib/api';
	import { isHtmlFilePath, isImageFilePath } from '$lib/canvas';
	import MonacoEditor from '$lib/components/MonacoEditor.svelte';

	interface Props {
		nodeId: string;
		agentId: string | null;
		initialPath: string | null;
		agents: Agent[];
		onUpdateNode: (patch: { agentId?: string | null; filePath?: string | null; title?: string }) => void;
		onOpenImage: (agentId: string, filePath: string, parentNodeId: string) => void;
		onOpenHtmlApp: (agentId: string, filePath: string, parentNodeId: string) => void;
	}

	let {
		nodeId,
		agentId,
		initialPath,
		agents,
		onUpdateNode,
		onOpenImage,
		onOpenHtmlApp,
	}: Props = $props();

	let activeAgentId = $state('');
	let directories = $state<Record<string, WorkspaceEntry[]>>({});
	let expanded = $state<string[]>(['']);
	let git = $state<WorkspaceGitStatus | null>(null);
	let selectedPath = $state('');
	let selectedFile = $state<WorkspaceFile | null>(null);
	let editorValue = $state('');
	let fileDiff = $state<WorkspaceGitDiff | null>(null);
	let viewMode = $state<'code' | 'diff'>('code');
	let showTree = $state(true);
	let loadingTree = $state(false);
	let loadingFile = $state(false);
	let saving = $state(false);
	let error = $state('');
	let saveMessage = $state('');
	let pathInput = $state('');
	let sequence = 0;
	let treeSequence = 0;

	let dirty = $derived(Boolean(selectedFile && editorValue !== selectedFile.content));
	let changeByPath = $derived(new Map((git?.files ?? []).map((change) => [change.path, change])));
	let visibleEntries = $derived(flattenEntries());

	onMount(() => {
		activeAgentId = agentId || agents[0]?.id || '';
		if (initialPath) {
			pathInput = initialPath;
		}
		if (activeAgentId) {
			void loadAgentWorkspace(activeAgentId, initialPath);
		}
	});

	async function loadAgentWorkspace(targetAgentId: string, fileToOpen: string | null = null) {
		if (!targetAgentId) return;
		const req = ++treeSequence;
		loadingTree = true;
		error = '';
		try {
			const [rootDir, gitStatus] = await Promise.all([
				workspaces.tree(targetAgentId),
				workspaces.gitStatus(targetAgentId).catch(() => null),
			]);
			if (req !== treeSequence || activeAgentId !== targetAgentId) return;
			directories = { '': rootDir.entries };
			expanded = [''];
			git = gitStatus;
			if (fileToOpen) {
				await openFile(fileToOpen, true);
			}
		} catch (cause) {
			if (req === treeSequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
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
				rows.push({ entry, depth });
				if (entry.kind === 'directory' && expanded.includes(entry.path)) {
					visit(entry.path, depth + 1);
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
		} catch (cause) {
			if (req === treeSequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
			}
		}
	}

	async function openFile(path: string, force = false) {
		const cleanPath = path.trim();
		const targetAgentId = activeAgentId;
		if (!targetAgentId || !cleanPath) return;
		if (!force && dirty && !window.confirm('Discard unsaved changes in this editor?')) return;
		if (isImageFilePath(cleanPath)) {
			onOpenImage(targetAgentId, cleanPath, nodeId);
			return;
		}
		const req = ++sequence;
		loadingFile = true;
		error = '';
		saveMessage = '';
		try {
			const [file, diff] = await Promise.all([
				workspaces.readFile(targetAgentId, cleanPath),
				workspaces.gitDiff(targetAgentId, cleanPath).catch(() => null),
			]);
			if (req !== sequence || activeAgentId !== targetAgentId) return;
			selectedPath = file.path;
			pathInput = file.path;
			selectedFile = file;
			editorValue = file.content;
			fileDiff = diff;
			viewMode = 'code';
			onUpdateNode({
				agentId: targetAgentId,
				filePath: file.path,
				title: file.path.split('/').pop() || file.path,
			});
		} catch (cause) {
			if (req === sequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (req === sequence && activeAgentId === targetAgentId) {
				loadingFile = false;
			}
		}
	}

	async function saveFile() {
		const targetAgentId = activeAgentId;
		if (!targetAgentId || !selectedFile || !dirty || saving) return;
		const snapshot = selectedFile;
		const content = editorValue;
		const req = sequence;
		saving = true;
		error = '';
		saveMessage = '';
		try {
			const saved = await workspaces.saveFile(targetAgentId, { ...snapshot, content });
			if (req !== sequence || activeAgentId !== targetAgentId) return;
			if (selectedFile?.path === snapshot.path) {
				selectedFile = { ...selectedFile, content, revision: saved.revision, size: saved.size };
				saveMessage = 'Saved';
				setTimeout(() => { saveMessage = ''; }, 2000);
			}
			const nextGit = await workspaces.gitStatus(targetAgentId).catch(() => git);
			const nextDiff = await workspaces.gitDiff(targetAgentId, snapshot.path).catch(() => fileDiff);
			if (req === sequence && activeAgentId === targetAgentId) {
				git = nextGit;
				fileDiff = nextDiff;
			}
		} catch (cause) {
			if (req === sequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (req === sequence && activeAgentId === targetAgentId) {
				saving = false;
			}
		}
	}

	function handleAgentChange(nextId: string) {
		if (dirty && !window.confirm('Discard unsaved changes before switching agents?')) return;
		sequence += 1;
		treeSequence += 1;
		loadingFile = false;
		loadingTree = false;
		saving = false;
		activeAgentId = nextId;
		directories = {};
		expanded = [''];
		git = null;
		selectedPath = '';
		selectedFile = null;
		editorValue = '';
		pathInput = '';
		fileDiff = null;
		error = '';
		saveMessage = '';
		onUpdateNode({ agentId: nextId || null, filePath: null });
		if (nextId) void loadAgentWorkspace(nextId, null);
	}

	function statusCode(change: GitChange): string {
		if (change.status === '??') return 'U';
		if (change.status.includes('A')) return 'A';
		if (change.status.includes('D')) return 'D';
		return 'M';
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background text-xs" data-canvas-editor-card>
	<div class="flex flex-wrap items-center gap-1.5 border-b border-border bg-[hsl(var(--inset))] px-2 py-1.5">
		<button
			type="button"
			onclick={() => (showTree = !showTree)}
			class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
			title={showTree ? 'Hide file tree' : 'Show file tree'}
			aria-label={showTree ? 'Hide file tree' : 'Show file tree'}
		>
			<PanelLeft class="h-3.5 w-3.5" />
		</button>
		<select
			aria-label="Editor agent"
			value={activeAgentId}
			onchange={(event) => handleAgentChange(event.currentTarget.value)}
			class="max-w-36 rounded border border-input bg-background px-1.5 py-1 text-[11px]"
		>
			{#if agents.length === 0}<option value="">No agents</option>{/if}
			{#each agents as agent (agent.id)}
				<option value={agent.id}>{agent.title || agent.name}</option>
			{/each}
		</select>
		<form
			class="flex min-w-32 flex-1 items-center gap-1"
			onsubmit={(event) => { event.preventDefault(); void openFile(pathInput); }}
		>
			<input
				aria-label="Workspace file path"
				bind:value={pathInput}
				placeholder="src/main.rs"
				class="min-w-0 flex-1 rounded border border-input bg-background px-2 py-1 font-mono text-[11px]"
			/>
			<button class="rounded border border-border bg-card px-2 py-1 text-[11px] hover:bg-accent">Open</button>
		</form>
		{#if selectedFile && isHtmlFilePath(selectedFile.path) && activeAgentId}
			<button
				type="button"
				onclick={() => onOpenHtmlApp(activeAgentId, selectedFile!.path, nodeId)}
				class="inline-flex items-center gap-1 rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] font-medium text-amber-600 hover:bg-amber-500/15 dark:text-amber-400"
				title="Preview this HTML file as a live App window"
			>
				<Globe class="h-3 w-3" />
				<span>Preview App</span>
			</button>
		{/if}
		{#if selectedFile}
			<div class="inline-flex rounded border border-border bg-background p-0.5 text-[10px]">
				<button type="button" onclick={() => (viewMode = 'code')} class="rounded px-1.5 py-0.5 {viewMode === 'code' ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground'}">Code</button>
				<button type="button" onclick={() => (viewMode = 'diff')} class="rounded px-1.5 py-0.5 {viewMode === 'diff' ? 'bg-accent text-accent-foreground font-medium' : 'text-muted-foreground'}">Diff</button>
			</div>
			<button
				type="button"
				onclick={saveFile}
				disabled={!dirty || saving}
				class="inline-flex items-center gap-1 rounded bg-primary px-2 py-1 text-[11px] font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-40"
			>
				<Save class="h-3 w-3" />
				<span>{saving ? 'Saving…' : dirty ? 'Save •' : saveMessage || 'Saved'}</span>
			</button>
		{/if}
		<button
			type="button"
			onclick={() => activeAgentId && void loadAgentWorkspace(activeAgentId, selectedPath || null)}
			class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
			title="Refresh workspace"
			aria-label="Refresh workspace"
		>
			<RefreshCw class="h-3.5 w-3.5" />
		</button>
	</div>

	{#if error}
		<div role="alert" class="border-b border-destructive/30 bg-destructive/10 px-3 py-1.5 text-[11px] text-destructive">{error}</div>
	{/if}

	<div class="flex min-h-0 flex-1 overflow-hidden">
		{#if showTree}
			<div class="workspace-scroll-y w-48 shrink-0 border-r border-border bg-[hsl(var(--inset))] p-1.5">
				{#if loadingTree}
					<p class="p-2 text-[11px] text-muted-foreground">Loading files…</p>
				{:else if visibleEntries.length === 0}
					<p class="p-2 text-[11px] text-muted-foreground">No files found.</p>
				{:else}
					{#each visibleEntries as row (row.entry.path)}
						{@const change = changeByPath.get(row.entry.path)}
						{@const isImg = row.entry.kind === 'file' && isImageFilePath(row.entry.path)}
						<button
							type="button"
							onclick={() => row.entry.kind === 'directory' ? void toggleDirectory(row.entry.path) : void openFile(row.entry.path)}
							style="padding-left: {row.depth * 12 + 6}px"
							class="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] hover:bg-accent {selectedPath === row.entry.path ? 'bg-accent font-medium text-accent-foreground' : 'text-foreground'}"
							title={row.entry.path}
						>
							{#if row.entry.kind === 'directory'}
								{#if expanded.includes(row.entry.path)}<FolderOpen class="h-3.5 w-3.5 shrink-0 text-amber-500" />{:else}<Folder class="h-3.5 w-3.5 shrink-0 text-amber-500" />{/if}
							{:else if isImg}
								<ImageIcon class="h-3.5 w-3.5 shrink-0 text-violet-500" />
							{:else}
								<FileCode2 class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
							{/if}
							<span class="min-w-0 flex-1 truncate">{row.entry.name}</span>
							{#if change}<span class="font-mono text-[9px] font-semibold text-amber-500">{statusCode(change)}</span>{/if}
						</button>
					{/each}
				{/if}
			</div>
		{/if}

		<div class="flex min-h-0 min-w-0 flex-1 flex-col">
			{#if loadingFile}
				<div class="m-auto text-muted-foreground">Loading file…</div>
			{:else if selectedFile}
				{#if viewMode === 'diff'}
					<pre class="workspace-scroll-y m-0 flex-1 bg-[hsl(var(--code-background))] p-3 font-mono text-[11px] text-[hsl(var(--code-foreground))]">{fileDiff?.diff || 'No git diff for this file.'}</pre>
				{:else}
					<div class="min-h-0 flex-1">
						{#key `${nodeId}:${activeAgentId}:${selectedFile.path}`}
							<MonacoEditor
								value={editorValue}
								path={selectedFile.path}
								modelId={`${nodeId}:${activeAgentId}`}
								onChange={(value) => { editorValue = value; }}
								onSave={saveFile}
							/>
						{/key}
					</div>
				{/if}
			{:else}
				<div class="m-auto max-w-xs p-6 text-center text-muted-foreground">
					Pick a file from the workspace tree or enter a path above to edit in context. Clicking an image opens a connected Image Viewer window.
				</div>
			{/if}
		</div>
	</div>
</div>
