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
	let model = $state('');
	let llmProvider = $state('');
	let llmApiKey = $state('');
	let llmBaseUrl = $state('');
	let showModelModal = $state(false);

	$effect(() => {
		if (agentConfig) {
			displayName = agentConfig.display_name ?? (agentConfig.name.charAt(0).toUpperCase() + agentConfig.name.slice(1));
			roleTitle = agentConfig.role_title ?? '';
			responsibilities = agentConfig.responsibilities ?? '';
			model = agentConfig.model ?? '';
			llmProvider = agentConfig.llm?.provider ?? '';
			llmApiKey = agentConfig.llm?.api_key ?? '';
			llmBaseUrl = agentConfig.llm?.base_url ?? '';
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

	function applyModelConfig() {
		showModelModal = false;
		// Save immediately so model/provider changes persist
		onSave({
			model: model.trim() || undefined,
			llm: {
				provider: llmProvider || null,
				api_key: llmApiKey || null,
				base_url: llmBaseUrl || null,
			},
		});
	}

	function handleSave() {
		onSave({
			display_name: displayName.trim() || null,
			role_title: roleTitle.trim() || null,
			responsibilities: responsibilities.trim() || null,
			model: model.trim() || undefined,
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
				<label class="block text-xs text-muted-foreground mb-1">Agent Name</label>
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
					placeholder="What is this agent responsible for?"
					class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring"
				></textarea>
			</div>

			<div>
				<label class="block text-xs text-muted-foreground mb-1">Model</label>
				<div class="flex items-center gap-2">
					<span class="flex-1 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm font-mono">{model || 'default'}</span>
					<button onclick={() => showModelModal = true}
						class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent transition-colors">
						Change
					</button>
				</div>
			</div>

		</div>
	</div>

</div>

<!-- Model / LLM Provider Modal -->
{#if showModelModal}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onclick={() => showModelModal = false}>
		<div class="rounded-lg border border-border bg-card p-6 space-y-4 max-w-md mx-4 w-full" onclick={(e) => e.stopPropagation()}>
			<h2 class="text-lg font-semibold">Model & LLM Provider</h2>
			<div class="space-y-3">
				<div>
					<label class="block text-xs font-medium text-muted-foreground mb-1">Provider</label>
					<select bind:value={llmProvider}
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring">
						<option value="">Default (global)</option>
						<option value="openai">OpenAI-compatible</option>
						<option value="anthropic">Anthropic</option>
						<option value="local">Local</option>
					</select>
				</div>
				<div>
					<label class="block text-xs font-medium text-muted-foreground mb-1">Model</label>
					<input type="text" bind:value={model} placeholder="e.g. gpt-4o, claude-sonnet-4-5, qwen3.5:9b"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
				</div>
				{#if llmProvider === 'openai' || llmProvider === 'anthropic'}
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">API Key</label>
						<input type="password" bind:value={llmApiKey} placeholder={llmProvider === 'anthropic' ? 'sk-ant-...' : 'sk-...'}
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
					</div>
					<div>
						<label class="block text-xs font-medium text-muted-foreground mb-1">Base URL <span class="font-normal text-muted-foreground">(optional)</span></label>
						<input type="text" bind:value={llmBaseUrl} placeholder={llmProvider === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com'}
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-sm font-mono focus:outline-none focus:ring-1 focus:ring-ring" />
					</div>
				{/if}
			</div>
			<div class="flex justify-end gap-2">
				<button onclick={() => showModelModal = false}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent">Cancel</button>
				<button onclick={applyModelConfig}
					class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90">
					Apply
				</button>
			</div>
		</div>
	</div>
{/if}
