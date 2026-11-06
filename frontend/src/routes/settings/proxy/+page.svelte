<script lang="ts">
	import { onMount } from 'svelte';
	import { settings, type ProxyConfig, type ProxySettings } from '$lib/api';

	let data = $state<ProxySettings | null>(null);
	let form = $state<ProxyConfig | null>(null);
	let allowedText = $state('');
	let blockedText = $state('');
	let bypassText = $state('');
	let ruleDomainText = $state<string[]>([]);
	let busy = $state(false);
	let error = $state('');
	let notice = $state('');
	const lines = (value: string) => [...new Set(value.split(/[\n,]/).map((line) => line.trim().toLowerCase()).filter(Boolean))];

	onMount(async () => {
		try {
			data = await settings.getProxy();
			form = structuredClone($state.snapshot(data.config));
            allowedText = form.allowed_domains.join('\n');
            blockedText = form.blocked_domains.join('\n');
            bypassText = form.inspection_bypass.join('\n');
            ruleDomainText = form.header_rules.map(rule => rule.domains.join('\n'));
		} catch (cause) { error = String(cause); }
	});

	async function save() {
		if (!form || busy) return;
		busy = true;
		error = '';
		notice = '';
		try {
			form.allowed_domains = lines(allowedText);
            form.blocked_domains = lines(blockedText);
            form.inspection_bypass = lines(bypassText);
            form.header_rules.forEach((rule, index) => { rule.domains = lines(ruleDomainText[index] ?? ''); });
            data = await settings.putProxy(form);
			form = structuredClone($state.snapshot(data.config));
            allowedText = form.allowed_domains.join('\n');
            blockedText = form.blocked_domains.join('\n');
            bypassText = form.inspection_bypass.join('\n');
            ruleDomainText = form.header_rules.map(rule => rule.domains.join('\n'));
			notice = 'Proxy settings saved. Rules apply to new requests; Agent network changes apply on the next turn.';
		} catch (cause) { error = cause instanceof Error ? cause.message : String(cause); }
		finally { busy = false; }
	}

	async function refresh() {
		try { data = await settings.getProxy(); }
		catch (cause) { error = String(cause); }
	}

	function addRule() {
		ruleDomainText.push('');
		form?.header_rules.push({ header: 'User-Agent', matcher: 'email', value: '', action: 'replace', replacement: 'XpressCLAW/1.0', domains: [] });
	}
</script>

