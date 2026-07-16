<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { setup } from '$lib/api';
	import { openExternal } from '$lib/utils';
	import type { DockerStatus } from '$lib/api';

	const runnerOptions = [
		{
			kind: 'codex',
			name: 'Codex',
			mark: 'C',
			description: 'Codex CLI using an eligible ChatGPT subscription login.',
			image: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest'
		},
		{
			kind: 'claude',
			name: 'Claude Code',
			mark: 'A',
			description: 'Claude Code using an eligible Claude subscription login.',
			image: 'ghcr.io/xpressai/xpressclaw-runner-claude:latest'
		},
		{
			kind: 'opencode',
			name: 'OpenCode',
			mark: 'O',
			description: 'OpenCode with its native JSON event stream.',
			image: 'ghcr.io/xpressai/xpressclaw-runner-opencode:latest'
		}
	] as const;

	let isAddSession = $derived(
		['add-session', 'add-agent'].includes($page.url.searchParams.get('mode') ?? '')
	);
	let runnerKind = $state('codex');
	let runnerImage = $state(runnerOptions[0].image as string);
	let subscriptionAuth = $state(true);
	let workspacePath = $state('');
	let workspaceFolders = $state<string[]>([]);
	let newFolderPath = $state('');
	let dockerStatus = $state<DockerStatus | null>(null);
	let dockerLoading = $state(true);
	let saving = $state(false);
	let saveError = $state('');
	let contextLabel = $derived(
		workspacePath.split('/').filter(Boolean).pop() || runnerKind
	);

	onMount(async () => {
		// Keep old bookmarks working while making the native-session URL canonical.
		if ($page.url.searchParams.get('mode') === 'add-agent') {
			await goto('/setup?mode=add-session', { replaceState: true });
		}

		try {
			const info = await setup.systemInfo();
			workspacePath = info.working_directory ?? '';
		} catch {
			workspacePath = '';
		}
		await recheckDocker();
	});

	function selectRunner(kind: string) {
		const runner = runnerOptions.find((option) => option.kind === kind);
		if (!runner) return;
		runnerKind = runner.kind;
		runnerImage = runner.image;
	}

	function addFolder() {
		const folder = newFolderPath.trim();
		if (!folder || workspaceFolders.includes(folder)) return;
		workspaceFolders = [...workspaceFolders, folder];
		newFolderPath = '';
	}

	async function recheckDocker() {
		dockerLoading = true;
		try {
			dockerStatus = await setup.checkDocker();
		} catch {
			dockerStatus = { available: false, error: 'Could not check the container runtime' };
		}
		dockerLoading = false;
	}

	function additionalVolumes(): string[] {
		return workspaceFolders.map((folder) => {
			const basename = folder.split('/').filter(Boolean).pop() || 'shared';
			return `${folder}:/workspace/${basename}`;
		});
	}

	async function createSession() {
		if (!workspacePath.trim() || !runnerImage.trim() || saving) return;
		saving = true;
		saveError = '';

		const session = {
			backend: runnerKind,
			runner_kind: runnerKind,
			runner_image: runnerImage.trim(),
			runner_workspace: workspacePath.trim(),
			subscription_auth: subscriptionAuth,
			volumes: additionalVolumes()
		};

		try {
			if (isAddSession) {
				const created = await setup.addSession(session);
				await goto(`/agents/${created.session_id}`);
			} else {
				await setup.complete({
					agents: [session],
					isolation: 'docker'
				});
				await goto('/');
			}
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Could not create the project';
		} finally {
			saving = false;
		}
	}
</script>

