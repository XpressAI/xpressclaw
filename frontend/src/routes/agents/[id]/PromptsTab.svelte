<script lang="ts">
	import type { LiveConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		saveSignal: number;
		onSave: (data: { role?: string }) => void;
	}

	let { agentConfig, saveSignal, onSave }: Props = $props();

	let systemPrompt = $state('');

	$effect(() => {
		if (agentConfig) {
			systemPrompt = agentConfig.role ?? '';
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
		});
	}
</script>

<div class="space-y-6">
	<!-- System Prompt -->
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Native runner instructions</h2>
		<p class="text-xs text-muted-foreground">
			These profile instructions are prepended to every native work attempt.
		</p>
		<textarea
			bind:value={systemPrompt}
			rows={12}
			placeholder="You are a helpful assistant..."
			class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"
		></textarea>
	</div>
</div>
