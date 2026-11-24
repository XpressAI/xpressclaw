<script lang="ts" module>
	import { projects, type Project } from '$lib/api';
	import { PROJECT_MUTATION_EVENT, type ProjectMutation } from '$lib/projectEvents';

	interface CachedProjectEntry {
		project: Project | null;
		deleted: boolean;
		fetchedAt: number;
	}

	const PROJECT_CACHE_TTL_MS = 30_000;
	const projectCache = new Map<string, CachedProjectEntry>();
	const inflightProjectLoads = new Map<string, Promise<Project | null>>();
	const projectListeners = new Map<string, Set<(entry: CachedProjectEntry) => void>>();
	let mutationListenerAttached = false;

	function notifyProjectListeners(projectId: string, entry: CachedProjectEntry) {
		const listeners = projectListeners.get(projectId);
		if (!listeners) return;
		for (const listener of listeners) listener(entry);
	}

	function handleGlobalProjectMutation(event: Event) {
		const mutation = (event as CustomEvent<ProjectMutation>).detail;
		if (!mutation) return;
		if (mutation.kind === 'deleted') {
			const entry: CachedProjectEntry = { project: null, deleted: true, fetchedAt: Date.now() };
			projectCache.set(mutation.projectId, entry);
			notifyProjectListeners(mutation.projectId, entry);
			return;
		}
		const existing = projectCache.get(mutation.project.id);
		if (!existing?.project || mutation.authoritative || existing.project.updated_at < mutation.project.updated_at) {
			const entry: CachedProjectEntry = { project: mutation.project, deleted: false, fetchedAt: Date.now() };
			projectCache.set(mutation.project.id, entry);
			notifyProjectListeners(mutation.project.id, entry);
		}
	}

	function ensureMutationListener() {
		if (mutationListenerAttached || typeof window === 'undefined') return;
		mutationListenerAttached = true;
		window.addEventListener(PROJECT_MUTATION_EVENT, handleGlobalProjectMutation);
	}

	function subscribeProjectLink(projectId: string, listener: (entry: CachedProjectEntry) => void): () => void {
		ensureMutationListener();
		let listeners = projectListeners.get(projectId);
		if (!listeners) {
			listeners = new Set();
			projectListeners.set(projectId, listeners);
		}
		listeners.add(listener);
		return () => {
			listeners.delete(listener);
			if (listeners.size === 0) {
				projectListeners.delete(projectId);
				if (projectListeners.size === 0 && mutationListenerAttached && typeof window !== 'undefined') {
					window.removeEventListener(PROJECT_MUTATION_EVENT, handleGlobalProjectMutation);
					mutationListenerAttached = false;
				}
			}
		};
	}

	function fetchProjectCached(id: string): Promise<Project | null> {
		const cached = projectCache.get(id);
		if (cached && (cached.deleted || Date.now() - cached.fetchedAt < PROJECT_CACHE_TTL_MS)) {
			return Promise.resolve(cached.project);
		}
		const inflight = inflightProjectLoads.get(id);
		if (inflight) return inflight;
		const req = projects.get(id)
			.then((loaded) => {
				const existing = projectCache.get(id);
				if (!existing || (!existing.deleted && (!existing.project || existing.project.updated_at <= loaded.updated_at))) {
					const entry: CachedProjectEntry = { project: loaded, deleted: false, fetchedAt: Date.now() };
					projectCache.set(id, entry);
					notifyProjectListeners(id, entry);
				}
				return projectCache.get(id)?.project ?? loaded;
			})
			.catch((cause) => {
				console.warn(`Failed to load project ${id} for breadcrumbs`, cause);
				return projectCache.get(id)?.project ?? null;
			})
			.finally(() => {
				inflightProjectLoads.delete(id);
			});
		inflightProjectLoads.set(id, req);
		return req;
	}
</script>

<script lang="ts">
	import { collaborationProjectPath } from '$lib/workspace';

	// Breadcrumb link back to the owning Project. Follows renames and deletions
	// published from other views so the label never goes stale.
	let { projectId, separator = false, class: className = '' }: { projectId: string; separator?: boolean; class?: string } = $props();

	let project = $state<Project | null>(null);
	let deleted = $state(false);

	$effect(() => {
		const id = projectId;
		const cached = projectCache.get(id);
		project = cached?.project ?? null;
		deleted = cached?.deleted ?? false;
		let cancelled = false;
		const unsubscribe = subscribeProjectLink(id, (entry) => {
			if (cancelled) return;
			project = entry.project;
			deleted = entry.deleted;
		});
		if (!cached || (!cached.deleted && Date.now() - cached.fetchedAt >= PROJECT_CACHE_TTL_MS)) {
			void fetchProjectCached(id).then((loaded) => {
				if (cancelled) return;
				const latest = projectCache.get(id);
				if (latest) {
					project = latest.project;
					deleted = latest.deleted;
				} else if (loaded) {
					project = loaded;
				}
			});
		}
		return () => {
			cancelled = true;
			unsubscribe();
		};
	});
</script>

{#if !deleted}
	<a href={collaborationProjectPath(projectId)} data-project-link={projectId} title={project?.name ?? 'Project'} class="truncate hover:text-foreground {className}">{project?.name ?? 'Project'}</a>
	{#if separator}<span aria-hidden="true">/</span>{/if}
{/if}
