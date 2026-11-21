<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { Code2, ExternalLink, FileCode2, Folder, FolderOpen, Globe, Plus, RefreshCw, Radio } from '@lucide/svelte';
	import { environments, workspaces, type Agent, type PortForward, type WorkspaceEntry } from '$lib/api';
	import { classifyAppUrl, isHtmlFilePath } from '$lib/canvas';
	import { openExternal } from '$lib/utils';

	interface Props {
		nodeId: string;
		agentId: string | null;
		initialSource: 'port' | 'workspace-html' | 'url' | null;
		initialUrl: string | null;
		initialFilePath: string | null;
		agents: Agent[];
		onUpdateNode: (patch: {
			agentId?: string | null;
			appSource?: 'port' | 'workspace-html' | 'url' | null;
			appUrl?: string | null;
			appFilePath?: string | null;
			title?: string;
		}) => void;
		onOpenEditor: (agentId: string, filePath: string, parentNodeId: string) => void;
	}

	let {
		nodeId,
		agentId,
		initialSource,
		initialUrl,
		initialFilePath,
		agents,
		onUpdateNode,
		onOpenEditor,
	}: Props = $props();

	let activeAgentId = $state('');
	let sourceMode = $state<'port' | 'workspace-html' | 'url'>('port');
	let showLauncher = $state(true);
	let ports = $state<PortForward[]>([]);
	let loadingPorts = $state(false);
	let addingPort = $state(false);
	let newHostPort = $state(3000);
	let newContainerPort = $state(3000);

	let directories = $state<Record<string, WorkspaceEntry[]>>({});
	let expanded = $state<string[]>(['']);
	let loadingFiles = $state(false);
	let selectedHtmlPath = $state('');
	let htmlBlobUrl = $state('');

	let urlInput = $state('');
	let activeUrl = $state('');
	let reloadNonce = $state(0);
	let error = $state('');
	let disposed = false;
	let agentSequence = 0;

	let exposedPorts = $derived(ports.filter((port) => port.direction === 'container_to_host'));
	let htmlEntries = $derived(flattenHtmlEntries());
	let classifiedUrl = $derived(activeUrl ? classifyAppUrl(activeUrl, typeof window !== 'undefined' ? window.location.origin : '') : null);

	onMount(() => {
		activeAgentId = agentId || agents[0]?.id || '';
		sourceMode = initialSource ?? (initialFilePath ? 'workspace-html' : initialUrl ? 'url' : 'port');
		if (initialUrl) {
			urlInput = initialUrl;
			activeUrl = initialUrl;
			showLauncher = false;
		}
		if (initialFilePath) {
			selectedHtmlPath = initialFilePath;
			showLauncher = false;
		}
		if (activeAgentId) {
			void loadPorts(activeAgentId);
			void loadWorkspaceHtmlTree(activeAgentId);
			if (sourceMode === 'workspace-html' && initialFilePath) {
				void loadWorkspaceHtml(activeAgentId, initialFilePath);
			}
		}
	});

	onDestroy(() => {
		disposed = true;
		if (htmlBlobUrl) URL.revokeObjectURL(htmlBlobUrl);
	});

	async function loadPorts(targetAgentId: string) {
		const req = agentSequence;
		loadingPorts = true;
		try {
			const loaded = (await environments.ports(targetAgentId)).ports;
			if (req !== agentSequence || activeAgentId !== targetAgentId) return;
			ports = loaded;
		} catch {
			if (req === agentSequence && activeAgentId === targetAgentId) ports = [];
		} finally {
			if (req === agentSequence && activeAgentId === targetAgentId) loadingPorts = false;
		}
	}

	async function exposePort() {
		const targetAgentId = activeAgentId;
		if (!targetAgentId || addingPort) return;
		const req = agentSequence;
		addingPort = true;
		error = '';
		try {
			const created = await environments.addPort(targetAgentId, {
				direction: 'container_to_host',
				host_port: Number(newHostPort),
				container_port: Number(newContainerPort),
			});
			if (req !== agentSequence || activeAgentId !== targetAgentId) return;
			await loadPorts(targetAgentId);
			if (req === agentSequence && activeAgentId === targetAgentId) selectPort(created);
		} catch (cause) {
			if (req === agentSequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (req === agentSequence && activeAgentId === targetAgentId) addingPort = false;
		}
	}

	function selectPort(port: PortForward) {
		const url = `http://127.0.0.1:${port.host_port}/`;
		sourceMode = 'port';
		urlInput = url;
		activeUrl = url;
		error = '';
		showLauncher = false;
		onUpdateNode({
			agentId: activeAgentId || null,
			appSource: 'port',
			appUrl: url,
			appFilePath: null,
			title: `App :${port.host_port}`,
		});
	}

	async function loadWorkspaceHtmlTree(targetAgentId: string) {
		const req = agentSequence;
		loadingFiles = true;
		try {
			const dir = await workspaces.tree(targetAgentId);
			if (req !== agentSequence || activeAgentId !== targetAgentId) return;
			directories = { '': dir.entries };
			expanded = [''];
		} catch {
			if (req === agentSequence && activeAgentId === targetAgentId) directories = {};
		} finally {
			if (req === agentSequence && activeAgentId === targetAgentId) loadingFiles = false;
		}
	}

	function flattenHtmlEntries(): { entry: WorkspaceEntry; depth: number }[] {
		const rows: { entry: WorkspaceEntry; depth: number }[] = [];
		const visit = (dir: string, depth: number) => {
			for (const entry of directories[dir] ?? []) {
				if (entry.path === '.git' || entry.path === 'node_modules') continue;
				if (entry.kind === 'directory' || (entry.kind === 'file' && isHtmlFilePath(entry.path))) {
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
		const req = agentSequence;
		try {
			const dir = await workspaces.tree(targetAgentId, path);
			if (req !== agentSequence || activeAgentId !== targetAgentId) return;
			directories = { ...directories, [path]: dir.entries };
		} catch {}
	}

	async function loadWorkspaceHtml(targetAgentId: string, filePath: string) {
		const req = agentSequence;
		error = '';
		try {
			const file = await workspaces.readFile(targetAgentId, filePath);
			if (disposed || req !== agentSequence || activeAgentId !== targetAgentId) return;
			if (htmlBlobUrl) URL.revokeObjectURL(htmlBlobUrl);
			htmlBlobUrl = URL.createObjectURL(new Blob([file.content], { type: 'text/html;charset=utf-8' }));
			sourceMode = 'workspace-html';
			selectedHtmlPath = file.path;
			activeUrl = '';
			showLauncher = false;
			onUpdateNode({
				agentId: targetAgentId,
				appSource: 'workspace-html',
				appFilePath: file.path,
				appUrl: null,
				title: `App · ${file.path.split('/').pop() || file.path}`,
			});
		} catch (cause) {
			if (req === agentSequence && activeAgentId === targetAgentId) {
				error = cause instanceof Error ? cause.message : String(cause);
			}
		}
	}

	function applyCustomUrl() {
		const check = classifyAppUrl(urlInput, window.location.origin);
		if (!check.valid) {
			error = check.blockedReason || 'Invalid app URL.';
			return;
		}
		error = '';
		sourceMode = 'url';
		activeUrl = check.normalizedUrl;
		urlInput = check.normalizedUrl;
		showLauncher = false;
		const parsed = new URL(check.normalizedUrl);
		onUpdateNode({
			appSource: 'url',
			appUrl: check.normalizedUrl,
			appFilePath: null,
			title: `App · ${parsed.host}`,
		});
	}

	function refreshPreview() {
		if (sourceMode === 'workspace-html' && activeAgentId && selectedHtmlPath) {
			void loadWorkspaceHtml(activeAgentId, selectedHtmlPath);
		} else {
			reloadNonce += 1;
		}
	}

	function handleAgentChange(nextId: string) {
		agentSequence += 1;
		loadingPorts = false;
		loadingFiles = false;
		addingPort = false;
		activeAgentId = nextId;
		ports = [];
		directories = {};
		expanded = [''];
		error = '';
		onUpdateNode({ agentId: nextId || null });
		if (nextId) {
			void loadPorts(nextId);
			void loadWorkspaceHtmlTree(nextId);
		}
	}
</script>

<div class="flex h-full min-h-0 flex-col bg-background text-xs" data-canvas-app-card>
	<div class="flex flex-wrap items-center gap-1.5 border-b border-border bg-[hsl(var(--inset))] px-2 py-1.5">
		<button
			type="button"
			onclick={() => (showLauncher = !showLauncher)}
			class="rounded border border-border bg-card px-2 py-0.5 text-[11px] hover:bg-accent"
		>
			{showLauncher ? 'Hide sources' : 'Sources'}
		</button>
		<form
			class="flex min-w-36 flex-1 items-center gap-1"
			onsubmit={(event) => { event.preventDefault(); applyCustomUrl(); }}
		>
			<input
				aria-label="App preview URL"
				bind:value={urlInput}
				placeholder="http://127.0.0.1:3000"
				class="min-w-0 flex-1 rounded border border-input bg-background px-2 py-0.5 font-mono text-[11px]"
			/>
			<button class="rounded border border-border bg-card px-2 py-0.5 text-[11px] hover:bg-accent">Go</button>
		</form>
		{#if sourceMode === 'workspace-html' && activeAgentId && selectedHtmlPath}
			<button
				type="button"
				onclick={() => onOpenEditor(activeAgentId, selectedHtmlPath, nodeId)}
				class="inline-flex items-center gap-1 rounded border border-border bg-card px-2 py-0.5 text-[11px] hover:bg-accent"
				title="Open HTML source in a connected Code Editor window"
			>
				<Code2 class="h-3 w-3 text-sky-500" />
				<span>Edit</span>
			</button>
		{/if}
		<button type="button" onclick={refreshPreview} class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground" title="Reload app" aria-label="Reload app">
			<RefreshCw class="h-3.5 w-3.5" />
		</button>
		{#if activeUrl && classifiedUrl?.valid}
			<button
				type="button"
				onclick={() => void openExternal(classifiedUrl.normalizedUrl)}
				class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
				title="Open in external browser"
				aria-label="Open app externally"
			>
				<ExternalLink class="h-3.5 w-3.5" />
			</button>
		{/if}
	</div>

	{#if error}
		<div role="alert" class="border-b border-destructive/30 bg-destructive/10 px-3 py-1.5 text-[11px] text-destructive">{error}</div>
	{/if}

	<div class="flex min-h-0 flex-1 overflow-hidden">
		{#if showLauncher}
			<div class="workspace-scroll-y w-60 shrink-0 space-y-3 border-r border-border bg-[hsl(var(--inset))] p-2.5">
				<div>
					<label class="block text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
						Agent context
						<select
							aria-label="App agent"
							value={activeAgentId}
							onchange={(event) => handleAgentChange(event.currentTarget.value)}
							class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-[11px] font-normal text-foreground"
						>
							{#if agents.length === 0}<option value="">No agents</option>{/if}
							{#each agents as agent (agent.id)}
								<option value={agent.id}>{agent.title || agent.name}</option>
							{/each}
						</select>
					</label>
				</div>

				<section class="space-y-1.5">
					<div class="flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
						<span class="inline-flex items-center gap-1"><Radio class="h-3 w-3 text-emerald-500" /> Exposed Ports</span>
						{#if activeAgentId}
							<button type="button" onclick={() => void loadPorts(activeAgentId)} class="text-[10px] font-normal text-primary hover:underline">Refresh</button>
						{/if}
					</div>
					{#if loadingPorts}
						<p class="text-[11px] text-muted-foreground">Checking forwarded ports…</p>
					{:else if exposedPorts.length === 0}
						<p class="text-[11px] text-muted-foreground">No container ports exposed to host yet.</p>
					{:else}
						<div class="space-y-1">
							{#each exposedPorts as port (port.id)}
								<button
									type="button"
									onclick={() => selectPort(port)}
									class="flex w-full items-center justify-between rounded-md border border-border bg-card px-2 py-1.5 text-left hover:border-primary/40 hover:bg-accent"
								>
									<span class="font-mono text-[11px]">:{port.host_port} ← :{port.container_port}</span>
									<span class="h-2 w-2 rounded-full {port.active ? 'bg-emerald-500' : 'bg-muted-foreground'}"></span>
								</button>
							{/each}
						</div>
					{/if}
					{#if activeAgentId}
						<form class="mt-1.5 space-y-1.5 rounded-md border border-border/80 bg-card p-2" onsubmit={(event) => { event.preventDefault(); void exposePort(); }}>
							<div class="text-[10px] font-medium text-muted-foreground">Expose container port</div>
							<div class="grid grid-cols-2 gap-1.5">
								<label class="text-[10px] text-muted-foreground">
									Host
									<input type="number" min="1" max="65535" bind:value={newHostPort} class="mt-0.5 w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[11px] text-foreground" />
								</label>
								<label class="text-[10px] text-muted-foreground">
									Container
									<input type="number" min="1" max="65535" bind:value={newContainerPort} class="mt-0.5 w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[11px] text-foreground" />
								</label>
							</div>
							<button disabled={addingPort} class="inline-flex w-full items-center justify-center gap-1 rounded bg-primary px-2 py-1 text-[11px] font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-40">
								<Plus class="h-3 w-3" />
								<span>{addingPort ? 'Exposing…' : 'Expose & Open'}</span>
							</button>
						</form>
					{/if}
				</section>

				<section class="space-y-1.5">
					<div class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Workspace HTML Apps</div>
					{#if loadingFiles}
						<p class="text-[11px] text-muted-foreground">Scanning workspace…</p>
					{:else if htmlEntries.length === 0}
						<p class="text-[11px] text-muted-foreground">No .html files in workspace root.</p>
					{:else}
						<div class="space-y-0.5">
							{#each htmlEntries as row (row.entry.path)}
								<button
									type="button"
									onclick={() => row.entry.kind === 'directory' ? void toggleDirectory(row.entry.path) : void loadWorkspaceHtml(activeAgentId, row.entry.path)}
									style="padding-left: {row.depth * 10 + 6}px"
									class="flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left text-[11px] hover:bg-accent {selectedHtmlPath === row.entry.path ? 'bg-accent font-medium text-accent-foreground' : 'text-foreground'}"
									title={row.entry.path}
								>
									{#if row.entry.kind === 'directory'}
										{#if expanded.includes(row.entry.path)}<FolderOpen class="h-3.5 w-3.5 shrink-0 text-amber-500" />{:else}<Folder class="h-3.5 w-3.5 shrink-0 text-amber-500" />{/if}
									{:else}
										<FileCode2 class="h-3.5 w-3.5 shrink-0 text-amber-500" />
									{/if}
									<span class="min-w-0 flex-1 truncate">{row.entry.name}</span>
								</button>
							{/each}
						</div>
					{/if}
				</section>
			</div>
		{/if}

		<div class="relative flex min-h-0 min-w-0 flex-1 flex-col bg-card">
			{#if sourceMode === 'workspace-html' && htmlBlobUrl}
				{#key `${selectedHtmlPath}:${reloadNonce}`}
					<iframe
						title={selectedHtmlPath || 'Workspace HTML app'}
						src={htmlBlobUrl}
						sandbox="allow-scripts allow-forms"
						referrerpolicy="no-referrer"
						class="h-full w-full border-0 bg-white"
						data-canvas-app-frame
					></iframe>
				{/key}
			{:else if classifiedUrl?.valid}
				{#key `${classifiedUrl.normalizedUrl}:${reloadNonce}`}
					<iframe
						title={classifiedUrl.normalizedUrl}
						src={classifiedUrl.normalizedUrl}
						sandbox={classifiedUrl.sandbox}
						referrerpolicy="no-referrer"
						class="h-full w-full border-0 bg-white"
						data-canvas-app-frame
					></iframe>
				{/key}
			{:else}
				<div class="m-auto max-w-sm space-y-2 p-6 text-center text-muted-foreground">
					<Globe class="mx-auto h-8 w-8 text-amber-500/70" />
					<p class="text-xs font-medium text-foreground">Embed a live app in spatial context</p>
					<p class="text-[11px]">
						Pick an exposed Agent container port, open a workspace HTML app, or enter a local dev server URL (e.g. <code class="rounded bg-muted px-1 py-0.5 font-mono">http://127.0.0.1:3000</code>).
					</p>
				</div>
			{/if}
		</div>
	</div>
</div>
