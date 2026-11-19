<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { projects, type Project } from '$lib/api';
	import { PROJECT_MUTATION_EVENT, type ProjectMutation } from '$lib/projectEvents';
	import { collaborationProjectPath } from '$lib/workspace';

	// Breadcrumb link back to the owning Project. Follows renames and deletions
	// published from other views so the label never goes stale.
	let { projectId, separator = false, class: className = '' }: { projectId: string; separator?: boolean; class?: string } = $props();

	let project = $state<Project | null>(null);
	let deleted = $state(false);

	$effect(() => {
		const id = projectId;
		if (untrack(() => project?.id) === id) return;
		project = null;
		deleted = false;
		let cancelled = false;
		projects.get(id)
			.then((loaded) => { if (!cancelled) project = loaded; })
			.catch((cause) => console.warn(`Failed to load project ${id} for breadcrumbs`, cause));
		return () => { cancelled = true; };
	});

	onMount(() => {
		const handleProjectMutation = (event: Event) => {
			const mutation = (event as CustomEvent<ProjectMutation>).detail;
			if (!mutation) return;
			if (mutation.kind === 'deleted') {
				if (mutation.projectId === projectId) deleted = true;
				return;
			}
			if (mutation.project.id !== projectId) return;
			if (mutation.authoritative || !project || project.updated_at < mutation.project.updated_at) project = mutation.project;
			deleted = false;
		};
		window.addEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
		return () => window.removeEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
	});
</script>

{#if !deleted}
	<a href={collaborationProjectPath(projectId)} data-project-link={projectId} title={project?.name ?? 'Project'} class="truncate hover:text-foreground {className}">{project?.name ?? 'Project'}</a>
	{#if separator}<span aria-hidden="true">/</span>{/if}
{/if}
