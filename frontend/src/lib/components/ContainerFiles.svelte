<script lang="ts">
 import { onMount, onDestroy } from 'svelte';
 import { environments, type WorkspaceEntry, type WorkspaceFile } from '$lib/api';
 import MonacoEditor from './MonacoEditor.svelte';
 let { agentId, showTree = true, onDirtyChange = () => {} }: { agentId: string; showTree?: boolean; onDirtyChange?: (dirty: boolean) => void } = $props();
 let directory = $state('/tmp');
 let location = $state('/tmp');
 let entries = $state<WorkspaceEntry[]>([]);
 let file = $state<WorkspaceFile | null>(null);
 let content = $state('');
 let error = $state('');
 let busy = $state(false);
 let truncated = $state(false);
 let sequence = 0;
 let dirty = $derived(Boolean(file && file.content !== content));
 $effect(() => onDirtyChange(dirty));
 onDestroy(() => onDirtyChange(false));
 onMount(() => { void browse('/tmp'); });
 export function refresh() { void browse(directory); }
 function mayNavigate() { return !dirty || window.confirm('Discard the unsaved changes in the current file?'); }
 async function browse(path: string) {
  if (!mayNavigate()) return;
  const request = ++sequence; busy = true; error = '';
  try {
   const result = await environments.tree(agentId, path);
   if (request !== sequence) return;
   directory = result.path; location = result.path; entries = result.entries; truncated = result.truncated; file = null; content = '';
  } catch (cause) { if (request === sequence) error = String(cause); }
  finally { if (request === sequence) busy = false; }
 }
 async function open(path: string) {
  if (!mayNavigate()) return;
  const request = ++sequence; busy = true; error = '';
  try { const result = await environments.readFile(agentId, path); if (request === sequence) { file = result; content = result.content; } }
  catch (cause) { if (request === sequence) error = String(cause); }
  finally { if (request === sequence) busy = false; }
 }
 async function save() {
  if (!file || !dirty || busy) return;
  busy = true; error = '';
  const submitted = { ...file, content };
  try { const saved = await environments.saveFile(agentId, submitted); if (file?.path === submitted.path) file = saved; }
  catch (cause) { error = String(cause); }
  finally { busy = false; }
 }
</script>

<div class="flex min-h-0 flex-1 flex-col" data-container-files>
 <form class="flex flex-wrap items-center gap-2 border-b border-border p-2" onsubmit={(event) => { event.preventDefault(); void browse(location); }}>
  <button type="button" onclick={() => browse(directory.slice(0, directory.lastIndexOf('/')) || '/')} disabled={directory === '/'} class="rounded border border-border px-2 py-1 text-xs">Up</button>
  <input aria-label="Container directory" bind:value={location} class="min-w-32 flex-1 rounded border border-input bg-background px-2 py-1 font-mono text-xs" />
  <button class="rounded border border-border px-2 py-1 text-xs">Open folder</button>
  <a href={environments.downloadUrl(agentId, directory)} download class="rounded border border-border px-2 py-1 text-xs">Download folder</a>
 </form>
 {#if error}<p role="alert" class="border-b border-destructive/30 p-2 text-xs text-destructive">{error}</p>{/if}
 <div class="flex min-h-0 flex-1 flex-col md:flex-row">
  {#if showTree}<div class="max-h-64 overflow-auto border-r border-border md:max-h-none md:w-72 md:shrink-0">
   {#each entries as entry (entry.path)}
    <div class="flex items-center gap-1 px-2 py-1 text-xs hover:bg-accent">
     <button type="button" disabled={entry.kind !== 'file' && entry.kind !== 'directory'} onclick={() => entry.kind === 'directory' ? browse(entry.path) : open(entry.path)} class="min-w-0 flex-1 truncate text-left disabled:opacity-50" title={entry.path}>{entry.kind === 'directory' ? '▸' : '▧'} {entry.name}</button>
     {#if entry.kind === 'file' || entry.kind === 'directory'}<a href={environments.downloadUrl(agentId, entry.path)} download aria-label="Download {entry.name}" title="Download {entry.name}" class="px-2">↓</a>{/if}
    </div>
   {/each}
   {#if truncated}<p class="p-2 text-xs text-muted-foreground">Showing the first 2,000 entries.</p>{/if}
  </div>{/if}
  <div class="flex min-h-[20rem] min-w-0 flex-1 flex-col">
   {#if file}
    <div class="flex items-center justify-between gap-2 border-b border-border p-2 text-xs"><span class="truncate" title={file.path}>{file.path}{dirty ? ' •' : ''}</span><button type="button" onclick={save} disabled={!dirty || busy} class="rounded bg-primary px-3 py-1 text-primary-foreground disabled:opacity-50">Save</button></div>
    <div class="min-h-0 flex-1">{#key file.path}<MonacoEditor value={content} path={file.path} onChange={(value) => (content = value)} onSave={save} />{/key}</div>
   {:else}<p class="m-auto p-6 text-center text-sm text-muted-foreground">{busy ? 'Loading…' : 'Browse the container, edit text files, or download files and folders. Folders download as .tar.gz archives.'}</p>{/if}
  </div>
 </div>
</div>
