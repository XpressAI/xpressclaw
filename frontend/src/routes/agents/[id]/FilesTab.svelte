<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { workspaces } from '$lib/api';
	import type { GitChange, WorkspaceEntry, WorkspaceFile, WorkspaceGitDiff, WorkspaceGitStatus, WorkspaceStatus } from '$lib/api';
	import MonacoEditor from '$lib/components/MonacoEditor.svelte';
	import TerminalPanel from '$lib/components/TerminalPanel.svelte';

	let { agentId, route = '' }: { agentId: string; route?: string } = $props();
	let status = $state<WorkspaceStatus | null>(null);
	let git = $state<WorkspaceGitStatus | null>(null);
	let directories = $state<Record<string, WorkspaceEntry[]>>({});
	let expanded = $state<string[]>(['']);
	let selectedPath = $state('');
	let selectedFile = $state<WorkspaceFile | null>(null);
	let editorValue = $state('');
	let fileDiff = $state<WorkspaceGitDiff | null>(null);
	let viewMode = $state<'code' | 'diff'>('code');
	let loading = $state(true);
	let loadingFile = $state(false);
	let saving = $state(false);
	let showTerminal = $state(false);
	let showTree = $state(true);
	let initialized = $state(false);
	let syncedRoute = '';
	let fileOpenSequence = 0;
	let error = $state('');
	let saveMessage = $state('');
	type FileOpenResult = 'opened' | 'cancelled' | 'stale' | 'failed';

	let visibleEntries = $derived(flattenEntries());
	let dirty = $derived(Boolean(selectedFile && editorValue !== selectedFile.content));
	let changeByPath = $derived(new Map((git?.files ?? []).map((change) => [change.path, change])));

	onMount(() => {
		showTree = routeState(route).showTree;
		void initialize().finally(() => (initialized = true));
	});

	$effect(() => {
		const requestedRoute = route;
		if (!initialized || requestedRoute === syncedRoute) return;
		syncedRoute = requestedRoute;
		void applyRoute(requestedRoute);
	});

	function routeState(value: string): { path: string; showTree: boolean } {
		const search = value.includes('?') ? value.slice(value.indexOf('?')) : window.location.search;
		const params = new URLSearchParams(search);
		return {
			path: params.get('path') ?? '',
			showTree: params.get('tree') !== 'collapsed',
		};
	}

	function routeForFileState(value: string, path: string, treeVisible: boolean): string {
		const url = new URL(value || window.location.href, window.location.origin);
		if (path) url.searchParams.set('path', path);
		else url.searchParams.delete('path');
		if (treeVisible) url.searchParams.delete('tree');
		else url.searchParams.set('tree', 'collapsed');
		return `${url.pathname}${url.search}${url.hash}`;
	}

	async function applyRoute(requestedRoute: string) {
		const previousPath = selectedPath;
		const previousShowTree = showTree;
		const requested = routeState(requestedRoute);
		showTree = requested.showTree;
		if (requested.path === selectedPath) {
			fileOpenSequence += 1;
			loadingFile = false;
			return;
		}

		const result = requested.path
			? await openFile(requested.path, false, false)
			: clearFileSelection();
		if (result === 'opened' || result === 'stale' || route !== requestedRoute) return;

		showTree = previousShowTree;
		const restoredRoute = routeForFileState(requestedRoute, previousPath, previousShowTree);
		syncedRoute = restoredRoute;
		await goto(restoredRoute, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function clearFileSelection(): FileOpenResult {
		fileOpenSequence += 1;
		loadingFile = false;
		if (dirty && !window.confirm('Discard the unsaved changes in the current file?')) return 'cancelled';
		selectedPath = '';
		selectedFile = null;
		editorValue = '';
		fileDiff = null;
		viewMode = 'code';
		error = '';
		saveMessage = '';
		return 'opened';
	}

	async function initialize() {
		loading = true;
		error = '';
		try {
			const [workspaceStatus, rootDirectory, gitStatus] = await Promise.all([
				workspaces.status(agentId),
				workspaces.tree(agentId),
				workspaces.gitStatus(agentId).catch(() => null),
			]);
			status = workspaceStatus;
			directories = { '': rootDirectory.entries };
			git = gitStatus;
			const initialPath = routeState(route).path;
			if (initialPath) await openFile(initialPath, true, false);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loading = false;
		}
	}

	function flattenEntries(): { entry: WorkspaceEntry; depth: number }[] {
		const rows: { entry: WorkspaceEntry; depth: number }[] = [];
		const visit = (directory: string, depth: number) => {
			for (const entry of directories[directory] ?? []) {
				if (entry.path === '.git') continue;
				rows.push({ entry, depth });
				if (entry.kind === 'directory' && expanded.includes(entry.path)) visit(entry.path, depth + 1);
			}
		};
		visit('', 0);
		return rows;
	}

	async function toggleDirectory(path: string) {
		if (expanded.includes(path)) {
			expanded = expanded.filter((candidate) => candidate !== path);
			return;
		}
		expanded = [...expanded, path];
		if (directories[path]) return;
		try {
			const directory = await workspaces.tree(agentId, path);
			directories = { ...directories, [path]: directory.entries };
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	}

	async function openFile(path: string, force = false, navigate = true): Promise<FileOpenResult> {
		const requestSequence = ++fileOpenSequence;
		if (!force && dirty && !window.confirm('Discard the unsaved changes in the current file?')) {
			loadingFile = false;
			return 'cancelled';
		}
		loadingFile = true;
		error = '';
		saveMessage = '';
		try {
			const [fileResult, diff] = await Promise.all([
				workspaces.readFile(agentId, path)
					.then((file) => ({ file, error: null }))
					.catch((cause: unknown) => ({ file: null, error: cause })),
				workspaces.gitDiff(agentId, path).catch(() => null),
			]);
			if (requestSequence !== fileOpenSequence) return 'stale';
			const deleted = changeByPath.get(path)?.status.includes('D') ?? false;
			if (!fileResult.file && !(deleted && diff)) throw fileResult.error;
			selectedPath = path;
			selectedFile = fileResult.file;
			editorValue = fileResult.file?.content ?? '';
			fileDiff = diff;
			viewMode = fileResult.file ? 'code' : 'diff';
			if (navigate) {
				const url = new URL(window.location.href);
				url.searchParams.set('tab', 'files');
				url.searchParams.set('path', path);
				await goto(`${url.pathname}${url.search}`, { replaceState: true, keepFocus: true, noScroll: true });
			}
			return 'opened';
		} catch (cause) {
			if (requestSequence !== fileOpenSequence) return 'stale';
			error = cause instanceof Error ? cause.message : String(cause);
			return 'failed';
		} finally {
			if (requestSequence === fileOpenSequence) loadingFile = false;
		}
	}

	async function saveFile() {
		if (!selectedFile || !dirty || saving) return;
		const fileAtStart = selectedFile;
		const submittedContent = editorValue;
		saving = true;
		error = '';
		saveMessage = '';
		try {
			const saved = await workspaces.saveFile(agentId, { ...fileAtStart, content: submittedContent });
			if (selectedPath === fileAtStart.path && selectedFile?.path === fileAtStart.path) {
				selectedFile = {
					...selectedFile,
					content: submittedContent,
					revision: saved.revision,
					size: saved.size,
				};
				saveMessage = 'Saved';
			}
			await refreshGit();
			const refreshedDiff = await workspaces.gitDiff(agentId, fileAtStart.path).catch(() => null);
			if (selectedPath === fileAtStart.path && refreshedDiff) fileDiff = refreshedDiff;
			setTimeout(() => (saveMessage = ''), 2000);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			saving = false;
		}
	}

	async function refreshGit() {
		git = await workspaces.gitStatus(agentId).catch(() => git);
	}

	async function toggleTree() {
		showTree = !showTree;
		const currentRoute = route || window.location.href;
		const requestedPath = routeState(currentRoute).path;
		await goto(routeForFileState(currentRoute, requestedPath, showTree), { replaceState: true, keepFocus: true, noScroll: true });
	}

	function statusLabel(change: GitChange): string {
		if (change.status === '??') return 'U';
		if (change.status.includes('R')) return 'R';
		if (change.status.includes('A')) return 'A';
		if (change.status.includes('D')) return 'D';
		if (change.status.includes('C')) return 'C';
		return 'M';
	}

	function basename(path: string): string {
		return path.split('/').pop() || path;
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background" data-workspace-files>
	<div class="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-border px-3 py-2">
		<div class="min-w-0">
			<div class="truncate text-xs font-medium">{status?.root ?? 'Workspace'}</div>
			<div class="text-[11px] text-muted-foreground">
				{#if git?.repository}{git.branch || 'detached HEAD'} · {git.files.length} changed{:else}{status?.repository.message ?? 'No active Git repository'}{/if}
			</div>
			{#if status?.repository.github_status === 'attached'}<div class="mt-0.5 text-[10px] text-emerald-600 dark:text-emerald-400">GitHub MCP · {status.repository.github_repository}</div>{/if}
		</div>
		<div class="flex items-center gap-2">
			<button type="button" onclick={toggleTree} aria-expanded={showTree} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent">
				{showTree ? 'Hide files' : 'Show files'}
			</button>
			<button type="button" onclick={refreshGit} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent">Refresh</button>
			<button
				type="button"
				onclick={() => (showTerminal = !showTerminal)}
				class="rounded-md border px-2.5 py-1.5 text-xs {showTerminal ? 'border-primary bg-primary/10 text-primary' : 'border-border hover:bg-accent'}"
			>
				{showTerminal ? 'Hide terminal' : 'Terminal'}
			</button>
		</div>
	</div>

	{#if error}
		<div class="shrink-0 border-b border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
			{error}
			{#if selectedPath}<button type="button" onclick={() => openFile(selectedPath, true, false)} class="ml-2 underline">Reload file</button>{/if}
		</div>
	{/if}

	{#if loading}
		<div class="flex min-h-0 flex-1 items-center justify-center text-sm text-muted-foreground">Loading workspace…</div>
	{:else}
		<div class="flex min-h-0 flex-1 flex-col md:flex-row">
			{#if showTree}
			<div class="flex max-h-60 w-full shrink-0 flex-col border-b border-border md:max-h-none md:w-64 md:border-b-0 md:border-r">
				{#if git?.files.length}
					<div class="shrink-0 border-b border-border">
						<div class="px-3 py-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Changes</div>
						<div class="max-h-32 overflow-y-auto pb-1">
							{#each git.files as change (change.path)}
								<button type="button" onclick={() => openFile(change.path)} title={change.path}
									class="flex w-full items-center gap-2 px-3 py-1 text-left text-xs hover:bg-accent {selectedPath === change.path ? 'bg-accent' : ''}">
									<span class="w-3 shrink-0 font-mono text-[10px] text-amber-500">{statusLabel(change)}</span>
									<span class="truncate">{change.path}</span>
								</button>
							{/each}
						</div>
					</div>
				{/if}
				<div class="shrink-0 px-3 py-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Files</div>
				<div class="min-h-0 flex-1 overflow-y-auto pb-2" data-workspace-tree>
					{#each visibleEntries as row (row.entry.path)}
						{@const change = changeByPath.get(row.entry.path)}
						<button
							type="button"
							onclick={() => row.entry.kind === 'directory' ? toggleDirectory(row.entry.path) : row.entry.kind === 'file' ? openFile(row.entry.path) : undefined}
							disabled={row.entry.kind === 'other' || row.entry.kind === 'symlink'}
							title={row.entry.path}
							class="flex w-full items-center gap-1.5 py-1 pr-2 text-left text-xs hover:bg-accent disabled:cursor-default disabled:opacity-60 {selectedPath === row.entry.path ? 'bg-accent text-foreground' : 'text-muted-foreground'}"
							style:padding-left={`${8 + row.depth * 14}px`}
						>
							{#if row.entry.kind === 'directory'}
								<span class="w-3 shrink-0 text-[10px]">{expanded.includes(row.entry.path) ? '▾' : '▸'}</span><span>📁</span>
							{:else}
								<span class="w-3 shrink-0"></span><span>▧</span>
							{/if}
							<span class="min-w-0 flex-1 truncate">{row.entry.name}</span>
							{#if change}<span class="font-mono text-[9px] text-amber-500">{statusLabel(change)}</span>{/if}
						</button>
					{/each}
				</div>
			</div>
			{/if}

			<div class="flex min-h-[20rem] min-w-0 flex-1 flex-col">
				{#if selectedPath}
					<div class="flex h-9 shrink-0 items-center justify-between gap-2 border-b border-border px-3">
						<div class="flex min-w-0 items-center gap-2">
							<span class="truncate text-xs" title={selectedPath}>{basename(selectedPath)}</span>
							{#if dirty}<span class="h-1.5 w-1.5 rounded-full bg-amber-500" title="Unsaved changes"></span>{/if}
						</div>
						<div class="flex items-center gap-1.5">
							<div class="flex rounded-md border border-border p-0.5">
								<button type="button" onclick={() => (viewMode = 'code')} disabled={!selectedFile} class="rounded px-2 py-0.5 text-[10px] disabled:opacity-40 {viewMode === 'code' ? 'bg-accent text-foreground' : 'text-muted-foreground'}">Code</button>
								<button type="button" onclick={() => (viewMode = 'diff')} class="rounded px-2 py-0.5 text-[10px] {viewMode === 'diff' ? 'bg-accent text-foreground' : 'text-muted-foreground'}">Diff</button>
							</div>
							{#if saveMessage}<span class="text-[10px] text-emerald-500">{saveMessage}</span>{/if}
							<button type="button" onclick={saveFile} disabled={!dirty || saving}
								class="rounded-md bg-primary px-2.5 py-1 text-[11px] font-medium text-primary-foreground disabled:opacity-40">
								{saving ? 'Saving…' : 'Save'}
							</button>
						</div>
					</div>
					<div class="min-h-0 flex-1">
						{#if loadingFile}
							<div class="flex h-full items-center justify-center text-xs text-muted-foreground">Loading file…</div>
						{:else if viewMode === 'code' && selectedFile}
							{#key selectedPath}
								<MonacoEditor value={editorValue} path={selectedPath} onChange={(value) => (editorValue = value)} onSave={saveFile} />
							{/key}
						{:else}
							<MonacoEditor value={fileDiff?.diff || 'No tracked diff is available for this file.'} path={`${selectedPath}.diff`} language="diff" readOnly />
						{/if}
					</div>
				{:else}
					<div class="flex h-full flex-col items-center justify-center px-6 text-center text-sm text-muted-foreground">
						<div class="text-3xl opacity-40">⌘</div>
						<p class="mt-2">Choose a file to browse or edit it with Monaco.</p>
						<p class="mt-1 text-xs">Changed files are collected from the active repository's current Git status.</p>
					</div>
				{/if}
			</div>
		</div>
	{/if}

	{#if showTerminal}
		<div class="h-56 shrink-0 border-t border-border">
			{#if status?.terminal_available}
				<TerminalPanel {agentId} />
			{:else}
				<div class="flex h-full flex-col items-center justify-center px-6 text-center text-sm text-muted-foreground">
					<p>The retained container has not been initialized yet.</p>
					<p class="mt-1 text-xs">Run one task for this agent, then reconnect the terminal here. Stopped retained containers restart automatically.</p>
				</div>
			{/if}
		</div>
	{/if}
</div>
