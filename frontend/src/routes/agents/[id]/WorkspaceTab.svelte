<script lang="ts">
	import type { LiveConfig } from '$lib/api';
	import DirectoryPicker from '$lib/components/DirectoryPicker.svelte';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		onSave: (data: Record<string, unknown>) => Promise<void>;
	}

	let { agentConfig, onSave }: Props = $props();
	let newVolumePath = $state('');
	let saving = $state(false);
	let error = $state('');
	let showFolderPicker = $state(false);

	let rawVolumes = $derived(agentConfig?.volumes ?? []);

	function volumeParts(volume: string): { host: string; container: string } {
		const separator = volume.indexOf(':');
		if (separator < 0) return { host: volume, container: '' };
		return { host: volume.slice(0, separator), container: volume.slice(separator + 1) };
	}

	async function addVolume() {
		const path = newVolumePath.trim();
		if (!path || saving) return;
		const basename = path.split(/[\\/]/).filter(Boolean).pop() || 'resource';
		await saveVolumes([...rawVolumes, `${path}:/workspace/resources/${basename}`]);
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
	<div class="rounded-xl border border-border bg-card p-5">
		<h2 class="text-sm font-semibold">Primary project</h2>
		<p class="mt-1 text-xs text-muted-foreground">Configured in the Runner tab and mounted read-write at <code>/workspace</code>.</p>
		<div class="mt-3 rounded-md border border-border bg-background px-3 py-2 font-mono text-sm">
			{agentConfig?.runner.workspace || 'Using the server default workspace'}
		</div>
	</div>

	<div class="rounded-xl border border-border bg-card p-5">
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
