<script lang="ts">
	import { onMount } from 'svelte';
	import { settings } from '$lib/api';
	import type { ProjectSyncAction, ProjectSyncStatus } from '$lib/api';
	import { PROJECT_MUTATION_EVENT, type ProjectMutation } from '$lib/projectEvents';
	import { serverTimestampMs } from '$lib/serverTime';

	type SyncOperation = 'fetch' | 'publish';
	type Notice = { tone: 'success' | 'error'; message: string };

	let projects = $state<ProjectSyncStatus[]>([]);
	let loading = $state(true);
	let loadError = $state('');
	let active = $state<{ projectId: string; operation: SyncOperation } | null>(null);
	let notices = $state<Record<string, Notice>>({});
	let forceAvailable = $state<Record<string, boolean>>({});
	let loadVersion = 0;

	onMount(() => {
		window.addEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
		void load();
		return () => window.removeEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
	});

	async function load(showLoading = true) {
		const version = ++loadVersion;
		if (showLoading) loading = true;
		loadError = '';
		try {
			const loadedProjects = (await settings.listProjectSync()).projects;
			if (version === loadVersion) projects = loadedProjects;
		} catch (reason) {
			if (version === loadVersion) loadError = messageFrom(reason, 'Could not load Project sync settings');
		} finally {
			if (version === loadVersion) loading = false;
		}
	}

	function handleProjectMutation(event: Event) {
		const mutation = (event as CustomEvent<ProjectMutation>).detail;
		if (mutation?.kind === 'updated') {
			projects = projects.map((project) => project.project_id === mutation.project.id
				? { ...project, project_name: mutation.project.name, project_icon: mutation.project.icon }
				: project);
			void load(false);
			return;
		}
		if (mutation?.kind !== 'deleted') return;
		projects = projects.filter((project) => project.project_id !== mutation.projectId);
		notices = withoutKey(notices, mutation.projectId);
		forceAvailable = withoutKey(forceAvailable, mutation.projectId);
		void load(false);
	}

	async function run(project: ProjectSyncStatus, operation: SyncOperation, force = false) {
		if (active || project.status !== 'ready') return;
		active = { projectId: project.project_id, operation };
		notices = withoutKey(notices, project.project_id);
		forceAvailable = withoutKey(forceAvailable, project.project_id);
		try {
			const result = operation === 'fetch'
				? await settings.fetchProject(project.project_id, force)
				: await settings.publishProject(project.project_id);
			notices = {
				...notices,
				[project.project_id]: { tone: 'success', message: outcomeMessage(result) },
			};
			await load(false);
		} catch (reason) {
			const message = messageFrom(reason, `Could not ${operation} ${project.project_name}`);
			notices = { ...notices, [project.project_id]: { tone: 'error', message } };
			if (operation === 'fetch' && needsForceAcknowledgement(message)) {
				forceAvailable = { ...forceAvailable, [project.project_id]: true };
			}
		} finally {
			active = null;
		}
	}

	function withoutKey<T>(record: Record<string, T>, key: string): Record<string, T> {
		return Object.fromEntries(Object.entries(record).filter(([entry]) => entry !== key));
	}

	function messageFrom(reason: unknown, fallback: string): string {
		return reason instanceof Error ? reason.message : fallback;
	}

	function needsForceAcknowledgement(message: string): boolean {
		return message.includes('rerun with --force') || message.includes('first fetch for a populated local Project');
	}

	function outcomeMessage(result: ProjectSyncAction): string {
		const verb = result.action === 'fetch' ? 'Fetched' : 'Published';
		const { counts } = result;
		return `${verb} ${countLabel(counts.agents, 'Agent')}, ${countLabel(counts.tasks, 'task')}, ${countLabel(counts.conversations, 'Conversation')}, and ${countLabel(counts.workflows, 'workflow')} at ${shortCommit(result.commit)}.`;
	}

	function countLabel(count: number, singular: string): string {
		return `${count} ${singular}${count === 1 ? '' : 's'}`;
	}

	function shortCommit(commit: string): string {
		return commit.slice(0, 8);
	}

	function syncTime(value: string | null): string {
		if (!value) return 'Never';
		const parsed = serverTimestampMs(value);
		return parsed === null ? value : new Date(parsed).toLocaleString();
	}

	function statusLabel(status: ProjectSyncStatus['status']): string {
		if (status === 'ready') return 'Ready';
		if (status === 'unconfigured') return 'Needs setup';
		if (status === 'unavailable') return 'No workspace';
		if (status === 'conflict') return 'Configuration conflict';
		return 'Needs attention';
	}

	function statusTone(status: ProjectSyncStatus['status']): string {
		if (status === 'ready') return 'border-emerald-500/25 bg-emerald-500/10 text-emerald-600';
		if (status === 'error' || status === 'conflict') return 'border-destructive/25 bg-destructive/5 text-destructive';
		return 'border-amber-500/25 bg-amber-500/10 text-amber-600';
	}
