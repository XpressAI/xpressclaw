<script lang="ts">
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { projects, type Project } from '$lib/api';
	import { PROJECT_MUTATION_EVENT, sortProjectsByRecency, type ProjectMutation } from '$lib/projectEvents';
	import { timeAgo } from '$lib/utils';
	import AgentLoading from '$lib/components/AgentLoading.svelte';

	let projectList = $state<Project[]>([]);
	let loading = $state(true);
	let creating = $state(false);
	let name = $state('');
	let description = $state('');
	let error = $state('');
	const projectMutations = new Map<string, ProjectMutation>();

	onMount(() => {
		const handleProjectMutation = (event: Event) => {
			const mutation = (event as CustomEvent<ProjectMutation>).detail;
			if (!mutation || (mutation.kind !== 'updated' && mutation.kind !== 'deleted')) return;
			const projectId = mutation.kind === 'updated' ? mutation.project.id : mutation.projectId;
			projectMutations.set(projectId, mutation);
			projectList = applyProjectMutation(projectList, mutation);
		};

		window.addEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
		void load();
		return () => window.removeEventListener(PROJECT_MUTATION_EVENT, handleProjectMutation);
	});

	function applyProjectMutation(list: Project[], mutation: ProjectMutation): Project[] {
		if (mutation.kind === 'deleted') {
			return list.filter((project) => project.id !== mutation.projectId);
		}
		return sortProjectsByRecency(
			list.map((project) => project.id === mutation.project.id ? mutation.project : project),
		);
	}

	async function load() {
		const loadedProjects = await projects.list().catch(() => null);
		if (loadedProjects) {
			projectList = [...projectMutations.values()].reduce(applyProjectMutation, loadedProjects);
		}
		loading = false;
	}

	async function createProject() {
		if (!name.trim()) return;
		error = '';
		try {
			const project = await projects.create({ name: name.trim(), description: description.trim() || undefined });
			await goto(`/projects/${encodeURIComponent(project.id)}`);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not create the project.';
		}
	}
</script>

<div data-projects-scroll class="workspace-scroll-y h-full">
	<div class="mx-auto max-w-6xl space-y-6 p-4 sm:p-6">
		<header class="flex items-start justify-between gap-4">
			<div>
				<h1 class="text-2xl font-bold">Projects</h1>
				<p class="mt-1 text-sm text-muted-foreground">Keep related conversations, Agents, tasks, memory, and workspaces together.</p>
			</div>
			<button type="button" onclick={() => (creating = !creating)} class="shrink-0 rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">+ Project</button>
		</header>

		{#if creating}
			<form onsubmit={(event) => { event.preventDefault(); void createProject(); }} class="ai-card p-4">
				<div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto]">
					<input bind:value={name} placeholder="Project name" class="rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring" />
					<input bind:value={description} placeholder="What belongs in this project? (optional)" class="rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring" />
					<button disabled={!name.trim()} class="rounded-lg bg-primary px-4 py-2 text-sm text-primary-foreground disabled:opacity-40">Create</button>
				</div>
				{#if error}<p class="mt-2 text-xs text-destructive">{error}</p>{/if}
			</form>
		{/if}

		{#if loading}
			<div class="flex justify-center py-12"><AgentLoading label="Loading projects" phase="loading" /></div>
		{:else if projectList.length === 0}
			<div class="rounded-xl border border-dashed border-border p-12 text-center">
				<h2 class="font-semibold">Create your first project</h2>
				<p class="mt-2 text-sm text-muted-foreground">A Project is the shared context around one product, repository, or goal.</p>
			</div>
		{:else}
			<div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
				{#each projectList as project (project.id)}
					<a href="/projects/{encodeURIComponent(project.id)}" class="group ai-card p-5 transition hover:-translate-y-0.5 hover:bg-[hsl(var(--hover))]">
						<div class="flex items-start gap-3">
							<span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-lg text-primary">{project.icon || project.name.slice(0, 1).toUpperCase()}</span>
							<div class="min-w-0 flex-1">
								<h2 class="truncate font-semibold group-hover:text-primary">{project.name}</h2>
								<p class="mt-1 line-clamp-2 min-h-10 text-sm text-muted-foreground">{project.description || 'Conversations and work for this project.'}</p>
							</div>
						</div>
						<div class="mt-5 grid grid-cols-3 gap-2 text-center text-xs text-muted-foreground">
							<div class="rounded-lg bg-[hsl(var(--inset))] p-2 shadow-[var(--shadow-hairline)]"><strong class="block text-sm text-foreground">{project.conversation_count}</strong>Conversations</div>
							<div class="rounded-lg bg-[hsl(var(--inset))] p-2 shadow-[var(--shadow-hairline)]"><strong class="block text-sm text-foreground">{project.agent_ids.length}</strong>Agents</div>
							<div class="rounded-lg bg-[hsl(var(--inset))] p-2 shadow-[var(--shadow-hairline)]"><strong class="block text-sm text-foreground">{project.task_count}</strong>Tasks</div>
						</div>
						<p class="mt-3 text-[11px] text-muted-foreground">Updated {timeAgo(project.updated_at)}</p>
					</a>
				{/each}
			</div>
		{/if}
	</div>
</div>
