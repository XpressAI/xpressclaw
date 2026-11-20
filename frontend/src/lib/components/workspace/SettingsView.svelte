<script lang="ts" module>
	import { createLazyComponentRegistry, type AnyLazyComponent } from '$lib/lazyComponent';
	import type { WorkspaceTabKind } from '$lib/workspace';

	const loaders = {
		'settings-server': () => import('../../../routes/settings/server/+page.svelte'),
		'settings-connect': () => import('../../../routes/settings/connect/+page.svelte'),
		'settings-collaboration': () => import('../../../routes/settings/collaboration/+page.svelte'),
		'settings-proxy': () => import('../../../routes/settings/proxy/+page.svelte'),
		'settings-mcp': () => import('../../../routes/settings/mcp/+page.svelte'),
		'settings-sync': () => import('../../../routes/settings/sync/+page.svelte'),
		'settings-speech': () => import('../../../routes/settings/speech/+page.svelte'),
		settings: () => import('../../../routes/settings/+page.svelte')
	};

	type SettingsKey = keyof typeof loaders;
	const settingsViews = createLazyComponentRegistry<SettingsKey>(loaders);
</script>

<script lang="ts">
	import { onMount, untrack } from 'svelte';

	let { kind }: { kind: WorkspaceTabKind } = $props();
	let key = $derived<SettingsKey>(kind in loaders ? (kind as SettingsKey) : 'settings');
	const initialKey = untrack(() => key);
	let initialLoad: Promise<AnyLazyComponent> | null = settingsViews.has(initialKey)
		? null
		: settingsViews.load(initialKey);
	initialLoad?.catch(() => {});

	let ActiveView = $state<AnyLazyComponent | null>(settingsViews.get(initialKey));
	let viewLoadError = $state<string | null>(null);
	let retryNonce = $state(0);
	let failedRetries = $state(0);

	onMount(() => {
		const handleOnline = () => {
			if (viewLoadError) retryNonce += 1;
		};
		window.addEventListener('online', handleOnline);
		return () => window.removeEventListener('online', handleOnline);
	});

	$effect(() => {
		const targetKey = key;
		const currentRetry = retryNonce;
		const cached = settingsViews.get(targetKey);
		if (cached) {
			initialLoad = null;
			ActiveView = cached;
			viewLoadError = null;
			failedRetries = 0;
			return;
		}
		ActiveView = null;
		viewLoadError = null;
		let cancelled = false;
		const pending =
			initialLoad && targetKey === initialKey && currentRetry === 0
				? initialLoad
				: settingsViews.load(targetKey);
		initialLoad = null;
		pending
			.then((component) => {
				if (!cancelled) {
					ActiveView = component;
					viewLoadError = null;
					failedRetries = 0;
				}
			})
			.catch((err) => {
				if (!cancelled) {
					viewLoadError =
						err instanceof Error && err.message ? err.message : 'Could not load settings view.';
					if (currentRetry > 0) failedRetries += 1;
				}
			});
		return () => {
			cancelled = true;
		};
	});
</script>

<div class="h-full min-h-0 overflow-y-auto">
	{#if ActiveView}
		<ActiveView />
	{:else if viewLoadError}
		<div role="alert" data-settings-load-error class="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
			<p class="text-sm text-destructive">Could not load this settings page. Check your connection and try again.</p>
			<div class="flex items-center gap-2">
				<button
					type="button"
					onclick={() => { retryNonce += 1; }}
					class="rounded-md bg-primary px-4 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90"
				>
					Retry
				</button>
				{#if failedRetries > 0}
					<button
						type="button"
						onclick={() => window.location.reload()}
						class="rounded-md border border-border bg-card px-4 py-1.5 text-xs font-medium hover:bg-accent"
					>
						Reload page
					</button>
				{/if}
			</div>
		</div>
	{/if}
</div>