</script>

<div class="space-y-6 p-4 sm:p-6">
	<div>
		<h1 class="text-2xl font-bold">Project sync</h1>
		<p class="mt-1 max-w-2xl text-sm text-muted-foreground">Fetch or publish portable Project data through the Git store configured in each Project's <code class="font-mono text-xs">.xpressclaw.yml</code>. Matching copies across assigned clones and worktrees are treated as one configuration.</p>
	</div>

	<div class="flex items-start gap-3 rounded-xl border border-border bg-muted/35 px-4 py-3">
		<svg class="mt-0.5 h-4 w-4 shrink-0 text-primary" fill="none" stroke="currentColor" stroke-width="1.7" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M12 3v12m0 0 4-4m-4 4-4-4M5 21h14"/></svg>
		<div class="min-w-0">
			<p class="text-xs font-medium text-foreground">Sync stays explicit</p>
			<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">XpressClaw never fetches or publishes in the background. Wait for active tasks, Conversation turns, and workflows to finish first. Git uses the credentials available to this control-plane process. Only manifest copies whose effective settings disagree require attention.</p>
		</div>
	</div>

	{#if loadError}
		<div role="alert" class="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive">
			{loadError}
			<button type="button" onclick={() => load()} class="ml-2 font-medium underline underline-offset-2">Try again</button>
		</div>
	{/if}

	<div class="overflow-hidden rounded-xl border border-border bg-card" data-project-sync-list>
		{#if loading}
			<div class="flex items-center justify-center gap-2 px-4 py-14 text-sm text-muted-foreground">
				<span class="h-4 w-4 animate-spin rounded-full border-2 border-primary border-t-transparent"></span>
				Loading Projects…
			</div>
		{:else if projects.length === 0}
			<div class="px-4 py-14 text-center">
				<p class="text-sm font-medium text-foreground">No Projects yet</p>
				<p class="mt-1 text-xs text-muted-foreground">Create a Project and assign an Agent before configuring synchronization.</p>
			</div>
		{:else}
			<div class="divide-y divide-border">
				{#each projects as project (project.project_id)}
					<div class="p-4 sm:p-5" data-project-sync={project.project_id}>
						<div class="flex flex-col gap-4 lg:flex-row lg:items-start">
							<div class="flex min-w-0 flex-1 items-start gap-3">
								<span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-sm font-semibold text-primary">
									{project.project_icon || project.project_name.slice(0, 1).toUpperCase()}
								</span>
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-2">
										<h2 class="truncate text-sm font-semibold text-foreground">{project.project_name}</h2>
										<span class="rounded-full border px-2 py-0.5 text-[10px] font-medium {statusTone(project.status)}">{statusLabel(project.status)}</span>
									</div>
									{#if project.status === 'ready'}
										<p class="mt-1 break-all font-mono text-[11px] text-muted-foreground">{project.remote}</p>
									{:else}
										<p class="mt-1 whitespace-pre-line text-xs leading-relaxed text-muted-foreground">{project.message}</p>
									{/if}
								</div>
							</div>

							<div class="flex shrink-0 gap-2 sm:self-start">
								<button
									type="button"
									onclick={() => run(project, 'fetch')}
									disabled={active !== null || project.status !== 'ready'}
									class="flex min-w-24 flex-1 items-center justify-center gap-1.5 rounded-lg border border-border bg-background px-3 py-2 text-xs font-medium hover:bg-accent disabled:cursor-not-allowed disabled:opacity-45 sm:flex-none"
								>
									{#if active?.projectId === project.project_id && active.operation === 'fetch'}<span class="h-3 w-3 animate-spin rounded-full border-2 border-current border-t-transparent"></span>{/if}
									{active?.projectId === project.project_id && active.operation === 'fetch' ? 'Fetching…' : 'Fetch'}
								</button>
								<button
									type="button"
									onclick={() => run(project, 'publish')}
									disabled={active !== null || project.status !== 'ready'}
									class="flex min-w-24 flex-1 items-center justify-center gap-1.5 rounded-lg bg-primary px-3 py-2 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-45 sm:flex-none"
								>
									{#if active?.projectId === project.project_id && active.operation === 'publish'}<span class="h-3 w-3 animate-spin rounded-full border-2 border-current border-t-transparent"></span>{/if}
									{active?.projectId === project.project_id && active.operation === 'publish' ? 'Publishing…' : 'Publish'}
								</button>
							</div>
						</div>

						{#if project.warnings.length > 0}
							<div data-project-sync-warnings role="status" class="mt-4 rounded-lg border border-amber-500/25 bg-amber-500/5 px-3 py-2.5 text-xs text-amber-700 dark:text-amber-400">
								<p class="font-medium">Some assigned workspaces were ignored</p>
								<ul class="mt-1 list-disc space-y-1 pl-4 text-muted-foreground">
									{#each project.warnings as warning}
										<li class="break-words">{warning}</li>
									{/each}
								</ul>
							</div>
						{/if}

						{#if project.status === 'ready'}
							<dl class="mt-4 grid gap-x-6 gap-y-3 border-t border-border/70 pt-4 text-xs sm:grid-cols-2 xl:grid-cols-3">
								<div class="min-w-0"><dt class="text-muted-foreground">Branch</dt><dd class="mt-0.5 truncate font-mono text-foreground">{project.branch}</dd></div>
								<div class="min-w-0"><dt class="text-muted-foreground">Store path</dt><dd class="mt-0.5 truncate font-mono text-foreground">{project.store_path}</dd></div>
								<div class="min-w-0"><dt class="text-muted-foreground">Project memory</dt><dd class="mt-0.5 text-foreground">{project.share_project_memory ? 'Included' : 'Local only'}</dd></div>
								<div class="min-w-0"><dt class="text-muted-foreground">Last sync</dt><dd class="mt-0.5 text-foreground">{syncTime(project.last_synced_at)}</dd></div>
								<div class="min-w-0"><dt class="text-muted-foreground">Commit</dt><dd class="mt-0.5 font-mono text-foreground">{project.last_commit ? shortCommit(project.last_commit) : '—'}</dd></div>
								<div class="min-w-0"><dt class="text-muted-foreground">Workspace</dt><dd class="mt-0.5 truncate font-mono text-[11px] text-foreground" title={project.project_dir ?? undefined}>{project.project_dir}</dd></div>
							</dl>
						{/if}

						{#if notices[project.project_id]}
							{@const notice = notices[project.project_id]}
							<div aria-live="polite" class="mt-4 flex flex-col gap-3 rounded-lg border px-3 py-2.5 text-xs sm:flex-row sm:items-center sm:justify-between {notice.tone === 'success' ? 'border-emerald-500/25 bg-emerald-500/10 text-emerald-600' : 'border-destructive/25 bg-destructive/5 text-destructive'}">
								<span>{notice.message}</span>
								{#if forceAvailable[project.project_id]}
									<button type="button" onclick={() => run(project, 'fetch', true)} disabled={active !== null} class="shrink-0 rounded-md border border-current/25 bg-background/60 px-2.5 py-1.5 font-medium hover:bg-background disabled:opacity-50">Merge remote changes</button>
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
