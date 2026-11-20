<script lang="ts">
    import { onMount } from 'svelte';
    import { projects, type ProjectStorage, type StorageCleanupResult } from '$lib/api';
    let { projectId, projectName, onclose }: { projectId: string; projectName: string; onclose: () => void } = $props();
    let dialog = $state<HTMLDialogElement>();
    let report = $state<ProjectStorage | null>(null);
    let loading = $state(true);
    let busy = $state(false);
    let saving = $state(false);
    let error = $state('');
    let result = $state<StorageCleanupResult | null>(null);
    let selection = $state<string[]>([]);
    let confirming = $state(false);
    let automatic = $state(false);
    let days = $state(14);
    let selectedTrees = $derived(report?.worktrees.filter(tree => selection.includes(tree.id) && tree.eligible) ?? []);
    let selectedImages = $derived(report?.engine?.recovery_images.filter(image => selection.includes(image.id) && image.eligible) ?? []);
    let selectedBytes = $derived(selectedTrees.reduce((sum, tree) => sum + tree.bytes, 0) + selectedImages.reduce((sum, image) => sum + (image.bytes ?? 0), 0));
    let worktreeBytes = $derived(report?.worktrees.reduce((sum, tree) => sum + tree.bytes, 0) ?? 0);
    function size(bytes: number | null) {
        if (bytes === null) return 'Size unavailable';
        const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']; let index = 0;
        while (bytes >= 1024 && index < units.length - 1) { bytes /= 1024; index += 1; }
        return `${bytes.toFixed(index > 1 ? 1 : 0)} ${units[index]}`;
    }
    function date(timestamp: number) { return new Date(timestamp * 1000).toLocaleDateString(); }
    function toggle(id: string) { selection = selection.includes(id) ? selection.filter(value => value !== id) : [...selection, id]; }
    function close() { if (!busy && !saving) onclose(); }
    async function refresh() {
        loading = true; error = ''; confirming = false; selection = [];
        try { report = await projects.storage(projectId); automatic = report.policy.enabled; days = report.policy.days; }
        catch (cause) { error = String(cause); }
        finally { loading = false; }
    }
    async function savePolicy() {
        saving = true; error = '';
        try { const policy = await projects.saveStoragePolicy(projectId, { enabled: automatic, days }); if (report) report.policy = policy; }
        catch (cause) { error = String(cause); }
        finally { saving = false; }
    }
    async function cleanup() {
        if (busy) return;
        busy = true; error = ''; result = null;
        try {
            result = await projects.cleanupStorage(projectId, {
                worktrees: Object.fromEntries(selectedTrees.map(tree => [tree.id, tree.fingerprint])),
                recovery_images: selectedImages.map(image => image.id),
            });
            await refresh();
        } catch (cause) { error = String(cause); }
        finally { busy = false; confirming = false; }
    }
    onMount(() => { dialog?.showModal(); void refresh(); });
</script>

