<script lang="ts">
	import { onMount } from 'svelte';
	import { mcpServers } from '$lib/api';
	import type { McpServerDefinition } from '$lib/api';

	let servers = $state<McpServerDefinition[]>([]);
	let loading = $state(true);
	let editingName = $state<string | null>(null);
	let showForm = $state(false);
	let name = $state('');
	let serverType = $state<'stdio' | 'http' | 'sse'>('stdio');
	let commandOrUrl = $state('');
	let argsText = $state('');
	let environmentText = $state('');
	let headersText = $state('');
	let saving = $state(false);
	let deleting = $state<string | null>(null);
	let confirmDelete = $state<string | null>(null);
	let error = $state('');

	onMount(load);

	async function load() {
		loading = true;
		error = '';
		try {
			servers = (await mcpServers.list()).servers;
		} catch (reason) {
			error = reason instanceof Error ? reason.message : 'Could not load MCP servers';
		} finally {
			loading = false;
		}
	}

	function keyValueLines(value: Record<string, string> | string[] | undefined): string {
		if (!value || Array.isArray(value)) return '';
		return Object.entries(value).map(([key, entry]) => `${key}=${entry}`).join('\n');
	}

	function parseKeyValueLines(value: string): Record<string, string> {
		return Object.fromEntries(value.split('\n').map((line) => line.trim()).filter(Boolean).map((line) => {
			const separator = line.indexOf('=');
			return separator < 0
				? [line, '']
				: [line.slice(0, separator).trim(), line.slice(separator + 1)];
		}).filter(([key]) => key));
	}

	function resetForm() {
		editingName = null;
		showForm = false;
		name = '';
		serverType = 'stdio';
		commandOrUrl = '';
		argsText = '';
		environmentText = '';
		headersText = '';
		error = '';
	}

	function createServer() {
		resetForm();
		showForm = true;
	}

	function editServer(server: McpServerDefinition) {
		editingName = server.name;
		showForm = true;
		name = server.name;
		serverType = server.type === 'http' || server.type === 'sse' ? server.type : 'stdio';
		commandOrUrl = serverType === 'stdio' ? server.command ?? '' : server.url ?? '';
		argsText = server.args.join('\n');
		environmentText = keyValueLines(server.env);
		headersText = keyValueLines(server.headers);
		error = '';
	}

	async function save() {
		if (!name.trim() || !commandOrUrl.trim() || saving) return;
		saving = true;
		error = '';
		try {
			await mcpServers.upsert({
				name: name.trim(),
				type: serverType,
				command: serverType === 'stdio' ? commandOrUrl.trim() : null,
				args: serverType === 'stdio' ? argsText.split('\n').map((line) => line.trim()).filter(Boolean) : [],
				env: serverType === 'stdio' ? parseKeyValueLines(environmentText) : {},
				url: serverType === 'stdio' ? null : commandOrUrl.trim(),
				headers: serverType === 'stdio' ? {} : parseKeyValueLines(headersText)
			});
			resetForm();
			await load();
		} catch (reason) {
			error = reason instanceof Error ? reason.message : 'Could not save MCP server';
		} finally {
			saving = false;
		}
	}

	async function remove(serverName: string) {
		if (confirmDelete !== serverName) {
			confirmDelete = serverName;
			return;
		}
		deleting = serverName;
		error = '';
		try {
			await mcpServers.delete(serverName);
			confirmDelete = null;
			await load();
		} catch (reason) {
			error = reason instanceof Error ? reason.message : 'Could not delete MCP server';
		} finally {
			deleting = null;
		}
	}
</script>

