<script lang="ts">
	import { onMount } from 'svelte';
	import { agents, settings } from '$lib/api';
	import type { Agent, CollaborationConfig, CollaborationServiceStatus, CollaborationSettings } from '$lib/api';

	type Action = 'install' | 'start' | 'stop' | 'restart' | 'upgrade';
	type Service = 'gitbucket' | 'jenkins';

	let data = $state<CollaborationSettings | null>(null);
	let agentList = $state<Agent[]>([]);
	let form = $state<CollaborationConfig | null>(null);
	let loading = $state(true);
	let busy = $state<string | null>(null);
	let error = $state('');
	let notice = $state('');
	let resetText = $state('');
	let serviceLogs = $state<Record<Service, string>>({ gitbucket: '', jenkins: '' });

	onMount(async () => {
		try {
			const [loaded, loadedAgents] = await Promise.all([
				settings.getCollaboration(),
				agents.list()
			]);
			apply(loaded);
			agentList = loadedAgents;
		} catch (reason) {
			error = message(reason, 'Could not load Local collaboration settings');
		} finally {
			loading = false;
		}
	});

	function apply(next: CollaborationSettings) {
		data = next;
		form = structuredClone(next.config);
	}

	async function save() {
		if (!form || busy) return;
		busy = 'save';
		error = '';
		notice = '';
		try {
			apply(await settings.putCollaboration(form));
			notice = 'Settings saved. Services start only when you choose Install or Start.';
		} catch (reason) {
			error = message(reason, 'Could not save settings');
		} finally {
			busy = null;
		}
	}

	async function run(action: Action) {
		if (busy || ((action === 'install' || action === 'restart') && !form?.enabled)) return;
		busy = action;
		error = '';
		notice = '';
		try {
			// Install, Restart, and Upgrade reconcile immutable Docker configuration. Save
			// the visible form first so they cannot run against stale ports, images,
			// assignments, or a still-disabled configuration.
			if ((action === 'install' || action === 'restart' || action === 'upgrade') && form) {
				apply(await settings.putCollaboration(form));
			}
			apply(await settings.runCollaborationAction(action));
			notice = action === 'stop'
				? 'Services stopped. Persistent repositories, build history, and credentials were preserved.'
				: action.charAt(0).toUpperCase() + action.slice(1) + ' completed.';
		} catch (reason) {
			error = message(reason, 'Could not ' + action + ' local collaboration services');
		} finally {
			busy = null;
		}
	}

	async function reset() {
		if (!data || busy || resetText !== data.reset_confirmation) return;
		busy = 'reset';
		error = '';
		notice = '';
		try {
			apply(await settings.resetCollaboration(resetText));
			resetText = '';
			notice = 'Containers, persistent volumes, and generated credentials were removed.';
		} catch (reason) {
			error = message(reason, 'Could not reset local collaboration services');
		} finally {
			busy = null;
		}
	}

	async function showLogs(service: Service) {
		if (busy) return;
		if (serviceLogs[service]) {
			serviceLogs = { ...serviceLogs, [service]: '' };
			return;
		}
		busy = 'logs-' + service;
		error = '';
		try {
			serviceLogs = {
				...serviceLogs,
				[service]: (await settings.getCollaborationLogs(service)).logs || 'No logs yet.'
			};
		} catch (reason) {
			error = message(reason, 'Could not load ' + service + ' logs');
		} finally {
			busy = null;
		}
	}

	function toggleAgent(agentId: string) {
		if (!form) return;
		form.authorized_agents = form.authorized_agents.includes(agentId)
			? form.authorized_agents.filter((id) => id !== agentId)
			: [...form.authorized_agents, agentId];
	}

	function message(reason: unknown, fallback: string) {
		return reason instanceof Error ? reason.message : fallback;
	}

	function badge(service: CollaborationServiceStatus) {
		if (service.health === 'healthy') return 'border-emerald-500/30 bg-emerald-500/10 text-emerald-500';
		if (service.state === 'running' || service.health === 'starting') return 'border-amber-500/30 bg-amber-500/10 text-amber-500';
		if (service.state === 'not_installed' || service.state === 'exited') return 'border-border bg-muted text-muted-foreground';
		return 'border-red-500/30 bg-red-500/10 text-red-500';
	}

	function label(service: CollaborationServiceStatus) {
		if (service.health === 'healthy') return 'Healthy';
		if (service.health === 'starting') return 'Starting';
		if (service.state === 'not_installed') return 'Not installed';
		if (service.state === 'exited') return 'Stopped';
		return service.state.replaceAll('_', ' ');
	}
