<script lang="ts">
	import type { LiveConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: { role?: string; idle_prompt?: string | null }) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();

	let systemPrompt = $state('');
	let idlePrompt = $state('');

	$effect(() => {
		if (agentConfig) {
			systemPrompt = agentConfig.role ?? '';
			idlePrompt = agentConfig.idle_prompt ?? '';
		}
	});

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			handleSave();
		}
	});

	function handleSave() {
		onSave({
			role: systemPrompt,
			idle_prompt: idlePrompt.trim() || null,
		});
	}
</script>

<div class="space-y-6">
	<!-- System Prompt -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">System Prompt</h2>
		<p class="text-xs text-muted-foreground">
			The core instructions that define this agent's behavior, personality, and capabilities.
		</p>
		<textarea
			bind:value={systemPrompt}
			rows={12}
			placeholder="You are a helpful assistant..."
			class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"
		></textarea>
	</div>

	<!-- Idle Prompt -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Idle Prompt</h2>
		<p class="text-xs text-muted-foreground">
			When set, the agent self-activates during idle periods using an exponential backoff schedule.
			The agent maintains a scratch pad between cycles for notes and context.
		</p>
		<textarea
			bind:value={idlePrompt}
			rows={4}
			placeholder="e.g., Check for pending tasks, review your memory, and scan your workspace for anything that needs attention. If nothing needs action, rest."
			class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"
		></textarea>
		<p class="text-xs text-muted-foreground">
			Leave empty to disable idle tasks. Backoff schedule: immediate &rarr; 30m &rarr; 2h &rarr; 6h &rarr; 12h.
			Resets when the agent completes real work.
		</p>
	</div>

</div>