<div class="mx-auto max-w-5xl space-y-6 p-4 sm:p-6" data-testid="proxy-settings">
	<div class="flex flex-wrap items-start justify-between gap-4">
		<div>
			<h1 class="text-2xl font-semibold">Proxy</h1>
			<p class="mt-1 text-sm text-muted-foreground">Control the domains and request headers used by your Agents.</p>
		</div>
		{#if data}
			<span class="rounded-full border border-border px-3 py-1 text-xs" aria-label="Proxy status">{data.status.state.replaceAll('_', ' ')}</span>
		{/if}
	</div>
	{#if error}<p role="alert" class="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">{error}</p>{/if}
	{#if notice}<p role="status" class="rounded-lg border border-border bg-muted p-3 text-sm">{notice}</p>{/if}
	{#if !form}
		{#if !error}<p class="text-sm text-muted-foreground">Loading proxy settings…</p>{/if}
	{:else}
		<form onsubmit={(event) => { event.preventDefault(); save(); }} class="space-y-6">
			<fieldset disabled={busy} class="space-y-6">
				<section class="space-y-3 rounded-xl border border-border p-5">
					<label class="flex items-center gap-3 font-medium"><input type="checkbox" bind:checked={form.enabled} /> Enable instance proxy</label>
					<p class="text-sm text-muted-foreground">Agents inherit this setting. Saving with the proxy enabled starts the managed service and configures HTTPS certificate trust automatically.</p>
					{#if data?.host_engine_agents.length}
						<p class="text-sm">Host container-engine access: {data.host_engine_agents.join(', ')}. Set these Agents to connect directly or disable their host-engine access before enabling the proxy.</p>
					{/if}
					<p class="text-sm"><span class="font-medium">Agents connecting directly:</span> {data?.direct_agents.length ? data.direct_agents.join(', ') : 'None'}. Change this in each Agent’s runner settings.</p>
				</section>

				<section class="space-y-4 rounded-xl border border-border p-5">
					<h2 class="text-lg font-medium">Domain access</h2>
					<label class="block space-y-1 text-sm"><span>Default policy</span>
						<select bind:value={form.domain_policy} class="w-full rounded-md border border-input bg-background px-3 py-2">
							<option value="allow_except_blocked">Allow except blocked</option>
							<option value="allow_listed_only">Allow listed domains only</option>
						</select>
					</label>
					<p class="text-xs text-muted-foreground">One hostname per line. example.com matches only that host; *.example.com matches its subdomains. Blocked domains always take precedence.</p>
					<div class="grid gap-4 sm:grid-cols-2">
						<label class="block space-y-1 text-sm"><span>Allowed domains</span>
							<textarea aria-label="Allowed domains" rows="5" bind:value={allowedText} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs" placeholder="api.example.com"></textarea>
							<span class="block text-xs text-muted-foreground">Used in allowlist mode. An empty list permits no external destinations.</span>
						</label>
						<label class="block space-y-1 text-sm"><span>Blocked domains</span>
							<textarea rows="5" bind:value={blockedText} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs" placeholder="*.blocked.example"></textarea>
						</label>
					</div>
					<label class="block space-y-1 text-sm"><span>Skip HTTPS inspection</span>
						<textarea aria-label="Skip HTTPS inspection" rows="3" bind:value={bypassText} class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs" placeholder="pinned.example.com"></textarea>
						<span class="block text-xs text-muted-foreground">These hosts still pass domain checks. Their headers are unchanged. Changes apply to new connections.</span>
					</label>
				</section>

				<section class="space-y-4 rounded-xl border border-border p-5">
					<div class="flex items-center justify-between gap-3"><h2 class="text-lg font-medium">Request header rules</h2><button type="button" onclick={addRule} class="rounded-md border border-border px-3 py-1.5 text-sm">Add rule</button></div>
					<p class="text-xs text-muted-foreground">Rules run in order. Matching a value applies the action to every occurrence of that header. Request and response bodies stream unchanged.</p>
					{#each form.header_rules as rule, index}
						<div class="space-y-3 rounded-lg border border-border bg-muted/20 p-4" data-testid="header-rule">
							<div class="flex items-center justify-between"><h3 class="text-sm font-medium">Rule {index + 1}</h3><button type="button" aria-label={`Remove rule ${index + 1}`} onclick={() => { if (form) form.header_rules = form.header_rules.filter((_, i) => i !== index); ruleDomainText = ruleDomainText.filter((_, i) => i !== index); }} class="text-xs text-muted-foreground hover:text-foreground">Remove</button></div>
							<div class="grid gap-3 sm:grid-cols-2">
								<label class="block space-y-1 text-sm"><span>Header name</span><input required bind:value={rule.header} class="w-full rounded-md border border-input bg-background px-3 py-2" /></label>
								<label class="block space-y-1 text-sm"><span>Match</span><select aria-label="Match" bind:value={rule.matcher} class="w-full rounded-md border border-input bg-background px-3 py-2"><option value="any">Any value</option><option value="email">Contains an email address</option><option value="exact">Exact value</option><option value="contains">Contains text</option><option value="regex">Regular expression</option></select></label>
								{#if ['exact', 'contains', 'regex'].includes(rule.matcher)}<label class="block space-y-1 text-sm"><span>Match value</span><input bind:value={rule.value} maxlength="1024" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono" /></label>{/if}
								<label class="block space-y-1 text-sm"><span>Action</span><select aria-label="Action" bind:value={rule.action} class="w-full rounded-md border border-input bg-background px-3 py-2"><option value="remove">Remove header</option><option value="replace">Replace value</option><option value="block">Block request</option></select></label>
								{#if rule.action === 'replace'}<label class="block space-y-1 text-sm"><span>Replacement value</span><input bind:value={rule.replacement} maxlength="4096" class="w-full rounded-md border border-input bg-background px-3 py-2" /></label>{/if}
							</div>
							<label class="block space-y-1 text-sm"><span>Domains (optional)</span><textarea rows="2" bind:value={ruleDomainText[index]} placeholder="All inspected domains" class="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"></textarea></label>
						</div>
					{/each}
					{#if !form.header_rules.length}<p class="text-sm text-muted-foreground">No header rules configured.</p>{/if}
				</section>
				<button type="submit" class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50">{busy ? 'Saving and checking proxy…' : 'Save proxy settings'}</button>
			</fieldset>
		</form>
		<section class="space-y-3 rounded-xl border border-border p-5">
			<div class="flex items-center justify-between"><h2 class="text-lg font-medium">Recent policy events</h2><button onclick={refresh} class="rounded-md border border-border px-3 py-1.5 text-sm">Refresh events</button></div>
			{#if data?.status.events.length}
				<div class="overflow-x-auto"><table class="w-full text-left text-sm"><thead class="text-xs text-muted-foreground"><tr><th class="p-2">Time</th><th class="p-2">Domain</th><th class="p-2">Action</th><th class="p-2">Rule</th></tr></thead><tbody>{#each data.status.events as event}<tr class="border-t border-border"><td class="whitespace-nowrap p-2">{new Date(event.time * 1000).toLocaleTimeString()}</td><td class="p-2">{event.host || '—'}</td><td class="p-2">{event.action.replaceAll('_', ' ')}</td><td class="p-2">{event.rule ?? '—'}</td></tr>{/each}</tbody></table></div>
			{:else}<p class="text-sm text-muted-foreground">No recent events.</p>{/if}
		</section>
	{/if}
</div>
