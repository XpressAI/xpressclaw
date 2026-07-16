<script lang="ts">
	import type { LiveConfig, NativeRunnerConfig } from '$lib/api';

	interface Props {
		agentConfig: LiveConfig['agents'][0] | null;
		saveSignal: number;
		onSave: (data: { runner: NativeRunnerConfig }) => void;
	}

	let { agentConfig, saveSignal, onSave }: Props = $props();
	let kind = $state('auto');
	let image = $state('');
	let workspace = $state('');
	let subscriptionAuth = $state(true);
	let maxTurns = $state(100);
	let commandText = $state('');
	const defaultImages: Record<string, string> = {
		codex: 'ghcr.io/xpressai/xpressclaw-runner-codex:latest',
		claude: 'ghcr.io/xpressai/xpressclaw-runner-claude:latest',
		opencode: 'ghcr.io/xpressai/xpressclaw-runner-opencode:latest'
	};

	function selectDefaultImage() {
		image = defaultImages[kind] ?? '';
	}

	$effect(() => {
		if (agentConfig?.runner) {
			kind = agentConfig.runner.kind;
			image = agentConfig.runner.image;
			workspace = agentConfig.runner.workspace ?? '';
			subscriptionAuth = agentConfig.runner.subscription_auth;
			maxTurns = agentConfig.runner.max_turns;
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
					command: commandText.split('\n').map((line) => line.trim()).filter(Boolean),
					subscription_auth: subscriptionAuth,
					max_turns: Math.max(1, maxTurns || 100)
				}
			});
		}
	});
</script>

<div class="mx-auto max-w-3xl space-y-6">
	<div class="rounded-xl border border-border bg-card p-5">
		<h2 class="text-sm font-semibold">Native worker</h2>
		<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
			Each task launches the selected CLI in a short-lived container and resumes this project's active conversation by default. The CLI owns its reasoning loop and subagents.
		</p>

		<div class="mt-5 grid gap-4 sm:grid-cols-2">
			<div>
				<label for="runner-kind" class="mb-1 block text-xs font-medium text-muted-foreground">CLI</label>
				<select id="runner-kind" bind:value={kind} onchange={selectDefaultImage} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring">
					<option value="auto">Auto from configured harness</option>
					<option value="codex">Codex</option>
					<option value="claude">Claude Code</option>
					<option value="opencode">OpenCode</option>
					<option value="custom">Custom command</option>
				</select>
			</div>
			<div>
				<label for="max-turns" class="mb-1 block text-xs font-medium text-muted-foreground">Maximum turns</label>
				<input id="max-turns" type="number" min="1" bind:value={maxTurns} class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-1 focus:ring-ring" />
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
			<p class="mt-1 text-[11px] text-muted-foreground">Published images are pulled on demand. Extend one or provide a custom image containing only your chosen CLI.</p>
		</div>
	</div>

	<div class="rounded-xl border border-border bg-card p-5">
		<div class="flex items-start gap-3">
			<input id="subscription-auth" type="checkbox" bind:checked={subscriptionAuth} class="mt-0.5 h-4 w-4 rounded border-input" />
			<div>
				<label for="subscription-auth" class="text-sm font-medium">Use host subscription login</label>
				<p class="mt-1 text-xs leading-relaxed text-muted-foreground">
					Mount the selected CLI's standard login directory into each worker. Git identity and GitHub CLI auth are also shared when present; SSH keys require an explicit volume.
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
		<h2 class="text-sm font-semibold">Command override</h2>
		<p class="mt-1 text-xs text-muted-foreground">Optional. Enter one argument per line. Available placeholders: <code>{'{prompt}'}</code> and <code>{'{workspace}'}</code>.</p>
		<textarea bind:value={commandText} rows="7" placeholder={'my-agent\nrun\n--json\n{prompt}'} class="mt-3 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs outline-none focus:ring-1 focus:ring-ring"></textarea>
	</div>
</div>
