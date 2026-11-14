<script lang="ts" module>
	import type { Component } from 'svelte';
	import type { WorkspaceTabKind } from '$lib/workspace';

	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	type AnyComponent = Component<any>;

	const loaders: Record<string, () => Promise<{ default: AnyComponent }>> = {
		'settings-server': () => import('../../../routes/settings/server/+page.svelte'),
		'settings-connect': () => import('../../../routes/settings/connect/+page.svelte'),
		'settings-collaboration': () => import('../../../routes/settings/collaboration/+page.svelte'),
		'settings-proxy': () => import('../../../routes/settings/proxy/+page.svelte'),
		'settings-mcp': () => import('../../../routes/settings/mcp/+page.svelte'),
		'settings-sync': () => import('../../../routes/settings/sync/+page.svelte'),
		'settings-speech': () => import('../../../routes/settings/speech/+page.svelte'),
		settings: () => import('../../../routes/settings/+page.svelte')
	};

	const resolvedViews = new Map<string, AnyComponent>();
	const inflightViews = new Map<string, Promise<AnyComponent>>();

	function loadSettingsView(key: string): Promise<AnyComponent> {
		const cached = resolvedViews.get(key);
		if (cached) return Promise.resolve(cached);
		const inflight = inflightViews.get(key);
		if (inflight) return inflight;
		const loader = loaders[key] ?? loaders.settings;
		const pending = loader().then((mod) => {
			resolvedViews.set(key, mod.default);
			inflightViews.delete(key);
			return mod.default;
		});
		inflightViews.set(key, pending);
		return pending;
	}
</script>

<script lang="ts">
	import { untrack } from 'svelte';

	let { kind }: { kind: WorkspaceTabKind } = $props();
	let key = $derived(kind in loaders ? kind : 'settings');
	const initialKey = untrack(() => key);
	if (!resolvedViews.has(initialKey)) void loadSettingsView(initialKey);
	let ActiveView = $state<AnyComponent | null>(resolvedViews.get(initialKey) ?? null);

	$effect(() => {
		const targetKey = key;
		const cached = resolvedViews.get(targetKey);
		if (cached) {
			ActiveView = cached;
			return;
		}
		ActiveView = null;
		let cancelled = false;
		void loadSettingsView(targetKey).then((component) => {
			if (!cancelled) ActiveView = component;
		});
		return () => {
			cancelled = true;
		};
	});
</script>

<div class="h-full min-h-0 overflow-y-auto">
	{#if ActiveView}
		<ActiveView />
	{/if}
</div>
