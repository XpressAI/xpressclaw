<script lang="ts">
	import { onMount } from 'svelte';
	import type { LiveConfig } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	interface Props {
		agentId: string;
		agentConfig: LiveConfig['agents'][0] | null;
		onSave: (data: Record<string, unknown>) => Promise<void>;
	}

	let { agentId, agentConfig, onSave }: Props = $props();
	let newVolumePath = $state('');
	let saving = $state(false);

	interface FileEntry {
		name: string;
		size: number;
		url: string;
	}

	let files = $state<FileEntry[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let uploading = $state(false);

	// File viewer
	let viewingFile = $state<string | null>(null);
	let fileContent = $state<string | null>(null);
	let loadingContent = $state(false);

	let volumes = $state<{ host: string; container: string }[]>([]);

	$effect(() => {
		if (agentConfig) {
			volumes = (agentConfig.volumes || []).map(v => {
				const parts = v.split(':');
				return { host: parts[0] || v, container: parts[1] || '' };
			});
		}
	});

	onMount(() => {
		loadFiles();
	});

	async function loadFiles() {
		loading = true;
		error = null;
		try {
			const resp = await fetch(`/api/office/documents?agent_id=${agentId}`);
			files = await resp.json();
		} catch (e) {
			error = `Failed to load files: ${e}`;
		}
		loading = false;
	}

	async function viewFile(name: string) {
		if (viewingFile === name) {
			viewingFile = null;
			fileContent = null;
			return;
		}
		viewingFile = name;
		loadingContent = true;
		try {
			const resp = await fetch(`/api/office/documents/${encodeURIComponent(name)}/content?agent_id=${agentId}`);
			if (resp.ok) {
				const data = await resp.json();
				fileContent = data.content;
			} else {
				fileContent = '(binary file — cannot display)';
			}
		} catch {
			fileContent = '(failed to load)';
		}
		loadingContent = false;
	}

	async function deleteFile(name: string) {
		if (!confirm(`Delete ${name}?`)) return;
		try {
			await fetch(`/api/office/documents/${encodeURIComponent(name)}/delete?agent_id=${agentId}`, { method: 'POST' });
			files = files.filter(f => f.name !== name);
		} catch (e) {
			alert(`Failed to delete: ${e}`);
		}
	}

	async function uploadFiles(event: Event) {
		const input = event.target as HTMLInputElement;
		const fileList = input.files;
		if (!fileList || fileList.length === 0) return;

		uploading = true;
		try {
			const formData = new FormData();
			formData.append('agent_id', agentId);
			for (const file of fileList) {
				formData.append('file', file);
			}
			await fetch('/api/office/upload', { method: 'POST', body: formData });
			await loadFiles();
		} catch (e) {
			alert(`Upload failed: ${e}`);
		}
		uploading = false;
		input.value = '';
	}

	function formatSize(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	async function addVolume() {
		const path = newVolumePath.trim();
		if (!path) return;
		const basename = path.split('/').filter(Boolean).pop() || 'workspace';
		const mount = `${path}:/workspace/${basename}`;
		const rawVolumes = [...(agentConfig?.volumes || []), mount];
		saving = true;
		await onSave({ volumes: rawVolumes });
		saving = false;
		newVolumePath = '';
	}

	async function removeVolume(idx: number) {
		const rawVolumes = (agentConfig?.volumes || []).filter((_, i) => i !== idx);
		saving = true;
		await onSave({ volumes: rawVolumes });
		saving = false;
	}

	function isTextFile(name: string): boolean {
		const ext = name.split('.').pop()?.toLowerCase() ?? '';
		return ['txt', 'md', 'json', 'yaml', 'yml', 'toml', 'csv', 'log', 'xml',
			'html', 'css', 'js', 'ts', 'py', 'rs', 'sh', 'bash', 'env', 'cfg', 'ini',
			'svg', 'sql', 'graphql', 'dockerfile'].includes(ext);
	}
</script>

<div class="space-y-6">
	<!-- Documents / Files -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<div class="flex items-center justify-between">
			<div>
				<h2 class="text-sm font-semibold">Documents</h2>
				<p class="text-xs text-muted-foreground">Files in this agent's documents folder, accessible at <code class="bg-muted px-1 rounded text-xs">/workspace/Documents</code></p>
			</div>
			<label class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 cursor-pointer transition-colors {uploading ? 'opacity-50 pointer-events-none' : ''}">
				{uploading ? 'Uploading...' : 'Upload'}
				<input type="file" multiple onchange={uploadFiles} class="hidden" />
			</label>
		</div>

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading files...</p>
		{:else if error}
			<p class="text-sm text-destructive">{error}</p>
		{:else if files.length === 0}
			<div class="text-center py-6">
				<p class="text-sm text-muted-foreground">No documents yet. Upload files or let the agent create them.</p>
			</div>
		{:else}
			<div class="space-y-1">
				{#each files as file}
					<div class="rounded-md border border-border">
						<div class="flex items-center gap-3 px-3 py-2">
							<svg xmlns="http://www.w3.org/2000/svg" class="w-4 h-4 text-muted-foreground shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
								<path stroke-linecap="round" stroke-linejoin="round" d="M19.5 14.25v-2.625a3.375 3.375 0 0 0-3.375-3.375h-1.5A1.125 1.125 0 0 1 13.5 7.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H8.25m2.25 0H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 0 0-9-9Z" />
							</svg>
							<span class="text-sm font-mono flex-1 truncate">{file.name}</span>
							<span class="text-xs text-muted-foreground">{formatSize(file.size)}</span>
							<div class="flex gap-1">
								{#if isTextFile(file.name)}
									<button onclick={() => viewFile(file.name)}
										class="rounded px-2 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground">
										{viewingFile === file.name ? 'Close' : 'View'}
									</button>
								{/if}
								<a href="{file.url}?agent_id={agentId}" download
									class="rounded px-2 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground">
									Download
								</a>
								<button onclick={() => deleteFile(file.name)}
									class="rounded px-2 py-0.5 text-xs text-destructive hover:bg-destructive/10">
									Delete
								</button>
							</div>
						</div>
						{#if viewingFile === file.name}
							<div class="border-t border-border px-3 py-2 bg-muted/30">
								{#if loadingContent}
									<p class="text-xs text-muted-foreground">Loading...</p>
								{:else}
									<pre class="text-xs font-mono whitespace-pre-wrap max-h-80 overflow-y-auto">{fileContent}</pre>
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>

	<!-- Volume Mounts -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Volume Mounts</h2>
		<p class="text-xs text-muted-foreground">Host folders mounted into the agent's container. Changes require a restart.</p>

		{#if volumes.length > 0}
			<div class="space-y-1">
				{#each volumes as vol, i}
					<div class="flex items-center gap-2 rounded-md border border-border px-3 py-2">
						<span class="text-xs font-mono text-foreground truncate flex-1">{vol.host}</span>
						<span class="text-xs text-muted-foreground">&#8594;</span>
						<span class="text-xs font-mono text-muted-foreground truncate">{vol.container}</span>
						<button onclick={() => removeVolume(i)}
							class="rounded px-2 py-0.5 text-xs text-destructive hover:bg-destructive/10 shrink-0">
							Remove
						</button>
					</div>
				{/each}
			</div>
		{:else}
			<p class="text-xs text-muted-foreground italic">No volume mounts configured.</p>
		{/if}

		<div class="flex gap-2">
			<input type="text" bind:value={newVolumePath} placeholder="/path/on/host"
				onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') addVolume(); }}
				class="flex-1 rounded-md border border-border bg-background px-3 py-1.5 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
			<button onclick={addVolume} disabled={!newVolumePath.trim()}
				class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">
				Add Mount
			</button>
		</div>
	</div>
</div>
