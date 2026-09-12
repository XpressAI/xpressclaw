<script lang="ts">
	import { openExternal } from '$lib/utils';

	// The server decides whether this host can carry an SSH agent into a
	// runner, and the page that already loaded its runtime status passes the
	// answer down. The browser's own platform is irrelevant here: the instance
	// may be remote.
	let {
		enabled = $bindable(false),
		agentForwardingUnsupportedReason = null
	}: {
		enabled?: boolean;
		agentForwardingUnsupportedReason?: string | null;
	} = $props();

	const id = $props.id();
	const issueUrl = 'https://github.com/containers/podman/issues/23785';
</script>

<div class="flex items-start gap-3">
	<input {id} type="checkbox" bind:checked={enabled} aria-describedby={`${id}-description`}
		class="mt-0.5 h-4 w-4 rounded border-input" />
	<div>
		<label for={id} class="text-sm font-medium text-foreground">Share my host SSH access</label>
		<p id={`${id}-description`} class="mt-1 text-xs leading-relaxed text-muted-foreground">
			Give this runner the same access to <code>~/.ssh</code> as a coding agent running directly on this computer. Leave this off to keep those files out of the runner.
		</p>
	</div>
</div>
{#if agentForwardingUnsupportedReason}
	<p class="mt-3 rounded-md border border-sky-500/25 bg-sky-500/5 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
		{agentForwardingUnsupportedReason}. Key files in <code>~/.ssh</code> are still shared and still
		work, so this stays useful for ordinary key-based Git access. Keys that live only in an agent —
		YubiKeys, 1Password, Secretive — will not reach the runner. Use Docker Desktop for agent
		forwarding, or follow
		<a href={issueUrl} target="_blank" rel="noopener noreferrer" class="text-primary underline underline-offset-2"
			onclick={(event) => { event.preventDefault(); void openExternal(issueUrl); }}>Podman issue #23785</a>.
	</p>
{/if}
{#if enabled}
	<p class="mt-3 rounded-md border border-amber-500/25 bg-amber-500/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
		The harness can read and change files in <code>~/.ssh</code> and use unlocked SSH-agent keys when available. Enable this only for harnesses and tasks you trust.
	</p>
{/if}
