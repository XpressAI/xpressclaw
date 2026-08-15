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
	let deleting = $state(false);
	let error = $state('');
	let nameInput = $state<HTMLInputElement>();

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
		if (deleting) return;
		deleting = true;
		error = '';
		try {
			await projects.delete(project.id);
			ondeleted(project.id);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not delete the project.';
			deleting = false;
		}
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
				<p class="mt-0.5 text-xs text-muted-foreground">{project.name}</p>
			</div>
			<button type="button" onclick={close} disabled={saving || deleting} aria-label="Close project settings" class="flex h-8 w-8 items-center justify-center rounded-md text-xl leading-none text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40">&times;</button>
		</div>

		{#if confirmingDelete}
			<div class="space-y-4 p-5">
				<div class="rounded-xl border border-destructive/30 bg-destructive/5 p-4">
					<p class="text-sm font-medium text-foreground">This permanently deletes “{project.name}”.</p>
					<p class="mt-2 text-xs leading-relaxed text-muted-foreground">Only empty projects can be deleted. Move or remove its Agents, conversations, and tasks, and finish or cancel active workflows first. Project memory is also removed.</p>
				</div>
				{#if error}<p role="alert" class="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p>{/if}
			</div>
			<div class="flex justify-end gap-2 border-t border-border px-5 py-4">
				<button type="button" onclick={() => { confirmingDelete = false; error = ''; }} disabled={deleting} class="rounded-lg border border-border px-4 py-2 text-sm hover:bg-accent disabled:opacity-40">Back</button>
				<button type="button" onclick={() => void deleteProject()} disabled={deleting} class="rounded-lg bg-destructive px-4 py-2 text-sm font-medium text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50">{deleting ? 'Deleting…' : 'Delete project'}</button>
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
					<button type="button" onclick={() => { confirmingDelete = true; error = ''; }} class="rounded-lg px-3 py-2 text-sm font-medium text-destructive hover:bg-destructive/10">Delete project</button>
					<div class="flex gap-2">
						<button type="button" onclick={close} disabled={saving} class="rounded-lg border border-border px-4 py-2 text-sm hover:bg-accent disabled:opacity-40">Cancel</button>
						<button type="submit" disabled={saving || !name.trim()} class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-40">{saving ? 'Saving…' : 'Save changes'}</button>
					</div>
				</div>
			</form>
		{/if}
	</div>
</div>
