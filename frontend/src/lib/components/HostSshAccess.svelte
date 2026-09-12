<script lang="ts">
	import { onMount } from 'svelte';
	import { setup } from '$lib/api';
	import { openExternal } from '$lib/utils';

	let { enabled = $bindable(false) }: { enabled?: boolean } = $props();
	const id = $props.id();
	const issueUrl = 'https://github.com/podman-container-tools/podman/issues/23785';
	let checking = $state(true);
	let checkFailed = $state(false);
	let podmanOnMac = $state(false);
	let dialog: HTMLDialogElement;
	let keepDisabledButton: HTMLButtonElement;

	onMount(async () => {
		try {
			// Inspect the instance host, which may differ from the browser's OS.
			const [system, docker] = await Promise.all([setup.systemInfo(), setup.checkDocker()]);
			podmanOnMac = system.os === 'macos' && docker.runtime === 'podman';
		} catch (error) {
			console.warn('Could not check host SSH forwarding compatibility', error);
			checkFailed = true;
		} finally {
			checking = false;
		}
	});

	function changeAccess(event: Event) {
		const checkbox = event.currentTarget as HTMLInputElement;
		if (checkbox.checked && podmanOnMac) {
			checkbox.checked = false;
			dialog.showModal();
			keepDisabledButton.focus();
			return;
		}
		enabled = checkbox.checked;
	}

	function enableAnyway() {
		enabled = true;
		dialog.close();
	}
</script>

<div class="flex items-start gap-3">
	<input id={id} type="checkbox" checked={enabled} onchange={changeAccess} disabled={checking}
		aria-describedby={`${id}-description`} class="mt-0.5 h-4 w-4 rounded border-input disabled:opacity-50" />
	<div>
		<label for={id} class="text-sm font-medium text-foreground">Share my host SSH access</label>
		<p id={`${id}-description`} class="mt-1 text-xs leading-relaxed text-muted-foreground">
			Give this runner the same access to <code>~/.ssh</code> as a coding agent running directly on this computer. Leave this off to keep those files out of the runner.
		</p>
	</div>
</div>
{#if checking}
	<p role="status" class="mt-2 text-xs text-muted-foreground">Checking SSH forwarding compatibility…</p>
{:else if checkFailed}
	<p role="status" class="mt-2 text-xs text-muted-foreground">
		Could not check the container runtime. If this instance uses Podman on macOS, switch to Docker Desktop for SSH-agent forwarding.
	</p>
{/if}
{#if enabled}
	<p class="mt-3 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
		The harness can read and change files in <code>~/.ssh</code> and use unlocked SSH-agent keys when available. Enable this only for harnesses and tasks you trust.
	</p>
{/if}

<dialog bind:this={dialog} aria-labelledby={`${id}-warning-title`} aria-describedby={`${id}-warning-description`}
	class="m-auto w-[calc(100%-2rem)] max-w-lg rounded-xl border border-border bg-card p-6 text-foreground shadow-[var(--shadow-overlay)] backdrop:bg-black/50">
	<h2 id={`${id}-warning-title`} class="text-lg font-semibold">SSH forwarding with Podman on macOS</h2>
	<div id={`${id}-warning-description`} class="mt-3 space-y-3 text-sm leading-relaxed text-muted-foreground">
		<p>Podman on macOS cannot currently forward your host SSH agent into containers. Enabling this option can prevent agents from starting.</p>
		<p>
			This is a known Podman limitation tracked in
			<a href={issueUrl} target="_blank" rel="noopener noreferrer" class="text-primary underline underline-offset-2"
				onclick={(event) => { event.preventDefault(); void openExternal(issueUrl); }}>Podman issue #23785</a>.
			Switch to Docker Desktop to use SSH forwarding.
		</p>
		<p>You can still enable it to test whether a future Podman release resolves the issue.</p>
	</div>
	<div class="mt-6 flex flex-wrap justify-end gap-2">
		<button bind:this={keepDisabledButton} type="button" onclick={() => dialog.close()}
			class="rounded-md border border-input px-4 py-2 text-sm font-medium hover:bg-muted">Keep disabled</button>
		<button type="button" onclick={enableAnyway}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">Enable anyway</button>
	</div>
</dialog>
