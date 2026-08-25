<script lang="ts">
	import { onMount } from 'svelte';
	import { projects, type Project } from '$lib/api';

	let {
		project,
		onclose,
		onupdated,
		ondeleted,
	}: {
		project: Project;
		onclose: () => void;
		onupdated: (project: Project) => void;
		ondeleted: (projectId: string) => void;
	} = $props();

	let name = $state('');
	let description = $state('');
	let saving = $state(false);
	let confirmingDelete = $state(false);
	let refreshingDeleteDetails = $state(false);
	let deleting = $state(false);
	let deleteProjectDetails = $state<Project | null>(null);
	let deleteConfirmation = $state('');
	let error = $state('');
	let nameInput = $state<HTMLInputElement>();
	let deleteInput = $state<HTMLInputElement>();

	let deletionProject = $derived(deleteProjectDetails ?? project);
	let deletionCounts = $derived(deletionProject.deletion_counts ?? {
		agents: deletionProject.agent_ids.length,
		tasks: deletionProject.task_count,
		task_messages: 0,
		conversations: deletionProject.conversation_count,
		conversation_messages: 0,
		memory_notes: 0,
		workflow_runs: 0,
		schedules: 0,
	});
	let deleteConfirmed = $derived(deleteConfirmation === deletionProject.name);

	onMount(() => {
		name = project.name;
		description = project.description ?? '';
		nameInput?.focus();
	});

	function close() {
		if (!saving && !deleting) onclose();
	}

	async function save() {
		const trimmedName = name.trim();
		if (!trimmedName || saving) return;
		saving = true;
		error = '';
		try {
			const updated = await projects.update(project.id, {
				name: trimmedName,
				description: description.trim(),
			});
			onupdated(updated);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not update the project.';
		} finally {
			saving = false;
		}
	}

	async function deleteProject() {
		if (deleting || !deleteConfirmed) return;
		deleting = true;
		error = '';
		try {
			await projects.deleteCascade(project.id);
			ondeleted(project.id);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not delete the project.';
			deleting = false;
		}
	}

	async function beginDeleteConfirmation() {
		if (refreshingDeleteDetails || deleting) return;
		refreshingDeleteDetails = true;
		error = '';
		try {
			deleteProjectDetails = await projects.get(project.id);
			deleteConfirmation = '';
			confirmingDelete = true;
			setTimeout(() => deleteInput?.focus());
		} catch (cause) {
			error = cause instanceof Error
				? cause.message
				: 'Could not load current Project deletion details. Try again.';
		} finally {
			refreshingDeleteDetails = false;
		}
	}

	function leaveDeleteConfirmation() {
		confirmingDelete = false;
		deleteProjectDetails = null;
		deleteConfirmation = '';
		error = '';
	}
</script>

<svelte:window onkeydown={(event) => { if (event.key === 'Escape') close(); }} />

<div
	class="fixed inset-0 z-[220] flex items-end justify-center bg-black/60 p-0 backdrop-blur-sm sm:items-center sm:p-4"
	role="presentation"
	onclick={(event) => { if (event.target === event.currentTarget) close(); }}
