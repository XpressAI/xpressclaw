<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { agents, setup } from '$lib/api';
	import type { Agent, LiveConfig } from '$lib/api';
	import { statusColor, timeAgo } from '$lib/utils';

	let agent = $state<Agent | null>(null);
	let error = $state<string | null>(null);
	let agentConfig = $state<LiveConfig['agents'][0] | null>(null);

	// Editable state
	let editRole = $state('');
	let editModel = $state('');
	let editLlmProvider = $state('');
	let editLlmApiKey = $state('');
	let editLlmBaseUrl = $state('');
	let editVolumes = $state<string[]>([]);
	let newVolumePath = $state('');
	let saving = $state(false);
	let saveMessage = $state('');
	let needsRestart = $state(false);
	let showDeleteConfirm = $state(false);
	let deleting = $state(false);

	// Tool toggles
	let fetchEnabled = $state(false);
	let gitEnabled = $state(false);
	let githubEnabled = $state(false);
	let websearchEnabled = $state(false);

	// Skills
	let editSkills = $state<string[]>([]);
	let availableSkills = $state<{ name: string; description: string }[]>([]);

	// Budget override
	let budgetEnabled = $state(false);
	let editBudgetDaily = $state('');
	let editBudgetMonthly = $state('');
	let editBudgetPerTask = $state('');
	let editBudgetOnExceeded = $state('pause');
	let editBudgetFallbackModel = $state('local');
	let editBudgetWarnPercent = $state(80);

	// Rate limit override
	let rateLimitEnabled = $state(false);
	let editRpm = $state(60);
	let editTpm = $state(100000);
	let editConcurrent = $state(5);

	// Wake-on triggers
	let editWakeOn = $state<{ schedule: string; event: string; condition: string }[]>([]);

	// Idle prompt
	let editIdlePrompt = $state('');

	// Hooks
	let editBeforeHooks = $state<string[]>([]);
	let editAfterHooks = $state<string[]>([]);
	let newBeforeHook = $state('');
	let newAfterHook = $state('');

	let pollTimer: ReturnType<typeof setInterval> | null = null;

	// Track which agent ID we've loaded. Plain variable (not $state) so
	// reading it inside the subscription doesn't create reactive deps.
	let loadedAgentId = '';

	// Subscribe to the page store manually instead of using $effect,
	// which aggressively re-fires on any $page update (XCLAW-48).
	// This only calls loadAgent when the route param actually changes.
	let unsubPage: (() => void) | null = null;
	onMount(() => {
		unsubPage = page.subscribe(($p) => {
			const id = $p.params.id;
			if (id && id !== loadedAgentId) {
				loadedAgentId = id;
				loadAgent(id);
			}
		});
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		if (unsubPage) unsubPage();
	});

	async function loadAgent(id: string) {
		// Clear previous poll
		if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
		error = null;

		try {
			agent = await agents.get(id);
			const config = await setup.getConfig();
			agentConfig = config.agents.find(a => a.name === agent!.name) ?? null;
			if (agentConfig) {
				editRole = agentConfig.role;
				editModel = agentConfig.model ?? '';
				editLlmProvider = agentConfig.llm?.provider ?? '';
				editLlmApiKey = agentConfig.llm?.api_key ?? '';
				editLlmBaseUrl = agentConfig.llm?.base_url ?? '';
				editVolumes = [...(agentConfig.volumes || [])];
				fetchEnabled = agentConfig.tools.includes('fetch');
				gitEnabled = agentConfig.tools.includes('git');
				githubEnabled = agentConfig.tools.includes('github');
				websearchEnabled = agentConfig.tools.includes('websearch');
				editSkills = [...(agentConfig.skills || [])];

				if (agentConfig.budget) {
					budgetEnabled = true;
					editBudgetDaily = agentConfig.budget.daily ?? '';
					editBudgetMonthly = agentConfig.budget.monthly ?? '';
					editBudgetPerTask = agentConfig.budget.per_task ?? '';
					editBudgetOnExceeded = agentConfig.budget.on_exceeded ?? 'pause';
					editBudgetFallbackModel = agentConfig.budget.fallback_model ?? 'local';
					editBudgetWarnPercent = agentConfig.budget.warn_at_percent ?? 80;
				}

				if (agentConfig.rate_limit) {
					rateLimitEnabled = true;
					editRpm = agentConfig.rate_limit.requests_per_minute;
					editTpm = agentConfig.rate_limit.tokens_per_minute;
					editConcurrent = agentConfig.rate_limit.concurrent_requests;
				}

				editWakeOn = (agentConfig.wake_on ?? []).map(w => ({
					schedule: w.schedule ?? '',
					event: w.event ?? '',
					condition: w.condition ?? '',
				}));

				editIdlePrompt = agentConfig.idle_prompt ?? '';
				editBeforeHooks = [...(agentConfig.hooks?.before_message ?? [])];
				editAfterHooks = [...(agentConfig.hooks?.after_message ?? [])];
			}
		} catch (e) {
			error = String(e);
		}

		// Load available skills
		try {
			const resp = await fetch('/api/skills');
			availableSkills = await resp.json();
		} catch {}

		// Poll agent status every 5s so reconciler progress is visible
		pollTimer = setInterval(async () => {
			try {
				agent = await agents.get(id);
			} catch {}
		}, 5000);
	}

	async function handleStart() {
		if (!agent) return;
		try { agent = await agents.start(agent.id); needsRestart = false; } catch (e) { alert(String(e)); }
	}

	async function handleStop() {
		if (!agent) return;
		try { agent = await agents.stop(agent.id); } catch (e) { alert(String(e)); }
	}

	async function handleRestart() {
		if (!agent) return;
		try {
			agent = await agents.stop(agent.id);
			agent = await agents.start(agent.id);
			needsRestart = false;
		} catch (e) { alert(String(e)); }
	}

	async function handleDelete() {
		if (!agent || deleting) return;
		deleting = true;
		try {
			await agents.delete(agent.id);
			window.location.href = '/agents';
		} catch (e) {
			deleting = false;
			showDeleteConfirm = false;
			saveMessage = `Error deleting: ${e}`;
		}
	}

	async function saveConfig() {
		if (!agent) return;
		saving = true;
		saveMessage = '';
		try {
			const tools = ['filesystem', 'shell', 'memory'];
			if (fetchEnabled) tools.push('fetch');
			if (gitEnabled) tools.push('git');
			if (githubEnabled) tools.push('github');
			if (websearchEnabled) tools.push('websearch');

			const payload: Parameters<typeof agents.updateConfig>[1] = {
				role: editRole,
				model: editModel || undefined,
				volumes: editVolumes,
				tools,
				skills: editSkills,
			};

			payload.llm = {
				provider: editLlmProvider || null,
				api_key: editLlmApiKey || null,
				base_url: editLlmBaseUrl || null,
			};

			if (budgetEnabled) {
				payload.budget = {
					daily: editBudgetDaily || null,
					monthly: editBudgetMonthly || null,
					per_task: editBudgetPerTask || null,
					on_exceeded: editBudgetOnExceeded,
					fallback_model: editBudgetFallbackModel,
					warn_at_percent: editBudgetWarnPercent,
				};
			}

			if (rateLimitEnabled) {
				payload.rate_limit = {
					requests_per_minute: editRpm,
					tokens_per_minute: editTpm,
					concurrent_requests: editConcurrent,
				};
			}

			payload.wake_on = editWakeOn
				.filter(w => w.schedule || w.event || w.condition)
				.map(w => ({
					schedule: w.schedule || null,
					event: w.event || null,
					condition: w.condition || null,
				}));

			payload.hooks = {
				before_message: editBeforeHooks.filter(h => h.trim()),
				after_message: editAfterHooks.filter(h => h.trim()),
			};

			payload.idle_prompt = editIdlePrompt.trim() || null;

			const result = await agents.updateConfig(agent.id, payload);
			if (result.needs_restart) {
				needsRestart = true;
				saveMessage = 'Saved. Restart the agent for changes to take effect.';
			} else {
				saveMessage = 'Saved.';
			}
			if (agentConfig) {
				agentConfig = { ...agentConfig, ...result.agent };
			}
		} catch (e) {
			saveMessage = `Error: ${e}`;
		}
		saving = false;
	}

	function addVolume() {
		if (newVolumePath.trim()) {
			const path = newVolumePath.trim();
			const basename = path.split('/').filter(Boolean).pop() || 'workspace';
			editVolumes = [...editVolumes, `${path}:/workspace/${basename}`];
			newVolumePath = '';
		}
	}

	function removeVolume(idx: number) {
		editVolumes = editVolumes.filter((_, i) => i !== idx);
	}

	function addWakeOn() {
		editWakeOn = [...editWakeOn, { schedule: '', event: '', condition: '' }];
	}

	function removeWakeOn(idx: number) {
		editWakeOn = editWakeOn.filter((_, i) => i !== idx);
	}

	function addBeforeHook() {
		if (newBeforeHook.trim()) {
			editBeforeHooks = [...editBeforeHooks, newBeforeHook.trim()];
			newBeforeHook = '';
		}
	}

	function addAfterHook() {
		if (newAfterHook.trim()) {
			editAfterHooks = [...editAfterHooks, newAfterHook.trim()];
			newAfterHook = '';
		}
	}

	function removeBeforeHook(idx: number) { editBeforeHooks = editBeforeHooks.filter((_, i) => i !== idx); }
	function removeAfterHook(idx: number) { editAfterHooks = editAfterHooks.filter((_, i) => i !== idx); }