<div class="rounded-2xl border border-border bg-card shadow-sm">
	<div class="flex items-start justify-between border-b border-border px-4 py-5 sm:px-6">
		<div>
			<p class="text-xs font-medium uppercase tracking-wider text-primary">Project</p>
			<h2 class="mt-1 text-xl font-semibold text-foreground">
				{isAddSession ? 'Connect a project' : 'Connect your first project'}
			</h2>
			<p class="mt-1 text-sm text-muted-foreground">
				Choose the agent you already use, then point it at a folder on this computer.
			</p>
		</div>
		{#if isAddSession}
			<button
				type="button"
				onclick={() => goto('/agents')}
				aria-label="Cancel"
				class="ml-4 rounded-md p-2 text-xl leading-none text-muted-foreground hover:bg-accent hover:text-foreground"
			>&times;</button>
		{/if}
	</div>

	<form class="space-y-7 p-4 sm:p-6" onsubmit={(event) => { event.preventDefault(); createSession(); }}>
		<section>
			<div class="mb-3">
				<h3 class="text-sm font-medium text-foreground">Agent</h3>
				<p class="mt-0.5 text-xs text-muted-foreground">XpressClaw sends tasks to this product; it keeps its own reasoning, tools, and subagents.</p>
			</div>
			<div class="grid gap-3 sm:grid-cols-3">
				{#each runnerOptions as runner}
					<button
						type="button"
						onclick={() => selectRunner(runner.kind)}
						class="rounded-xl border p-4 text-left transition-colors {runnerKind === runner.kind
							? 'border-primary bg-primary/5 ring-1 ring-primary/20'
							: 'border-border hover:border-primary/40 hover:bg-accent/30'}"
					>
						<span class="flex h-9 w-9 items-center justify-center rounded-lg bg-muted text-sm font-semibold text-foreground">{runner.mark}</span>
						<span class="mt-3 block text-sm font-medium text-foreground">{runner.name}</span>
						<span class="mt-1 block text-xs leading-relaxed text-muted-foreground">{runner.description}</span>
					</button>
				{/each}
			</div>
		</section>

		<section>
			<label for="workspace-path" class="mb-1.5 block text-sm font-medium text-foreground">Project folder</label>
			<input
				id="workspace-path"
				type="text"
				bind:value={workspacePath}
				placeholder="/home/me/projects/my-app"
				class="w-full rounded-lg border border-input bg-background px-3.5 py-2.5 font-mono text-sm focus:outline-none focus:ring-2 focus:ring-ring"
			/>
			<p class="mt-1.5 text-xs text-muted-foreground">
				The folder must exist on this machine. It is mounted at <code>/workspace</code> and appears as <strong>{contextLabel}</strong> in the UI.
			</p>
		</section>

		<section class="rounded-xl border border-border bg-muted/20 p-4">
			<label class="flex cursor-pointer items-start gap-3">
				<input type="checkbox" bind:checked={subscriptionAuth} class="mt-0.5 rounded border-border" />
				<span>
					<span class="block text-sm font-medium text-foreground">Use my existing {runnerOptions.find((runner) => runner.kind === runnerKind)?.name} login</span>
					<span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">Mount the CLI's standard login directory so its subscription and conversations can continue across tasks. Only enable this for images you trust.</span>
				</span>
			</label>
		</section>

		<details class="group rounded-xl border border-border">
			<summary class="cursor-pointer list-none px-4 py-3 text-sm font-medium text-foreground">
				<span class="inline-flex items-center gap-2">
					<span class="text-muted-foreground transition-transform group-open:rotate-90">&#9656;</span>
					Advanced container options
				</span>
			</summary>
			<div class="space-y-5 border-t border-border px-4 py-4">
				<div>
					<label for="runner-image" class="mb-1 block text-xs font-medium text-foreground">Runner image</label>
					<input
						id="runner-image"
						type="text"
						bind:value={runnerImage}
						class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
					/>
					<p class="mt-1 text-xs text-muted-foreground">Use the minimal agent image or a compatible derivative. Richer development environments can be attached separately.</p>
				</div>

				<div>
					<label for="additional-folder" class="mb-1 block text-xs font-medium text-foreground">Additional folder mounts</label>
					{#if workspaceFolders.length > 0}
						<div class="mb-2 space-y-2">
							{#each workspaceFolders as folder, index}
								<div class="flex items-center gap-2 rounded-md border border-border bg-background px-3 py-2">
									<span class="min-w-0 flex-1 truncate font-mono text-xs">{folder}</span>
									<button
										type="button"
										onclick={() => { workspaceFolders = workspaceFolders.filter((_, itemIndex) => itemIndex !== index); }}
										aria-label={`Remove ${folder}`}
										class="text-muted-foreground hover:text-destructive"
									>&times;</button>
								</div>
							{/each}
						</div>
					{/if}
					<div class="flex gap-2">
						<input
							id="additional-folder"
							type="text"
							bind:value={newFolderPath}
							onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); addFolder(); } }}
							placeholder="/home/me/projects/shared"
							class="min-w-0 flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus:outline-none focus:ring-2 focus:ring-ring"
						/>
						<button type="button" onclick={addFolder} disabled={!newFolderPath.trim()}
							class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50">Add</button>
					</div>
				</div>
			</div>
		</details>

		<section>
			{#if dockerLoading}
				<div class="flex items-center gap-3 rounded-lg border border-border px-4 py-3">
					<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
					<span class="text-xs text-muted-foreground">Checking Docker or Podman...</span>
				</div>
			{:else if dockerStatus?.available}
				<div class="flex items-center gap-3 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-4 py-3">
					<span class="flex h-6 w-6 items-center justify-center rounded-full bg-emerald-500/15 text-xs text-emerald-500">&#10003;</span>
					<div>
						<p class="text-sm font-medium text-foreground">Container runtime ready</p>
						<p class="text-xs text-muted-foreground">The agent image will be checked after the project is created.</p>
					</div>
				</div>
			{:else}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 px-4 py-3">
					<div class="flex items-start justify-between gap-4">
						<div>
							<p class="text-sm font-medium text-foreground">Container runtime not available</p>
							<p class="mt-0.5 text-xs text-muted-foreground">You can save the project, but work will wait until Docker or Podman is running.</p>
						</div>
						<button type="button" onclick={recheckDocker} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent">Retry</button>
					</div>
					<div class="mt-3 flex gap-3 text-xs">
						<button type="button" onclick={() => openExternal('https://docs.docker.com/get-docker/')} class="text-primary hover:underline">Docker setup</button>
						<button type="button" onclick={() => openExternal('https://podman.io/getting-started/installation')} class="text-primary hover:underline">Podman setup</button>
					</div>
				</div>
			{/if}
		</section>

		{#if saveError}
			<p class="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">{saveError}</p>
		{/if}

		<div class="flex items-center justify-between border-t border-border pt-5">
			{#if isAddSession}
				<button type="button" onclick={() => goto('/agents')} class="rounded-lg px-4 py-2 text-sm text-muted-foreground hover:bg-accent hover:text-foreground">Cancel</button>
			{:else}
				<span class="text-xs text-muted-foreground">No API key required when using a subscription login.</span>
			{/if}
			<button
				type="submit"
				disabled={saving || !workspacePath.trim() || !runnerImage.trim()}
				class="rounded-lg bg-primary px-5 py-2.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-50"
			>
				{saving ? 'Creating...' : (isAddSession ? 'Create project' : 'Finish setup')}
			</button>
		</div>
	</form>
</div>
