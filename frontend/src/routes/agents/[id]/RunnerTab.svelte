<script lang="ts">
	import { onMount } from 'svelte';
	import { sessions } from '$lib/api';
	import type { LiveConfig, NativeRunnerConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		agentId: string;
		saveSignal: number;
		onSave: (data: { runner: NativeRunnerConfig }) => void;
	}

	let { agentConfig, agentId, saveSignal, onSave }: Props = $props();
	let kind = $state('auto');
	let image = $state('');
	let workspace = $state('');
	let model = $state('');
	let subscriptionAuth = $state(true);
	let commandText = $state('');
	let modelOptions = $state<{ value: string; name: string }[]>([]);
	const defaultImages: Record<string, string> = {
		codex: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest',
		claude: 'ghcr.io/xpressai/xpressclaw-runner-claude:latest',
		opencode: 'ghcr.io/xpressai/xpressclaw-runner-opencode:latest'
	};

	function selectDefaultImage() {
		image = defaultImages[kind] ?? '';
		if (kind === 'custom') subscriptionAuth = false;
	}

	onMount(async () => {
		try {
			const events = await sessions.events(agentId);
			const advertised = [...events].reverse().find((event) => event.event_type === 'session_config_options');
			const choices = advertised?.payload.models;
			if (Array.isArray(choices)) {
				modelOptions = choices.filter((choice): choice is { value: string; name: string } =>
					typeof choice === 'object' && choice !== null
					&& typeof (choice as Record<string, unknown>).value === 'string'
					&& typeof (choice as Record<string, unknown>).name === 'string'
				);
			}
		} catch {
			modelOptions = [];
		}
	});

	$effect(() => {
		if (agentConfig?.runner) {
			const configuredKind = agentConfig.runner.kind;
			kind = configuredKind === 'auto'
				? (agentConfig.backend.includes('claude') ? 'claude' : agentConfig.backend.includes('opencode') ? 'opencode' : 'codex')
				: configuredKind;
			image = agentConfig.runner.image;
			workspace = agentConfig.runner.workspace ?? '';
			model = agentConfig.runner.model ?? '';
			subscriptionAuth = agentConfig.runner.subscription_auth;
			commandText = agentConfig.runner.command.join('\n');
		}
	});

	let lastSignal = 0;
	$effect(() => {
		if (saveSignal > 0 && saveSignal !== lastSignal) {
			lastSignal = saveSignal;
			onSave({
				runner: {
					kind,
					image: image.trim() || defaultImages[kind] || '',
					workspace: workspace.trim() || null,
					model: model.trim() || null,
					command: commandText.split('\n').map((line) => line.trim()).filter(Boolean),
					subscription_auth: subscriptionAuth
				}
			});
		}
	});
</script>

<div class="mx-auto max-w-3xl space-y-6">
	<div class="rounded-xl border border-border bg-card p-5">
		<h2 class="text-sm font-semibold">ACP agent</h2>
		<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
			XpressClaw talks to the agent over the Agent Client Protocol. Each task resumes this project's active conversation by default; the agent keeps ownership of its reasoning, tools, and subagents.
		</p>

		<div class="mt-5 grid gap-4 sm:grid-cols-2">
			<div>
				<label for="runner-kind" class="mb-1 block text-xs font-medium text-muted-foreground">Agent</label>
				<select id="runner-kind" bind:value={kind} onchange={selectDefaultImage} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
					<option value="codex">Codex</option>
					<option value="claude">Claude Code</option>
					<option value="opencode">OpenCode</option>
					<option value="custom">Other ACP agent</option>
				</select>
			</div>
			<div>
				<label for="runner-model" class="mb-1 block text-xs font-medium text-muted-foreground">Model</label>
				<input id="runner-model" list="runner-model-options" bind:value={model} placeholder="Agent default" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
				<datalist id="runner-model-options">
					{#each modelOptions as option}
						<option value={option.value}>{option.name}</option>
					{/each}
				</datalist>
				<p class="mt-1 text-[11px] text-muted-foreground">
					{modelOptions.length > 0 ? `${modelOptions.length} choices reported by the agent. ` : 'Choices appear after the agent starts its first session. '}
					Leave blank to use the agent's default.
				</p>
			</div>
		</div>

		<div class="mt-4">
			<label for="runner-workspace" class="mb-1 block text-xs font-medium text-muted-foreground">Project workspace</label>
			<input id="runner-workspace" bind:value={workspace} placeholder="~/projects/my-app" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
			<p class="mt-1 text-[11px] text-muted-foreground">An existing host folder mounted read-write at <code>/workspace</code>.</p>
		</div>

		<div class="mt-4">
			<label for="runner-image" class="mb-1 block text-xs font-medium text-muted-foreground">Container image</label>
			<input id="runner-image" bind:value={image} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
			<p class="mt-1 text-[11px] text-muted-foreground">Published images are pulled on demand. Extend one or provide a custom image whose command starts an ACP server over stdio.</p>
		</div>
	</div>

	<div class="rounded-xl border border-border bg-card p-5">
		<div class="flex items-start gap-3">
			<input id="subscription-auth" type="checkbox" bind:checked={subscriptionAuth} disabled={kind === 'custom'} class="mt-0.5 h-4 w-4 rounded border-input disabled:opacity-50" />
			<div>
				<label for="subscription-auth" class="text-sm font-medium">Use host subscription login</label>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					{kind === 'custom'
						? 'Custom agents manage their own authentication; add any required directories in the Volumes tab.'
						: "Mount the selected agent's standard login directory into each worker. Git identity and GitHub CLI auth are also shared when present; SSH keys require an explicit volume."}
				</p>
				{#if subscriptionAuth}
					<p class="mt-2 rounded-md border border-amber-500/20 bg-amber-500/10 px-3 py-2 text-xs text-amber-600">
						The worker can refresh the mounted OAuth credentials. Use trusted images only.
					</p>
				{/if}
			</div>
		</div>
	</div>

	<div class="rounded-xl border border-border bg-card p-5">
		<h2 class="text-sm font-semibold">ACP server command</h2>
		<p class="mt-1 text-xs text-muted-foreground">{kind === 'custom' ? 'Required for custom agents.' : 'Optional override for the built-in adapter.'} Enter one argument per line. Available placeholder: <code>{'{workspace}'}</code>.</p>
		<textarea bind:value={commandText} rows="7" placeholder={'my-agent\nacp\n--cwd\n{workspace}'} class="mt-3 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
	</div>
</div>