</script>

<div class="flex flex-col h-full">
	<!-- Header (stays at top) -->
	<div class="shrink-0 px-6 py-4 space-y-3 border-b border-border">
		<div class="flex items-center gap-2 text-sm text-muted-foreground">
			<a href="/agents" class="hover:text-foreground">Agents</a>
			<span>/</span>
			<span class="text-foreground">{agent?.name ?? '...'}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">{error}</div>
		{:else if agent}
			<div class="flex items-start justify-between">
				<div>
					<h1 class="text-2xl font-bold">{agent.name}</h1>
					<p class="text-sm text-muted-foreground mt-1">
						<span class="{statusColor(agent.status)}">{agent.status}</span>
						{#if agent.restart_count > 0 && agent.desired_status === 'running' && agent.status !== 'running'}
							<span class="text-amber-500">(restarting, attempt {agent.restart_count})</span>
						{/if}
						&middot; {agent.backend}
					</p>
				</div>
				<div class="flex gap-2">
					{#if agent.desired_status === 'running'}
						{#if needsRestart}
							<button onclick={handleRestart}
								class="rounded-md bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700 transition-colors">
								Restart
							</button>
						{/if}
						<button onclick={handleStop}
							class="rounded-md border border-border bg-secondary px-4 py-2 text-sm font-medium hover:bg-accent transition-colors">
							Stop
					</button>
				{:else}
					<button onclick={handleStart}
						class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors">
						Start
					</button>
				{/if}
				<button onclick={() => { showDeleteConfirm = true; }}
					class="rounded-md border border-destructive/50 px-4 py-2 text-sm font-medium text-destructive hover:bg-destructive/10 transition-colors">
					Delete
				</button>
			</div>
		</div>

		{#if needsRestart && saveMessage}
			<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm text-amber-600">
				{saveMessage}
			</div>
		{/if}
		{/if}
	</div>

	<!-- Body (scrollable) -->
	<div class="flex-1 overflow-y-auto px-6 py-6 space-y-6">
		{#if agent}
		{#if agent.error_message}
			<div class="rounded-lg border border-destructive/50 bg-card p-4 space-y-2">
				<h2 class="text-sm font-semibold text-destructive">Error</h2>
				<pre class="text-xs text-muted-foreground whitespace-pre-wrap">{agent.error_message}</pre>
			</div>
		{/if}

		<div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
			<!-- Left: Details -->
			<div class="space-y-4">
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Details</h2>
					<dl class="space-y-2 text-sm">
						<div class="flex justify-between">
							<dt class="text-muted-foreground">ID</dt>
							<dd class="font-mono text-xs">{agent.id}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Backend</dt>
							<dd>{agent.backend}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Created</dt>
							<dd>{timeAgo(agent.created_at)}</dd>
						</div>
						{#if agent.started_at}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Started</dt>
								<dd>{timeAgo(agent.started_at)}</dd>
							</div>
						{/if}
						{#if agent.container_id}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Container</dt>
								<dd class="font-mono text-xs truncate max-w-[200px]">{agent.container_id}</dd>
							</div>
						{/if}
					</dl>
				</div>
			</div>

			<!-- Right: Configuration -->
			<div class="lg:col-span-2 space-y-4">
				<!-- System Prompt -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">System Prompt</h2>
					<textarea bind:value={editRole} rows="5"
						class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"></textarea>
				</div>

				<!-- LLM -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">LLM</h2>
					<div class="grid grid-cols-2 gap-3">
						<div>
							<label class="block text-xs text-muted-foreground mb-1">Provider</label>
							<select bind:value={editLlmProvider}
								class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring">
								<option value="">Default</option>
								<option value="openai">OpenAI-compatible</option>
								<option value="anthropic">Anthropic</option>
								<option value="local">Local</option>
							</select>
						</div>
						<div>
							<label class="block text-xs text-muted-foreground mb-1">Model</label>
							<input type="text" bind:value={editModel} placeholder="e.g. gpt-4o, claude-sonnet-4-5"
								class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
					</div>
					{#if editLlmProvider === 'openai' || editLlmProvider === 'anthropic'}
						<div class="grid grid-cols-2 gap-3">
							<div>
								<label class="block text-xs text-muted-foreground mb-1">API Key</label>
								<input type="password" bind:value={editLlmApiKey} placeholder={agentConfig?.llm?.api_key ? '(set)' : '(uses global key)'}
									class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
							</div>
							<div>
								<label class="block text-xs text-muted-foreground mb-1">Base URL</label>
								<input type="text" bind:value={editLlmBaseUrl} placeholder="Default API endpoint"
									class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
							</div>
						</div>
					{:else if editLlmProvider === 'local'}
						<div>
							<label class="block text-xs text-muted-foreground mb-1">Base URL</label>
							<input type="text" bind:value={editLlmBaseUrl} placeholder="http://localhost:11434/v1"
								class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						</div>
					{/if}
				</div>

				<!-- Workspace Folders -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Workspace Folders</h2>
					<p class="text-xs text-muted-foreground">
						Folders from your machine mounted into the agent's container at <code class="bg-muted px-1 rounded">/workspace/</code>.
					</p>
					{#if editVolumes.length > 0}
						<div class="space-y-2">
							{#each editVolumes as vol, i}
								{@const parts = vol.split(':')}
								<div class="flex items-center gap-2 rounded-md border border-border px-3 py-2">
									<span class="flex-1 text-sm font-mono truncate">{parts[0]}</span>
									<span class="text-xs text-muted-foreground">{parts[1] || ''}</span>
									<button onclick={() => removeVolume(i)}
										class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground">&#x2715;</button>
								</div>
							{/each}
						</div>
					{/if}
					<div class="flex gap-2">
						<input type="text" bind:value={newVolumePath} placeholder="~/projects/my-app"
							onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') addVolume(); }}
							class="flex-1 rounded-md border border-border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
						<button onclick={addVolume} disabled={!newVolumePath.trim()}
							class="rounded-md border border-border px-3 py-2 text-sm hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
					</div>
				</div>

				<!-- Tools -->
				<div class="rounded-lg border border-border bg-card p-4 space-y-3">
					<h2 class="text-sm font-semibold">Tools</h2>
					<div class="space-y-2 mb-3">
						<div class="flex items-center gap-1.5 mb-1">
							<span class="text-xs text-muted-foreground">Always included:</span>
						</div>
						<div class="flex flex-wrap gap-1.5">
							{#each [
								{ name: 'Filesystem', desc: 'Read, write, edit files' },
								{ name: 'Shell', desc: 'Run commands' },
								{ name: 'Tasks', desc: 'Create, track, schedule tasks' },
								{ name: 'Memory', desc: 'Search and save long-term memory' },
								{ name: 'Apps', desc: 'Build and publish web apps' },
								{ name: 'Skills', desc: 'Read skill instructions' },
								{ name: 'Office', desc: 'Word, Excel, PowerPoint via AppleScript' },
								{ name: 'Browser', desc: 'Screenshots, scraping via Playwright' },
								{ name: 'Logs', desc: 'View agent and app container logs' },
							] as tool}
								<span class="inline-flex items-center rounded-md bg-muted px-2 py-1 text-xs text-muted-foreground" title={tool.desc}>
									{tool.name}
								</span>
							{/each}
						</div>
					</div>
					<div class="space-y-2">
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={fetchEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">Internet Access (Fetch)</span>
								<span class="text-xs text-muted-foreground ml-1">Fetch web pages and APIs</span>
							</div>
						</label>
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={websearchEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">Web Search</span>
								<span class="text-xs text-muted-foreground ml-1">Search the web via DuckDuckGo</span>
							</div>
						</label>
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={gitEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">Git</span>
								<span class="text-xs text-muted-foreground ml-1">Interact with repositories</span>
							</div>
						</label>
						<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50">
							<input type="checkbox" bind:checked={githubEnabled} class="rounded border-border" />
							<div>
								<span class="text-sm font-medium text-foreground">GitHub</span>
								<span class="text-xs text-muted-foreground ml-1">Issues, PRs, repos</span>
							</div>
						</label>
					</div>
				</div>

				<!-- Skills -->
				{#if availableSkills.length > 0}
					<div class="rounded-lg border border-border bg-card p-4 space-y-3">
						<h2 class="text-sm font-semibold">Skills</h2>
						<p class="text-xs text-muted-foreground">Skills teach the agent how to perform specific tasks.</p>
						<div class="space-y-2">
							{#each availableSkills as skill}
								{@const enabled = editSkills.includes(skill.name)}
								<label class="flex items-center gap-3 cursor-pointer rounded-md border border-border p-2 hover:bg-accent/50 {enabled ? 'border-primary/30 bg-primary/5' : ''}">
									<input
										type="checkbox"
										checked={enabled}
										onchange={() => {
											if (enabled) {
												editSkills = editSkills.filter(s => s !== skill.name);
											} else {
												editSkills = [...editSkills, skill.name];
											}
										}}
										class="rounded border-border"
									/>
									<div>
										<span class="text-sm font-medium text-foreground">{skill.name}</span>
										<span class="text-xs text-muted-foreground ml-1 line-clamp-1">{skill.description}</span>
									</div>
								</label>
							{/each}
						</div>
					</div>
				{/if}

				<!-- Budget Override -->
				<details class="rounded-lg border border-border bg-card" open={budgetEnabled}>
					<summary class="cursor-pointer p-4 text-sm font-semibold select-none hover:bg-accent/30">
						Budget Override
						{#if budgetEnabled}<span class="ml-2 text-xs font-normal text-emerald-500">enabled</span>{/if}
					</summary>
					<div class="px-4 pb-4 space-y-3 border-t border-border pt-3">
						<label class="flex items-center gap-2 cursor-pointer">
							<input type="checkbox" bind:checked={budgetEnabled} class="rounded border-border" />
							<span class="text-sm">Override system budget for this agent</span>
						</label>
						{#if budgetEnabled}
							<div class="grid grid-cols-3 gap-3">
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Daily limit</label>
									<input type="text" bind:value={editBudgetDaily} placeholder="$20.00"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Monthly limit</label>
									<input type="text" bind:value={editBudgetMonthly} placeholder="$500.00"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Per-task limit</label>
									<input type="text" bind:value={editBudgetPerTask} placeholder="$5.00"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
							</div>
							<div class="grid grid-cols-3 gap-3">
								<div>
									<label class="block text-xs text-muted-foreground mb-1">On exceeded</label>
									<select bind:value={editBudgetOnExceeded}
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring">
										<option value="pause">Pause</option>
										<option value="alert">Alert</option>
										<option value="degrade">Degrade</option>
										<option value="stop">Stop</option>
									</select>
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Fallback model</label>
									<input type="text" bind:value={editBudgetFallbackModel} placeholder="local"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Warn at %</label>
									<input type="number" bind:value={editBudgetWarnPercent} min="0" max="100"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
							</div>
						{/if}
					</div>
				</details>

				<!-- Rate Limiting Override -->
				<details class="rounded-lg border border-border bg-card" open={rateLimitEnabled}>
					<summary class="cursor-pointer p-4 text-sm font-semibold select-none hover:bg-accent/30">
						Rate Limiting Override
						{#if rateLimitEnabled}<span class="ml-2 text-xs font-normal text-emerald-500">enabled</span>{/if}
					</summary>
					<div class="px-4 pb-4 space-y-3 border-t border-border pt-3">
						<label class="flex items-center gap-2 cursor-pointer">
							<input type="checkbox" bind:checked={rateLimitEnabled} class="rounded border-border" />
							<span class="text-sm">Override system rate limits for this agent</span>
						</label>
						{#if rateLimitEnabled}
							<div class="grid grid-cols-3 gap-3">
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Requests/min</label>
									<input type="number" bind:value={editRpm} min="1"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Tokens/min</label>
									<input type="number" bind:value={editTpm} min="1"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<div>
									<label class="block text-xs text-muted-foreground mb-1">Concurrent</label>
									<input type="number" bind:value={editConcurrent} min="1"
										class="w-full rounded-md border border-border bg-background px-3 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
							</div>
						{/if}
					</div>
				</details>

				<!-- Wake-on Triggers -->
				<details class="rounded-lg border border-border bg-card" open={editWakeOn.length > 0}>
					<summary class="cursor-pointer p-4 text-sm font-semibold select-none hover:bg-accent/30">
						Wake-on Triggers
						{#if editWakeOn.length > 0}<span class="ml-2 text-xs font-normal text-muted-foreground">{editWakeOn.length}</span>{/if}
					</summary>
					<div class="px-4 pb-4 space-y-3 border-t border-border pt-3">
						<p class="text-xs text-muted-foreground">Define when this agent should automatically activate.</p>
						{#each editWakeOn as trigger, i}
							<div class="flex items-start gap-2 rounded-md border border-border p-2">
								<div class="flex-1 grid grid-cols-3 gap-2">
									<input type="text" bind:value={trigger.schedule} placeholder="cron: */30 * * * *"
										class="rounded-md border border-border bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring" />
									<input type="text" bind:value={trigger.event} placeholder="event: user.message"
										class="rounded-md border border-border bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring" />
									<input type="text" bind:value={trigger.condition} placeholder="condition"
										class="rounded-md border border-border bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring" />
								</div>
								<button onclick={() => removeWakeOn(i)}
									class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground text-xs">&#x2715;</button>
							</div>
						{/each}
						<button onclick={addWakeOn}
							class="rounded-md border border-dashed border-border px-3 py-1.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground">
							+ Add trigger
						</button>
					</div>
				</details>

				<!-- Idle Prompt -->
				<details class="rounded-lg border border-border bg-card" open={!!editIdlePrompt}>
					<summary class="cursor-pointer p-4 text-sm font-semibold select-none hover:bg-accent/30">
						Idle Prompt
						{#if editIdlePrompt}<span class="ml-2 text-xs font-normal text-muted-foreground">active</span>{/if}
					</summary>
					<div class="px-4 pb-4 space-y-3 border-t border-border pt-3">
						<p class="text-xs text-muted-foreground">
							When set, the agent self-activates during idle periods with exponential backoff.
							The agent maintains a scratch pad between cycles for notes and context.
						</p>
						<textarea bind:value={editIdlePrompt} rows="4"
							placeholder="e.g., Check for pending tasks, review your memory, and scan your workspace for anything that needs attention. If nothing needs action, rest."
							class="w-full rounded-md border border-border bg-background px-3 py-2 text-xs font-mono focus:outline-none focus:ring-1 focus:ring-ring"></textarea>
						<p class="text-xs text-muted-foreground">
							Leave empty to disable idle tasks. Backoff: immediate &rarr; 30m &rarr; 2h &rarr; 6h &rarr; 12h. Resets when the agent completes real work.
						</p>
					</div>
				</details>

				<!-- Hooks -->
				<details class="rounded-lg border border-border bg-card" open={editBeforeHooks.length > 0 || editAfterHooks.length > 0}>
					<summary class="cursor-pointer p-4 text-sm font-semibold select-none hover:bg-accent/30">
						Hooks
						{#if editBeforeHooks.length > 0 || editAfterHooks.length > 0}
							<span class="ml-2 text-xs font-normal text-muted-foreground">{editBeforeHooks.length + editAfterHooks.length}</span>
						{/if}
					</summary>
					<div class="px-4 pb-4 space-y-4 border-t border-border pt-3">
						<!-- Before message hooks -->
						<div class="space-y-2">
							<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Before Message</h3>
							{#each editBeforeHooks as hook, i}
								<div class="flex items-center gap-2">
									<span class="flex-1 rounded-md border border-border px-3 py-1.5 text-xs font-mono bg-background">{hook}</span>
									<button onclick={() => removeBeforeHook(i)}
										class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground text-xs">&#x2715;</button>
								</div>
							{/each}
							<div class="flex gap-2">
								<input type="text" bind:value={newBeforeHook} placeholder="hook name"
									onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') addBeforeHook(); }}
									class="flex-1 rounded-md border border-border bg-background px-3 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-ring" />
								<button onclick={addBeforeHook} disabled={!newBeforeHook.trim()}
									class="rounded-md border border-border px-2 py-1 text-xs hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
							</div>
						</div>
						<!-- After message hooks -->
						<div class="space-y-2">
							<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">After Message</h3>
							{#each editAfterHooks as hook, i}
								<div class="flex items-center gap-2">
									<span class="flex-1 rounded-md border border-border px-3 py-1.5 text-xs font-mono bg-background">{hook}</span>
									<button onclick={() => removeAfterHook(i)}
										class="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground text-xs">&#x2715;</button>
								</div>
							{/each}
							<div class="flex gap-2">
								<input type="text" bind:value={newAfterHook} placeholder="hook name"
									onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') addAfterHook(); }}
									class="flex-1 rounded-md border border-border bg-background px-3 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-ring" />
								<button onclick={addAfterHook} disabled={!newAfterHook.trim()}
									class="rounded-md border border-border px-2 py-1 text-xs hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed">Add</button>
							</div>
						</div>
					</div>
				</details>

			</div>
		</div>
		{:else}
			<div class="text-sm text-muted-foreground">Loading...</div>
		{/if}
	</div>

	<!-- Save bar (always at bottom) -->
	{#if agent}
		<div class="shrink-0 border-t border-border bg-background px-6 py-3 flex items-center justify-end gap-3">
			<button onclick={saveConfig} disabled={saving}
				class="rounded-md bg-primary px-6 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
				{saving ? 'Saving...' : 'Save Changes'}
			</button>
			{#if saveMessage && !needsRestart}
				<span class="text-xs text-emerald-500">{saveMessage}</span>
			{/if}
		</div>
	{/if}
</div>

{#if showDeleteConfirm && agent}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
		<div class="rounded-lg border border-border bg-card p-6 shadow-lg w-96 space-y-4">
			<h3 class="text-lg font-semibold">Delete Agent</h3>
			<p class="text-sm text-muted-foreground">
				Are you sure you want to delete <strong>{agent.name}</strong>? This will remove the agent and its configuration. This cannot be undone.
			</p>
			<div class="flex gap-2 justify-end">
				<button
					onclick={() => { showDeleteConfirm = false; }}
					class="rounded-md border border-border px-4 py-2 text-sm hover:bg-accent"
				>
					Cancel
				</button>
				<button
					onclick={handleDelete}
					disabled={deleting}
					class="rounded-md bg-destructive px-4 py-2 text-sm font-medium text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50"
				>
					{deleting ? 'Deleting...' : 'Delete Agent'}
				</button>
			</div>
		</div>
	</div>
{/if}
