<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import type { AgentAuthentication } from '$lib/api';
	import { openExternal } from '$lib/utils';
	type Field = { type?: string; title?: string; description?: string; format?: string; secret?: boolean; default?: unknown; enum?: unknown[]; oneOf?: { const: unknown; title?: string }[]; minimum?: number; maximum?: number; minLength?: number; maxLength?: number; items?: Field };
	type Prompt = { id: string; request: { mode: string; message: string; url?: string; requestedSchema?: { properties: Record<string, Field>; required?: string[] } } };
	let { agentId, method, onauthenticated }: { agentId: string; method: AgentAuthentication['methods'][number]; onauthenticated: () => void } = $props();
	let socket: WebSocket | null = null;
	let prompts = $state<Prompt[]>([]);
	let prompt = $derived(prompts[0] ?? null);
	let values = $state<Record<string, unknown>>({});
	let apiKey = $state('');
	let started = $state(false);
	let error = $state('');
	let completed = false;
	let disposed = false;
	function choices(field: Field) {
		return field.oneOf?.map(option => ({ value: String(option.const), label: option.title ?? String(option.const) }))
			?? field.enum?.map(value => ({ value: String(value), label: String(value) })) ?? [];
	}
	function defaults(next: Prompt | undefined) {
		values = Object.fromEntries(Object.entries(next?.request.requestedSchema?.properties ?? {}).filter(([,field]) => field.default !== undefined || field.type === 'boolean' || field.type === 'array').map(([key, field]) => [key, field.default ?? (field.type === 'boolean' ? false : [])]));
	}
	function begin() {
		socket?.close(); error = ''; prompts = []; values = {}; started = true; completed = false;
		const url = new URL(`/api/sessions/${encodeURIComponent(agentId)}/authentication/agent`, window.location.href);
		url.protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
		url.searchParams.set('method_id', method.id);
		const connection = new WebSocket(url); socket = connection;
		connection.onopen = () => { if (!disposed && socket === connection) connection.send(JSON.stringify({ type: 'start', ...(method.api_key && apiKey ? { api_key: apiKey } : {}) })); };
		connection.onmessage = event => {
			if (disposed || socket !== connection) return;
			try {
				const message = JSON.parse(String(event.data));
				if (message.type === 'prompt') {
					prompts = [...prompts, { id: message.id, request: message.request }];
					if (prompts.length === 1) defaults(prompts[0]);
				} else if (message.type === 'authenticated') { completed = true; apiKey = ''; values = {}; onauthenticated(); }
				else if (message.type === 'error') { error = message.message || 'Sign-in failed.'; prompts = []; }
			} catch { error = 'The sign-in response could not be read.'; }
		};
		connection.onerror = () => { if (!disposed && socket === connection) error = 'Could not connect to the Agent sign-in.'; };
		connection.onclose = () => { if (!disposed && socket === connection && !completed && !error) error = 'Sign-in was interrupted. Try again.'; };
	}
	function respond(accept: boolean) {
		if (!prompt || socket?.readyState !== WebSocket.OPEN) return;
		socket.send(JSON.stringify({ type: 'response', id: prompt.id, response: { action: accept ? 'accept' : 'cancel', ...(accept && prompt.request.mode === 'form' ? { content: values } : {}) } }));
		prompts = prompts.slice(1); defaults(prompts[0]);
	}
	function retry() { socket?.close(); if (method.api_key) { started = false; apiKey = ''; } else begin(); }
	function openPromptUrl() {
		const url = prompt?.request.url;
		if (url) void openExternal(url).catch(cause => error = String(cause));
	}
	onMount(() => { if (!method.api_key) begin(); });
	onDestroy(() => { disposed = true; socket?.close(); apiKey = ''; values = {}; });
</script>

<div class="space-y-4">
	{#if method.api_key && !started}
		<form onsubmit={event => { event.preventDefault(); begin(); }} class="space-y-3">
			<label class="block text-sm">API key<input type="password" autocomplete="off" bind:value={apiKey} class="mt-1 block w-full rounded border border-input bg-background px-3 py-2" /></label>
			<p class="text-xs text-muted-foreground">Leave blank to use the key already configured for this Agent.</p>
			<button class="rounded bg-primary px-3 py-2 text-sm text-primary-foreground">Sign in</button>
		</form>
	{:else if prompt}
		<p class="whitespace-pre-wrap text-sm">{prompt.request.message}</p>
		{#if prompt.request.mode === 'url' && prompt.request.url}
			<div class="flex flex-wrap gap-2">
				<button type="button" onclick={openPromptUrl} class="rounded bg-primary px-3 py-2 text-sm text-primary-foreground">Open provider sign-in</button>
				<button type="button" onclick={() => respond(true)} class="rounded border border-border px-3 py-2 text-sm">I've completed sign-in</button>
			</div>
		{:else if prompt.request.mode === 'form' && prompt.request.requestedSchema}
			<form class="space-y-3" onsubmit={event => { event.preventDefault(); respond(true); }}>
				{#each Object.entries(prompt.request.requestedSchema.properties) as [name, field]}
					{@const required = prompt.request.requestedSchema.required?.includes(name) ?? false}
					<label class="block text-sm">
						<span>{field.title ?? name}</span>
						{#if field.type === 'boolean'}
							<input type="checkbox" checked={Boolean(values[name])} onchange={event => values[name] = event.currentTarget.checked} class="ml-2" />
						{:else if field.type === 'array'}
							<select multiple {required} value={values[name] as string[]} onchange={event => values[name] = Array.from(event.currentTarget.selectedOptions, option => option.value)} class="mt-1 block w-full rounded border border-input bg-background p-2">{#each choices(field.items ?? {}) as choice}<option value={choice.value}>{choice.label}</option>{/each}</select>
						{:else if choices(field).length}
							<select {required} value={String(values[name] ?? '')} onchange={event => values[name] = event.currentTarget.value} class="mt-1 block w-full rounded border border-input bg-background p-2"><option value="">Choose…</option>{#each choices(field) as choice}<option value={choice.value}>{choice.label}</option>{/each}</select>
						{:else if field.type === 'number' || field.type === 'integer'}
							<input type="number" {required} min={field.minimum} max={field.maximum} step={field.type === 'integer' ? 1 : 'any'} value={values[name] === undefined ? undefined : Number(values[name])} oninput={event => values[name] = event.currentTarget.value === '' ? undefined : event.currentTarget.valueAsNumber} class="mt-1 block w-full rounded border border-input bg-background px-3 py-2" />
						{:else}
							<input type={field.secret || field.format === 'password' ? 'password' : 'text'} {required} minlength={field.minLength} maxlength={field.maxLength} value={String(values[name] ?? '')} oninput={event => values[name] = event.currentTarget.value} class="mt-1 block w-full rounded border border-input bg-background px-3 py-2" />
						{/if}
						{#if field.description}<span class="mt-1 block text-xs text-muted-foreground">{field.description}</span>{/if}
					</label>
				{/each}
				<button class="rounded bg-primary px-3 py-2 text-sm text-primary-foreground">Continue</button>
			</form>
		{:else}
			<p class="text-sm text-destructive">This sign-in prompt uses an unsupported format.</p>
		{/if}
	{:else}
		<p role="status" class="text-sm text-muted-foreground">Waiting for the provider to complete sign-in…</p>
	{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p><button type="button" onclick={retry} class="rounded border border-border px-3 py-1.5 text-sm">Try again</button>{/if}
</div>
