<script lang="ts">
	import { onMount } from 'svelte';
	import type { LiveConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: { skills?: string[] }) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();

	let availableSkills = $state<{ name: string; description: string }[]>([]);
	let selectedSkills = $state<string[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	onMount(async () => {
		try {
			const resp = await fetch('/api/skills');
			if (!resp.ok) throw new Error(resp.statusText);
			availableSkills = await resp.json();
		} catch (e) {
			error = `Failed to load skills: ${e}`;
		}
		loading = false;
	});

	$effect(() => {
		if (agentConfig) {
			selectedSkills = [...(agentConfig.skills || [])];
		}
	});

	function toggleSkill(name: string) {
		if (selectedSkills.includes(name)) {
			selectedSkills = selectedSkills.filter(s => s !== name);
		} else {
			selectedSkills = [...selectedSkills, name];
		}
	}

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			handleSave();
		}
	});

	function handleSave() {
		onSave({ skills: selectedSkills });
	}
</script>

<div class="space-y-6">
	<div class="rounded-lg border border-border bg-card p-4 space-y-3">
		<h2 class="text-sm font-semibold">Skills</h2>
		<p class="text-xs text-muted-foreground">
			Skills teach the agent how to perform specific tasks. Select which skills this agent should have access to.
		</p>

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading skills...</p>
		{:else if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
				{error}
			</div>
		{:else if availableSkills.length === 0}
			<p class="text-sm text-muted-foreground italic">No skills available. Create skills in the Skills section.</p>
		{:else}
			<div class="space-y-2">
				{#each availableSkills as skill}
					{@const enabled = selectedSkills.includes(skill.name)}
					<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-3 hover:bg-accent/50 {enabled ? 'border-primary/30 bg-primary/5' : ''}">
						<input
							type="checkbox"
							checked={enabled}
							onchange={() => toggleSkill(skill.name)}
							class="rounded border-border"
						/>
						<div class="flex-1 min-w-0">
							<span class="text-sm font-medium text-foreground">{skill.name}</span>
							<p class="text-xs text-muted-foreground line-clamp-2 mt-0.5">{skill.description}</p>
						</div>
					</label>
				{/each}
			</div>
		{/if}
	</div>

</div>
