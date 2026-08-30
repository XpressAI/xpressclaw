<script lang="ts">
	import { workspaces, type LiveConfig, type WorkspaceRepositoryStatus } from '$lib/api';
	import DirectoryPicker from '$lib/components/DirectoryPicker.svelte';

	interface Props {
		agentId: string;
		agentConfig: LiveConfig['agents'][0] | null;
		onSave: (data: Record<string, unknown>) => Promise<void>;
	}

	let { agentId, agentConfig, onSave }: Props = $props();
	let newVolumePath = $state('');
	let saving = $state(false);
	let error = $state('');
	let showFolderPicker = $state(false);
	let repository = $state<WorkspaceRepositoryStatus | null>(null);
	let repositoryLoading = $state(true);
	let repositorySaving = $state(false);
	let repositoryError = $state('');
	let repositoryRequest = 0;

	let rawVolumes = $derived(agentConfig?.volumes ?? []);

	$effect(() => {
		const requestedAgent = agentId;
		void loadRepository(requestedAgent);
	});

	async function loadRepository(requestedAgent: string) {
		const request = ++repositoryRequest;
		repository = null;
		repositoryLoading = true;
		repositorySaving = false;
		repositoryError = '';
		try {
			const result = await workspaces.repository(requestedAgent);
			if (request === repositoryRequest && requestedAgent === agentId) repository = result;
		} catch (cause) {
			if (request === repositoryRequest && requestedAgent === agentId) {
				repositoryError = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (request === repositoryRequest && requestedAgent === agentId) repositoryLoading = false;
		}
	}

	async function refreshRepository() {
		await loadRepository(agentId);
	}

	async function selectRepository(path: string) {
		if (repositorySaving) return;
		const requestedAgent = agentId;
		const request = ++repositoryRequest;
		repositorySaving = true;
		repositoryError = '';
		try {
			const result = await workspaces.selectRepository(requestedAgent, path);
			if (request === repositoryRequest && requestedAgent === agentId) repository = result;
		} catch (cause) {
			if (request === repositoryRequest && requestedAgent === agentId) {
				repositoryError = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (request === repositoryRequest && requestedAgent === agentId) repositorySaving = false;
		}
	}

	async function clearRepository() {
		if (repositorySaving || (!repository?.active && !repository?.selected_relative_path)) return;
		const requestedAgent = agentId;
		const request = ++repositoryRequest;
		repositorySaving = true;
		repositoryError = '';
		try {
			const result = await workspaces.clearRepository(requestedAgent);
			if (request === repositoryRequest && requestedAgent === agentId) repository = result;
		} catch (cause) {
			if (request === repositoryRequest && requestedAgent === agentId) {
				repositoryError = cause instanceof Error ? cause.message : String(cause);
			}
		} finally {
			if (request === repositoryRequest && requestedAgent === agentId) repositorySaving = false;
		}
	}

	function githubLabel(status: WorkspaceRepositoryStatus): string {
		return ({
			attached: 'GitHub MCP attached',
			explicit_override: 'Explicit github MCP',
			non_github_origin: 'Non-GitHub origin',
			missing_credential: 'GitHub credential missing',
			incompatible_image: 'Runner image incompatible',
			unavailable: 'GitHub unavailable',
		} as const)[status.github_status];
	}

	function volumeParts(volume: string): { host: string; container: string } {
		const lastSeparator = volume.lastIndexOf(':');
		const suffix = lastSeparator >= 0 ? volume.slice(lastSeparator + 1) : '';
		const mount = /^(?:ro|rw|z|Z)(?:,(?:ro|rw|z|Z))*$/.test(suffix)
			? volume.slice(0, lastSeparator)
			: volume;
		const separator = mount.lastIndexOf(':');
		if (separator < 0) return { host: volume, container: '' };
		return { host: mount.slice(0, separator), container: mount.slice(separator + 1) };
	}

	async function addVolume() {
		const path = newVolumePath.trim();
		if (!path || saving) return;
		const basename = path.split(/[\\/]/).filter(Boolean).pop() || 'resource';
		await saveVolumes([...rawVolumes, `${path}:/workspace/resources/${basename}:z`]);
		newVolumePath = '';
	}

	async function removeVolume(index: number) {
		if (saving) return;
		await saveVolumes(rawVolumes.filter((_, candidate) => candidate !== index));
	}

	async function saveVolumes(volumes: string[]) {
		saving = true;
		error = '';
		try {
			await onSave({ volumes });
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			saving = false;
		}
	}
</script>

<div class="mx-auto max-w-3xl space-y-6">
	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">Primary workspace</h2>
		<p class="mt-1 text-xs text-muted-foreground">The durable writable boundary mounted at <code>/workspace</code>. Repository adoption never grants access outside it.</p>
		<div class="mt-3 rounded-md border border-border bg-background px-3 py-2 font-mono text-sm">
			{agentConfig?.runner.workspace || 'Using the server default workspace'}
		</div>
	</div>

	<div class="ai-card p-5" data-active-repository>
		<div class="flex flex-wrap items-start justify-between gap-3">
			<div>
				<h2 class="text-sm font-semibold">Active repository</h2>
				<p class="mt-1 text-xs text-muted-foreground">ACP cwd, Git status, Files, and the bundled GitHub MCP follow this repository.</p>
			</div>
			<button type="button" onclick={refreshRepository} disabled={repositoryLoading || repositorySaving} class="rounded-md border border-border px-2.5 py-1.5 text-xs hover:bg-accent disabled:opacity-50">Refresh</button>
		</div>

		{#if repositoryLoading}
			<div class="mt-4 h-20 animate-pulse rounded-lg bg-muted/60" aria-label="Loading repository status"></div>
		{:else if repository}
			<div class="mt-4 rounded-lg border border-border bg-background p-3">
				<div class="flex flex-wrap items-center gap-2">
					<span class="inline-flex items-center gap-1.5 rounded-full border px-2 py-1 text-[11px] {repository.github_status === 'attached' ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400' : 'border-border text-muted-foreground'}">
						<span class="h-1.5 w-1.5 rounded-full {repository.github_status === 'attached' ? 'bg-emerald-500' : 'bg-amber-500'}"></span>
						{githubLabel(repository)}
					</span>
					{#if repository.github_repository}<span class="font-mono text-[11px] text-muted-foreground">{repository.github_repository}</span>{/if}
				</div>
				<div class="mt-3 font-mono text-xs break-all">
					{repository.active?.root ?? 'No active repository'}
				</div>
				<p class="mt-2 text-xs leading-relaxed text-muted-foreground">{repository.message}</p>
				{#if repository.restart_required}
					<p class="mt-2 text-xs text-amber-600 dark:text-amber-400">The current turn keeps its existing process. The next turn starts a fresh session with the pending repository change.</p>
				{/if}
				{#if repository.active || repository.selected_relative_path}
					<button type="button" onclick={clearRepository} disabled={repositorySaving} class="mt-3 text-xs text-destructive hover:underline disabled:opacity-50">Clear active repository</button>
				{/if}
			</div>

			{#if repository.candidates.length > 0}
				<div class="mt-4">
					<div class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Repositories in workspace</div>
					<div class="mt-2 space-y-2">
						{#each repository.candidates as candidate (candidate.relative_path)}
							<div class="flex items-center gap-3 rounded-lg border border-border px-3 py-2">
								<div class="min-w-0 flex-1">
									<div class="truncate font-mono text-xs">{candidate.relative_path}</div>
									<div class="truncate text-[11px] text-muted-foreground">{candidate.github_repository || 'No supported GitHub origin'}</div>
								</div>
								{#if repository.active?.relative_path === candidate.relative_path}
									<span class="text-[11px] font-medium text-primary">Active</span>
								{:else}
									<button type="button" onclick={() => selectRepository(candidate.relative_path)} disabled={repositorySaving} class="rounded-md border border-border px-2.5 py-1 text-xs hover:bg-accent disabled:opacity-50">Use</button>
								{/if}
							</div>
						{/each}
					</div>
				</div>
			{/if}
		{/if}
		{#if repositoryError}<p class="mt-3 text-xs text-destructive">{repositoryError}</p>{/if}
	</div>

	<div class="ai-card p-5">
		<h2 class="text-sm font-semibold">Additional folders</h2>
		<p class="mt-1 text-xs text-muted-foreground">Optional references or sibling repositories mounted below <code>/workspace/resources</code>.</p>

		{#if rawVolumes.length > 0}
			<div class="mt-4 space-y-2">
				{#each rawVolumes as volume, index}
					{@const parts = volumeParts(volume)}
					<div class="flex items-center gap-3 rounded-md border border-border px-3 py-2">
						<span class="min-w-0 flex-1 truncate font-mono text-xs">{parts.host}</span>
						<span class="text-xs text-muted-foreground">→ {parts.container}</span>
						<button onclick={() => removeVolume(index)} disabled={saving} class="text-xs text-destructive hover:underline disabled:opacity-50">Remove</button>
					</div>
				{/each}
			</div>
		{/if}

		<div class="mt-4 flex gap-2">
			<input bind:value={newVolumePath} placeholder="~/projects/shared-library" class="min-w-0 flex-1 rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
			<button onclick={addVolume} disabled={!newVolumePath.trim() || saving} class="rounded-md bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving…' : 'Add folder'}</button>
			<button type="button" onclick={() => (showFolderPicker = true)} disabled={saving} class="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent disabled:opacity-50">Browse…</button>
		</div>
		{#if error}<p class="mt-2 text-xs text-destructive">{error}</p>{/if}
	</div>
</div>

{#if showFolderPicker}
	<DirectoryPicker
		title="Choose additional folder"
		initialPath={newVolumePath || agentConfig?.runner.workspace || ''}
		onclose={() => (showFolderPicker = false)}
		onselect={(path) => {
			newVolumePath = path;
			showFolderPicker = false;
		}}
	/>
{/if}