>
	<div
		class="flex max-h-[90vh] w-full max-w-lg flex-col rounded-t-2xl border border-border bg-card shadow-2xl sm:rounded-2xl"
		role="dialog"
		aria-modal="true"
		aria-labelledby="project-settings-title"
		tabindex="-1"
	>
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<h2 id="project-settings-title" class="text-base font-semibold">
					{confirmingDelete ? 'Delete project?' : 'Project settings'}
				</h2>
				<p class="mt-0.5 text-xs text-muted-foreground">{confirmingDelete ? deletionProject.name : project.name}</p>
			</div>
			<button type="button" onclick={close} disabled={saving || deleting} aria-label="Close project settings" class="flex h-8 w-8 items-center justify-center rounded-md text-xl leading-none text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40">&times;</button>
		</div>

		{#if confirmingDelete}
			<div class="min-h-0 flex-1 space-y-4 overflow-y-auto p-5" aria-busy={deleting}>
				<div class="rounded-xl border border-destructive/30 bg-destructive/5 p-4">
					<p class="text-sm font-medium text-foreground">This permanently deletes “{deletionProject.name}” and cannot be undone.</p>
					<p class="mt-2 text-xs leading-relaxed text-muted-foreground">XpressClaw will cancel active work, stop associated runtimes, and delete:</p>
					<ul class="mt-3 space-y-1.5 text-xs leading-relaxed text-muted-foreground">
						<li><strong class="text-foreground">{deletionCounts.agents}</strong> Agent{deletionCounts.agents === 1 ? '' : 's'}, their configuration, local collaboration access, and XpressClaw runtime containers</li>
						<li><strong class="text-foreground">{deletionCounts.tasks}</strong> task{deletionCounts.tasks === 1 ? '' : 's'} and <strong class="text-foreground">{deletionCounts.task_messages}</strong> task message{deletionCounts.task_messages === 1 ? '' : 's'}, including attachments</li>
						<li><strong class="text-foreground">{deletionCounts.conversations}</strong> conversation{deletionCounts.conversations === 1 ? '' : 's'} and <strong class="text-foreground">{deletionCounts.conversation_messages}</strong> conversation message{deletionCounts.conversation_messages === 1 ? '' : 's'}, including turns and attachments</li>
						<li><strong class="text-foreground">{deletionCounts.memory_notes}</strong> Project memory note{deletionCounts.memory_notes === 1 ? '' : 's'}</li>
						<li><strong class="text-foreground">{deletionCounts.workflow_runs}</strong> Project workflow run{deletionCounts.workflow_runs === 1 ? '' : 's'}, including steps and waits, and <strong class="text-foreground">{deletionCounts.schedules}</strong> owned schedule{deletionCounts.schedules === 1 ? '' : 's'}, plus sync and runtime metadata</li>
					</ul>
					<p class="mt-3 text-xs leading-relaxed text-muted-foreground"><strong class="text-foreground">Kept:</strong> source repositories, workspace folders, and reusable workflow definitions shared outside this Project.</p>
				</div>
				{#if deletionProject.deletion_started_at}
					<p class="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">A previous deletion attempt was interrupted after new work was blocked. Confirm again to safely resume cleanup.</p>
				{/if}
				<div>
					<label for="project-delete-confirmation" class="text-xs font-medium">Type <span class="font-semibold">{deletionProject.name}</span> to confirm</label>
					<input bind:this={deleteInput} id="project-delete-confirmation" bind:value={deleteConfirmation} disabled={deleting} autocomplete="off" spellcheck="false" class="mt-1.5 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-destructive disabled:opacity-50" />
				</div>
				{#if deleting}<p role="status" aria-live="polite" class="text-xs text-muted-foreground">Cancelling active work and removing Project data…</p>{/if}
				{#if error}<p role="alert" class="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p>{/if}
			</div>
			<div class="flex justify-end gap-2 border-t border-border px-5 py-4">
				<button type="button" onclick={leaveDeleteConfirmation} disabled={deleting} class="rounded-lg border border-border px-4 py-2 text-sm hover:bg-accent disabled:opacity-40">Back</button>
				<button type="button" onclick={() => void deleteProject()} disabled={deleting || !deleteConfirmed} class="rounded-lg bg-destructive px-4 py-2 text-sm font-medium text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50">{deleting ? 'Deleting…' : 'Permanently delete project'}</button>
			</div>
		{:else}
			<form onsubmit={(event) => { event.preventDefault(); void save(); }}>
				<div class="space-y-4 p-5">
					<div>
						<label for="project-settings-name" class="text-xs font-medium">Project name</label>
						<input bind:this={nameInput} id="project-settings-name" bind:value={name} autocomplete="off" class="mt-1.5 w-full rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring" />
					</div>
					<div>
						<label for="project-settings-description" class="text-xs font-medium">Description</label>
						<textarea id="project-settings-description" bind:value={description} rows="3" placeholder="What belongs in this project?" class="mt-1.5 w-full resize-y rounded-lg border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring"></textarea>
					</div>
					{#if error}<p role="alert" class="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p>{/if}
				</div>
				<div class="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-4">
					<button type="button" onclick={() => void beginDeleteConfirmation()} disabled={refreshingDeleteDetails} class="rounded-lg px-3 py-2 text-sm font-medium text-destructive hover:bg-destructive/10 disabled:opacity-40">{refreshingDeleteDetails ? 'Checking Project…' : 'Delete project'}</button>
					<div class="flex gap-2">
						<button type="button" onclick={close} disabled={saving} class="rounded-lg border border-border px-4 py-2 text-sm hover:bg-accent disabled:opacity-40">Cancel</button>
						<button type="submit" disabled={saving || !name.trim()} class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-40">{saving ? 'Saving…' : 'Save changes'}</button>
					</div>
				</div>
			</form>
		{/if}
	</div>
</div>