</script>

<div class="p-6 space-y-6" data-testid="local-collaboration-settings">
	<div>
		<h1 class="text-2xl font-bold">Local collaboration services</h1>
		<p class="mt-1 max-w-3xl text-sm text-muted-foreground">
			Run an optional self-hosted GitBucket forge and Jenkins build service for selected Agents. Existing GitHub workflows are unchanged.
		</p>
	</div>

	{#if error}
		<div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-500" role="alert">{error}</div>
	{/if}
	{#if notice}
		<div class="rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-500" role="status">{notice}</div>
	{/if}

	{#if loading}
		<p class="text-sm text-muted-foreground">Inspecting Docker and local services…</p>
	{:else if form && data}
		<section class="rounded-lg border border-border bg-card p-4 space-y-4">
			<label class="flex items-start gap-3">
				<input type="checkbox" class="mt-1" checked={form.enabled} onchange={(event) => form && (form.enabled = event.currentTarget.checked)} />
				<span>
					<span class="block text-sm font-semibold">Enable local collaboration configuration</span>
					<span class="block text-xs text-muted-foreground">This opt-in alone does not start containers. Lifecycle actions remain explicit.</span>
				</span>
			</label>
			<div class="grid gap-4 sm:grid-cols-3">
				<label class="space-y-1 text-xs"><span class="font-medium">Host bind address</span><input class="w-full rounded-md border border-border bg-background px-3 py-2 font-mono" bind:value={form.bind_address} /></label>
				<label class="space-y-1 text-xs"><span class="font-medium">GitBucket port</span><input type="number" min="1" max="65535" class="w-full rounded-md border border-border bg-background px-3 py-2" bind:value={form.gitbucket_port} /></label>
				<label class="space-y-1 text-xs"><span class="font-medium">Jenkins port</span><input type="number" min="1" max="65535" class="w-full rounded-md border border-border bg-background px-3 py-2" bind:value={form.jenkins_port} /></label>
			</div>
			{#if form.bind_address !== '127.0.0.1' && form.bind_address !== '::1'}
				<p class="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-500">
					This address can expose administrator interfaces beyond this machine. Use an authenticated TLS reverse proxy and firewall.
				</p>
			{/if}
			<details class="text-xs">
				<summary class="cursor-pointer font-medium">Pinned images and upgrade policy</summary>
				<div class="mt-3 grid gap-3 sm:grid-cols-2">
					<label class="space-y-1"><span>GitBucket image</span><input class="w-full rounded-md border border-border bg-background px-3 py-2 font-mono" bind:value={form.gitbucket_image} /></label>
					<label class="space-y-1"><span>Jenkins image</span><input class="w-full rounded-md border border-border bg-background px-3 py-2 font-mono" bind:value={form.jenkins_image} /></label>
				</div>
				<p class="mt-2 text-muted-foreground">Upgrade pulls these explicit tags and recreates only managed containers; volumes are retained.</p>
			</details>
			<div class="flex justify-end">
				<button class="rounded-md bg-primary px-3 py-2 text-xs font-medium text-primary-foreground disabled:opacity-50" disabled={busy !== null} onclick={save}>{busy === 'save' ? 'Saving…' : 'Save configuration'}</button>
			</div>
		</section>

		<div class="grid gap-3 lg:grid-cols-2">
			{#each [['gitbucket', 'GitBucket', data.status.gitbucket], ['jenkins', 'Jenkins', data.status.jenkins]] as entry}
				{@const key = entry[0] as Service}
				{@const name = entry[1] as string}
				{@const service = entry[2] as CollaborationServiceStatus}
				<section class="rounded-lg border border-border bg-card p-4 space-y-3">
					<div class="flex items-center justify-between gap-3">
						<div><h2 class="text-sm font-semibold">{name}</h2><p class="text-xs text-muted-foreground">{key === 'gitbucket' ? 'Repositories, issues, and pull requests' : 'Build execution, status, and logs'}</p></div>
						<span class="rounded-full border px-2 py-1 text-[10px] font-semibold capitalize {badge(service)}">{label(service)}</span>
					</div>
					<dl class="space-y-2 text-xs">
						<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Browser URL</dt><dd><a class="text-primary hover:underline" href={service.host_url} target="_blank" rel="noreferrer">{service.host_url}</a></dd></div>
						<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Agent endpoint</dt><dd class="font-mono">{service.internal_url}</dd></div>
						<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Image</dt><dd class="break-all text-right font-mono">{service.image}</dd></div>
						<div class="flex justify-between gap-4"><dt class="text-muted-foreground">Volume</dt><dd class="break-all text-right font-mono">{service.volume}</dd></div>
					</dl>
					{#if service.error}<p class="rounded-md bg-red-500/10 px-3 py-2 text-xs text-red-500">{service.error}</p>{/if}
					<button class="text-xs text-primary hover:underline disabled:opacity-50" disabled={busy !== null} onclick={() => showLogs(key)}>
						{serviceLogs[key] ? 'Hide logs' : busy === 'logs-' + key ? 'Loading logs…' : 'Inspect logs'}
					</button>
					{#if serviceLogs[key]}<pre class="max-h-64 overflow-auto rounded-md bg-muted p-3 text-[10px] whitespace-pre-wrap">{serviceLogs[key]}</pre>{/if}
				</section>
			{/each}
		</div>
		<p class="text-xs text-muted-foreground">Managed data directory: <span class="font-mono">{data.status.data_path}</span></p>

		<section class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div><h2 class="text-sm font-semibold">Service lifecycle</h2><p class="mt-1 text-xs text-muted-foreground">Stop preserves all data. Restart applies the visible configuration and recreates containers while retaining named volumes.</p></div>
			<div class="flex flex-wrap gap-2">
				<button class="rounded-md bg-primary px-3 py-2 text-xs font-medium text-primary-foreground disabled:opacity-50" disabled={busy !== null || !form.enabled} onclick={() => run('install')}>{busy === 'install' ? 'Installing…' : 'Install services'}</button>
				{#each ['start', 'stop', 'restart', 'upgrade'] as action}
					<button class="rounded-md border border-border px-3 py-2 text-xs capitalize disabled:opacity-50" disabled={busy !== null || (action === 'restart' && !form.enabled)} onclick={() => run(action as Action)}>
						{busy === action ? action + '…' : action === 'upgrade' ? 'Upgrade pinned images' : action === 'restart' ? 'Restart & apply configuration' : action}
					</button>
				{/each}
			</div>
		</section>

		<section class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div><h2 class="text-sm font-semibold">Agent access</h2><p class="mt-1 text-xs text-muted-foreground">Only selected Agents join the dedicated Docker network and receive constrained forge/build tools.</p></div>
			<div class="grid gap-2 sm:grid-cols-2">
				{#each agentList as agent (agent.id)}
					<label class="flex items-center gap-3 rounded-md border border-border px-3 py-2 text-xs">
						<input type="checkbox" checked={form.authorized_agents.includes(agent.name)} onchange={() => toggleAgent(agent.name)} />
						<span><span class="block font-medium">{agent.title}</span><span class="text-muted-foreground">{agent.name}</span></span>
					</label>
				{/each}
			</div>
			<p class="text-xs text-muted-foreground">Save after changing assignments. A retained Agent container is recreated at its next safe launch.</p>
		</section>

		<section class="rounded-lg border border-border bg-card p-4 space-y-3">
			<h2 class="text-sm font-semibold">Security and current limitations</h2>
			<ul class="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
				<li>Host interfaces bind to loopback by default; Agent containers use isolated internal aliases.</li>
				<li>Jenkins gets no host Docker socket, privileged mode, Docker-in-Docker, or experimental GitBucket CI plugin.</li>
				<li>The managed job builds public repositories containing <code>.xpressclaw/jenkins.sh</code>. Private Jenkins clones, artifacts, GitHub-style approvals, and commit checks are follow-up capabilities.</li>
				<li>Git mirroring covers commits, branches, and tags—not pull requests, reviews, issue comments, or build metadata.</li>
			</ul>
		</section>

		<details class="rounded-lg border border-red-500/20 bg-card p-4">
			<summary class="cursor-pointer text-sm font-semibold text-red-500">Permanently reset local collaboration data</summary>
			<div class="mt-3 space-y-3">
				<p class="text-xs text-muted-foreground">Deletes containers, named volumes, repositories, build history, and credentials. Back up volumes first; Stop is the reversible action.</p>
				<label class="block space-y-1 text-xs"><span>Type <span class="font-mono">{data.reset_confirmation}</span></span><input class="w-full rounded-md border border-red-500/30 bg-background px-3 py-2 font-mono" bind:value={resetText} /></label>
				<button class="rounded-md bg-red-600 px-3 py-2 text-xs font-medium text-white disabled:opacity-50" disabled={busy !== null || resetText !== data.reset_confirmation} onclick={reset}>{busy === 'reset' ? 'Resetting…' : 'Delete services and persistent data'}</button>
			</div>
		</details>
	{/if}
</div>