<dialog bind:this={dialog} onclose={close} oncancel={event => { if (busy || saving) event.preventDefault(); }} aria-label="Project storage and cleanup" class="m-auto w-[min(64rem,calc(100vw-2rem))] max-h-[90vh] overflow-hidden rounded-xl border border-border bg-card p-0 text-foreground shadow-xl backdrop:bg-black/50 open:flex open:flex-col">
    <div class="flex shrink-0 items-center justify-between border-b border-border px-5 py-4">
        <div><h2 class="text-lg font-semibold">Storage &amp; cleanup</h2><p class="text-sm text-muted-foreground">{projectName}</p></div>
        <button type="button" onclick={close} disabled={busy || saving} aria-label="Close storage" class="rounded px-2 py-1 hover:bg-accent disabled:opacity-40">✕</button>
    </div>
    <div class="min-h-0 flex-1 space-y-5 overflow-y-auto p-5">
        {#if error}<p role="alert" class="rounded border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error}</p>{/if}
        {#if result}
            <div role="status" class="rounded border border-border bg-muted/40 p-3 text-sm">
                Removed {result.removed.length} item{result.removed.length === 1 ? '' : 's'}{result.kept.length ? `; kept ${result.kept.length}` : ''}.
                {#each result.kept as item}<p class="mt-1 break-words text-xs text-muted-foreground">{item.reason}</p>{/each}
            </div>
        {/if}
        {#if loading}
            <p role="status" class="text-sm text-muted-foreground">Inspecting project worktrees and container storage…</p>
        {:else if report}
            <div class="grid gap-3 sm:grid-cols-3">
                <div class="rounded-lg border border-border p-3"><p class="text-xs text-muted-foreground">Worktree storage scanned</p><p class="mt-1 text-xl font-semibold">{size(worktreeBytes)}</p><p class="mt-1 text-xs text-muted-foreground">{report.worktrees.length} registered worktrees</p></div>
                <div class="rounded-lg border border-border p-3"><p class="text-xs text-muted-foreground">Eligible worktrees</p><p class="mt-1 text-xl font-semibold">{report.worktrees.filter(tree => tree.eligible).length}</p><button type="button" onclick={() => selection = report?.worktrees.filter(tree => tree.eligible).map(tree => tree.id) ?? []} class="mt-1 text-xs text-primary">Select eligible worktrees</button></div>
                <div class="rounded-lg border border-border p-3"><p class="text-xs text-muted-foreground">Selected for cleanup</p><p class="mt-1 text-xl font-semibold">{size(selectedBytes)}</p><p class="mt-1 text-xs text-muted-foreground">Estimated space · {selection.length} item{selection.length === 1 ? '' : 's'}</p></div>
            </div>
            <section class="rounded-lg border border-border p-4">
                <h3 class="font-medium">Automatic worktree cleanup</h3>
                <p class="mt-1 text-xs leading-relaxed text-muted-foreground">Check hourly for merged, clean worktrees in .worktrees/. Keep active, locked, unmerged worktrees and ignored personal files. Branches and commit history stay in Git.</p>
                <div class="mt-3 flex flex-wrap items-center gap-3">
                    <label class="flex items-center gap-2 text-sm"><input type="checkbox" bind:checked={automatic} disabled={saving || busy} />Enable automatic cleanup</label>
                    <label class="text-sm">Retain eligible worktrees for <input type="number" min="1" max="365" bind:value={days} disabled={saving || busy} class="mx-1 w-16 rounded border border-input bg-background px-2 py-1" aria-label="Retention days" /> days</label>
                    <button type="button" onclick={savePolicy} disabled={saving || busy || !days || days < 1 || days > 365 || (automatic === report.policy.enabled && days === report.policy.days)} class="rounded border border-border px-3 py-1.5 text-sm disabled:opacity-40">{saving ? 'Saving…' : 'Save retention'}</button>
                </div>
                <p class="mt-2 text-xs text-muted-foreground">Retention starts when XpressClaw first observes an eligible worktree. A scan that finds changes resets that clock.</p>
            </section>
            <section>
                <div class="mb-2 flex items-center justify-between"><h3 class="font-medium">Git worktrees</h3><button type="button" onclick={refresh} disabled={busy || saving} class="text-xs text-primary disabled:opacity-40">Refresh scan</button></div>
                <div class="divide-y divide-border overflow-hidden rounded-lg border border-border">
                    {#each report.worktrees as tree (tree.id)}
                        <label class="flex items-start gap-3 p-3" data-storage-worktree={tree.id}>
                            <input type="checkbox" aria-label={`Select worktree ${tree.branch ?? tree.path}`} checked={selection.includes(tree.id)} disabled={!tree.eligible || busy} onchange={() => toggle(tree.id)} class="mt-1" />
                            <span class="min-w-0 flex-1"><span class="block break-words text-sm font-medium">{tree.branch ?? 'Detached HEAD'}{tree.missing ? ' · Missing directory' : ''}</span><span class="block break-all font-mono text-[11px] text-muted-foreground">{tree.path}</span><span class="mt-1 block text-xs text-muted-foreground">{tree.eligible ? `Eligible${tree.eligible_since ? ` since ${date(tree.eligible_since)}` : ''}` : tree.reasons.join(' · ')}</span></span>
                            <span class="shrink-0 text-xs text-muted-foreground">{tree.size_complete ? size(tree.bytes) : tree.bytes ? `≥ ${size(tree.bytes)}` : 'Not measured'}</span>
                        </label>
                    {:else}<p class="p-3 text-sm text-muted-foreground">No worktrees found in this Project's configured repositories.</p>{/each}
                </div>
                {#each report.errors as message}<p class="mt-2 text-xs text-destructive">{message}</p>{/each}
            </section>
            <section>
                <h3 class="font-medium">Agent recovery snapshots</h3>
                <p class="mt-1 text-xs text-muted-foreground">Explicit cleanup only. These snapshots contain old Agent filesystem state. Images used by containers, other images, or Agent settings are kept.</p>
                {#if report.engine}
                    <div class="mt-2 divide-y divide-border overflow-hidden rounded-lg border border-border">
                        {#each report.engine.recovery_images as image (image.id)}
                            <label class="flex items-start gap-3 p-3"><input type="checkbox" aria-label={`Select recovery snapshot ${image.name}`} checked={selection.includes(image.id)} disabled={!image.eligible || busy} onchange={() => toggle(image.id)} class="mt-1" /><span class="min-w-0 flex-1"><span class="block text-sm">{image.agent_id} · {date(image.created_at)}</span><span class="block break-all font-mono text-[11px] text-muted-foreground">{image.name}</span><span class="mt-1 block text-xs text-muted-foreground">{image.eligible ? 'Unused snapshot' : image.reasons.join(' · ')}</span></span><span class="text-xs text-muted-foreground">{size(image.bytes)}</span></label>
                        {:else}<p class="p-3 text-sm text-muted-foreground">No recovery snapshots owned by this Project.</p>{/each}
                    </div>
                    <p class="mt-3 text-xs text-muted-foreground">Across the container engine: {size(report.engine.image_bytes)} in images and {size(report.engine.volume_bytes)} in volumes ({size(report.engine.unused_volume_bytes)} currently unused). Volumes and unassigned images are outside this Project's cleanup.</p>
                {:else if report.engine_error}<p class="mt-2 text-xs text-muted-foreground">{report.engine_error}</p>{/if}
            </section>
            {#if confirming}
                <section role="alert" class="rounded-lg border border-amber-500/40 bg-amber-500/5 p-4 text-sm">
                    Remove {selectedTrees.length} worktree{selectedTrees.length === 1 ? '' : 's'} and {selectedImages.length} recovery snapshot{selectedImages.length === 1 ? '' : 's'}?
                    <p class="mt-1 text-xs text-muted-foreground">Selected worktree directories and their generated caches will be deleted. Git branches are retained. Each item is checked again before removal.</p>
                    <div class="mt-3 flex gap-2"><button type="button" onclick={cleanup} disabled={busy} class="rounded bg-destructive px-3 py-2 text-sm text-destructive-foreground disabled:opacity-40">{busy ? 'Cleaning up…' : 'Confirm cleanup'}</button><button type="button" onclick={() => confirming = false} disabled={busy} class="rounded border border-border px-3 py-2 text-sm">Back</button></div>
                </section>
            {/if}
        {/if}
    </div>
    <div class="flex shrink-0 justify-end gap-2 border-t border-border px-5 py-3">
        <button type="button" onclick={close} disabled={busy || saving} class="rounded border border-border px-3 py-2 text-sm disabled:opacity-40">Close</button>
        {#if !confirming}<button type="button" onclick={() => confirming = true} disabled={loading || busy || saving || selection.length === 0} class="rounded bg-primary px-3 py-2 text-sm text-primary-foreground disabled:opacity-40">Clean up selected</button>{/if}
    </div>
</dialog>