<div class="space-y-6 p-4 sm:p-6">
	<div class="flex items-start justify-between gap-4">
		<div>
			<h1 class="text-2xl font-bold">MCP servers</h1>
			<p class="mt-1 max-w-2xl text-sm text-muted-foreground">Create a shared catalog of tools that can be attached to any project from its Agent tab. Servers are never attached automatically.</p>
		</div>
		<button type="button" onclick={createServer} class="shrink-0 rounded-lg bg-primary px-3.5 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Add server</button>
	</div>

	<div class="rounded-xl border border-border bg-card">
		{#if loading}
			<div class="flex items-center justify-center gap-2 px-4 py-12 text-sm text-muted-foreground">
				<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
				Loading MCP servers…
			</div>
		{:else if servers.length === 0}
			<div class="px-4 py-12 text-center">
				<p class="text-sm font-medium text-foreground">No shared MCP servers yet</p>
				<p class="mt-1 text-xs text-muted-foreground">Add a remote HTTP/SSE server or a stdio executable included in your runner images.</p>
			</div>
		{:else}
			<div class="divide-y divide-border">
				{#each servers as server}
					<div class="flex items-start gap-3 px-4 py-4">
						<span class="rounded-md bg-muted px-2 py-1 text-[10px] font-medium uppercase text-muted-foreground">{server.type}</span>
						<div class="min-w-0 flex-1">
							<p class="text-sm font-medium text-foreground">{server.name}</p>
							<p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">{server.command || server.url}</p>
						</div>
						<button type="button" onclick={() => editServer(server)} class="text-xs text-muted-foreground hover:text-foreground">Edit</button>
						<button type="button" onclick={() => remove(server.name)} disabled={deleting === server.name} class="text-xs {confirmDelete === server.name ? 'font-medium text-destructive' : 'text-muted-foreground hover:text-destructive'} disabled:opacity-50">
							{deleting === server.name ? 'Deleting…' : confirmDelete === server.name ? 'Confirm delete' : 'Delete'}
						</button>
					</div>
				{/each}
			</div>
		{/if}
	</div>

	{#if showForm}
		<div class="rounded-xl border border-border bg-card p-4 sm:p-5">
			<h2 class="text-sm font-semibold">{editingName ? `Edit ${editingName}` : 'Add MCP server'}</h2>
			<div class="mt-4 grid gap-4 sm:grid-cols-2">
				<div>
					<label for="mcp-name" class="mb-1 block text-xs font-medium text-muted-foreground">Name</label>
					<input id="mcp-name" bind:value={name} disabled={editingName !== null} placeholder="documentation" class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm disabled:opacity-60" />
				</div>
				<div>
					<label for="mcp-type" class="mb-1 block text-xs font-medium text-muted-foreground">Transport</label>
					<select id="mcp-type" bind:value={serverType} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"><option value="stdio">stdio</option><option value="http">HTTP</option><option value="sse">SSE</option></select>
				</div>
			</div>
			<div class="mt-4">
				<label for="mcp-command" class="mb-1 block text-xs font-medium text-muted-foreground">{serverType === 'stdio' ? 'Command inside the runner' : 'Server URL'}</label>
				<input id="mcp-command" bind:value={commandOrUrl} placeholder={serverType === 'stdio' ? '/absolute/path/in/container' : 'https://mcp.example.com'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs" />
				{#if serverType === 'stdio'}<p class="mt-1 text-[11px] text-muted-foreground">Use an absolute path that exists in every runner image where you attach this server.</p>{/if}
			</div>
			{#if serverType === 'stdio'}
				<div class="mt-4 grid gap-4 sm:grid-cols-2">
					<div><label for="mcp-args" class="mb-1 block text-xs font-medium text-muted-foreground">Arguments</label><textarea id="mcp-args" bind:value={argsText} rows="4" placeholder="One argument per line" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea></div>
					<div><label for="mcp-env" class="mb-1 block text-xs font-medium text-muted-foreground">Environment</label><textarea id="mcp-env" bind:value={environmentText} rows="4" placeholder={'One KEY=value per line\nAPI_TOKEN=…'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea></div>
				</div>
			{:else}
				<div class="mt-4"><label for="mcp-headers" class="mb-1 block text-xs font-medium text-muted-foreground">HTTP headers</label><textarea id="mcp-headers" bind:value={headersText} rows="4" placeholder={'One Name=value per line\nAuthorization=Bearer …'} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea></div>
			{/if}
			<p class="mt-3 text-[11px] leading-relaxed text-muted-foreground">Credentials are stored in the local XpressClaw configuration and supplied only to projects that explicitly attach this server.</p>
			{#if error}<p class="mt-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">{error}</p>{/if}
			<div class="mt-4 flex justify-end gap-2">
				<button type="button" onclick={resetForm} class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent">Cancel</button>
				<button type="button" onclick={save} disabled={saving || !name.trim() || !commandOrUrl.trim()} class="rounded-md bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving…' : 'Save server'}</button>
			</div>
		</div>
	{/if}

	{#if error && !showForm}
		<p class="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">{error}</p>
	{/if}
</div>
