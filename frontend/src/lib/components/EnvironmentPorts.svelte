<script lang="ts">
 import { onMount } from 'svelte';
 import { environments, type PortForward } from '$lib/api';
 let { agentId }: { agentId: string } = $props();
 let ports = $state<PortForward[]>([]);
 let direction = $state<PortForward['direction']>('host_to_container');
 let hostPort = $state(8080);
 let containerPort = $state(8080);
 let error = $state('');
 let busy = $state(false);
 async function refresh() {
  try { ports = (await environments.ports(agentId)).ports; error = ''; }
  catch (cause) { error = String(cause); }
 }
 onMount(() => { void refresh(); });
 async function add() {
  busy = true; error = '';
  try { await environments.addPort(agentId, { direction, host_port: hostPort, container_port: containerPort }); await refresh(); }
  catch (cause) { error = String(cause); }
  finally { busy = false; }
 }
 async function remove(id: string) {
  busy = true;
  try { await environments.removePort(agentId, id); await refresh(); }
  catch (cause) { error = String(cause); }
  finally { busy = false; }
 }
</script>

<section class="ai-card space-y-3 p-5" data-environment-ports>
 <div class="flex items-center justify-between"><h3 class="text-sm font-medium">Port forwarding</h3><button type="button" onclick={refresh} class="text-xs underline">Refresh</button></div>
 <p class="text-xs text-muted-foreground">Connect a local LLM to this container, or share a container server with the user. Both addresses use 127.0.0.1. Host means the machine running XpressClaw. Saved mappings start with the environment.</p>
 <form onsubmit={(event) => { event.preventDefault(); void add(); }} class="flex flex-wrap items-end gap-3">
  <label class="text-xs">Direction<select bind:value={direction} class="mt-1 block rounded border border-input bg-background p-2"><option value="host_to_container">Host → container</option><option value="container_to_host">Container → host</option></select></label>
  <label class="text-xs">Host port<input type="number" min="1" max="65535" required bind:value={hostPort} class="mt-1 block w-24 rounded border border-input bg-background p-2" /></label>
  <label class="text-xs">Container port<input type="number" min="1" max="65535" required bind:value={containerPort} class="mt-1 block w-28 rounded border border-input bg-background p-2" /></label>
  <button disabled={busy} class="rounded bg-primary px-3 py-2 text-xs text-primary-foreground disabled:opacity-50">Add forward</button>
 </form>
 {#if error}<p role="alert" class="text-xs text-destructive">{error}</p>{/if}
 {#each ports as port (port.id)}
  <div class="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-2 text-xs">
   <span class="font-mono">Host :{port.host_port} {port.direction === 'host_to_container' ? '→' : '←'} Container :{port.container_port}</span>
   <span class="text-muted-foreground">{port.active ? 'Active' : 'Inactive'}</span>
   <button type="button" disabled={busy} onclick={() => remove(port.id)} class="text-destructive underline">Remove</button>
  </div>
 {/each}
</section>
