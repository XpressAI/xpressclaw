<script lang="ts">
	import type { LiveConfig } from '$lib/api';
	import { agentAvatar } from '$lib/utils';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: Record<string, unknown>) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();

	let displayName = $state('');
	let roleTitle = $state('');
	let responsibilities = $state('');

	$effect(() => {
		if (agentConfig) {
			displayName = agentConfig.display_name ?? (agentConfig.name.charAt(0).toUpperCase() + agentConfig.name.slice(1));
			roleTitle = agentConfig.role_title ?? '';
			responsibilities = agentConfig.responsibilities ?? '';
		}
	});

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			handleSave();
		}
	});

	async function uploadAvatar(event: Event) {
		const input = event.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		try {
			const formData = new FormData();
			formData.append('agent_id', agentId);
			formData.append('file', new File([file], 'avatar.png', { type: file.type }));
			await fetch('/api/office/upload', { method: 'POST', body: formData });
			const avatarUrl = `/api/office/documents/avatar.png?agent_id=${agentId}&t=${Date.now()}`;
			onSave({ avatar: avatarUrl });
		} catch (e) {
			alert(`Upload failed: ${e}`);
		}
		input.value = '';
	}

	function handleSave() {
		onSave({
			display_name: displayName.trim() || null,
			role_title: roleTitle.trim() || null,
			responsibilities: responsibilities.trim() || null,
		});
	}

	function getInitials(name: string): string {
		return name
			.split(/[\s_-]+/)
			.map(w => w[0]?.toUpperCase() ?? '')
			.slice(0, 2)
			.join('');
	}
</script>

<div class="space-y-6">
	<div class="flex gap-8">
		<!-- Left: Avatar -->
		<div class="flex flex-col items-center gap-3 shrink-0">
			<div class="w-28 h-28 rounded-full overflow-hidden border-2 border-border bg-muted flex items-center justify-center">
				{#if agentConfig?.avatar}
					<img src={agentConfig.avatar} alt="Avatar" class="w-full h-full object-cover rounded-full" />
				{:else if agentConfig}
					<img
						src={agentAvatar({ name: agentConfig.name, id: agentId })}
						alt={agentConfig.name}
						class="w-full h-full object-cover"
					/>
				{:else}
					<span class="text-2xl font-bold text-muted-foreground">
						{getInitials(displayName || agentId)}
					</span>
				{/if}
			</div>
			<label class="rounded-md border border-border px-3 py-1.5 text-xs text-muted-foreground hover:bg-accent cursor-pointer transition-colors">
				Change Avatar
				<input type="file" accept="image/*" onchange={uploadAvatar} class="hidden" />
			</label>
		</div>

		<!-- Right: Fields -->
		<div class="flex-1 space-y-4">
			<div>
				<label class="block text-xs text-muted-foreground mb-1">Session ID</label>
				<input
					type="text"
					value={agentConfig?.name ?? ''}
					disabled
					class="w-full rounded-md border border-border bg-muted px-3 py-2 text-sm text-muted-foreground cursor-not-allowed"
				/>
			</div>

			<div>
				<label class="block text-xs text-muted-foreground mb-1">Display Name</label>
				<input
					type="text"
					bind:value={displayName}
					placeholder="e.g. Atlas, Cody, Luna"
					class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
				/>
			</div>

			<div>
				<label class="block text-xs text-muted-foreground mb-1">Role Title</label>
				<input
					type="text"
					bind:value={roleTitle}
					placeholder="e.g. Personal Assistant, Code Reviewer"
					class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
				/>
			</div>

			<div>
				<label class="block text-xs text-muted-foreground mb-1">Responsibilities</label>
				<textarea
					bind:value={responsibilities}
					rows={3}
					placeholder="What is this session's native runner responsible for?"
					class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
				></textarea>
			</div>

		</div>
	</div>

</div>
