<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { agents } from '$lib/api';
	import type { Agent } from '$lib/api';
	import { harnessMark } from '$lib/utils';

	// Bottom tabs per ADR-016
	const tabs = [
		{ id: 'agents', label: 'Sessions', icon: 'M15 19.128a9.38 9.38 0 002.625.372 9.337 9.337 0 004.121-.952 4.125 4.125 0 00-7.533-2.493M15 19.128v-.003c0-1.113-.285-2.16-.786-3.07M15 19.128v.106A12.318 12.318 0 018.624 21c-2.331 0-4.512-.645-6.374-1.766l-.001-.109a6.375 6.375 0 0111.964-3.07M12 6.375a3.375 3.375 0 11-6.75 0 3.375 3.375 0 016.75 0zm8.25 2.25a2.625 2.625 0 11-5.25 0 2.625 2.625 0 015.25 0z' },
		{ id: 'tasks', label: 'Tasks', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4' },
		{ id: 'workflows', label: 'Workflows', icon: 'M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5' },
		{ id: 'settings', label: 'Settings', icon: 'M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z M15 12a3 3 0 11-6 0 3 3 0 016 0z' },
	];

	// Determine active tab from current route
	let activeTab = $derived(
		(() => {
			const p = $page.url.pathname;
			if (p.startsWith('/tasks') || p.startsWith('/schedules')) return 'tasks';
			if (p.startsWith('/workflows')) return 'workflows';
			if (p.startsWith('/settings') || p.startsWith('/budget')) return 'settings';
			return 'agents';
		})()
	);

	function isActive(href: string, pathname: string): boolean {
		if (href === '/dashboard') return pathname === '/dashboard';
		return pathname.startsWith(href);
	}

	function isAgentActive(id: string, pathname: string): boolean {
		return pathname === `/agents/${id}`;
	}

	let isSetupRoute = $derived($page.url.pathname.startsWith('/setup'));

	let agentList = $state<Agent[]>([]);

	// Docker status — checked periodically
	let dockerAvailable = $state(true);
	let dockerInstalled = $state(true);
	let dockerCanStart = $state(false);
	let dockerStarting = $state(false);

	let { children } = $props();

	// Collapsible sidebar — expanded by default, manual toggle only
	let sidebarCollapsed = $state(false);

	function toggleSidebar() {
		sidebarCollapsed = !sidebarCollapsed;
	}

	async function loadSidebar() {
		agentList = await agents.list().catch(() => []);
	}

	async function checkDocker() {
		try {
			const resp = await fetch('/api/setup/check-docker');
			const data = await resp.json();
			dockerAvailable = data.available;
			dockerInstalled = data.installed;
			dockerCanStart = data.can_start;
		} catch {
			// Server itself is down — don't show Docker modal
		}
	}

	async function startDocker() {
		dockerStarting = true;
		try {
			await fetch('/api/setup/start-docker', { method: 'POST' });
			// Poll until Docker is available (up to 60s)
			for (let i = 0; i < 30; i++) {
				await new Promise(r => setTimeout(r, 2000));
				await checkDocker();
				if (dockerAvailable) break;
			}
		} catch {}
		dockerStarting = false;
	}

	// Connection health monitor — detect when server goes away and
	// auto-reload when it comes back. Requires two consecutive poll
	// failures before showing the modal so a single hiccup doesn't
	// flash it (Tauri's WebKitGTK webview is especially flaky here).
	let serverConnected = $state(true);
	let wasDisconnected = false;
	let consecutiveFailures = 0;
	const FAILURES_BEFORE_DISCONNECT = 2;
	const HEALTH_TIMEOUT_MS = 8000;

	async function checkConnection() {
		try {
			const resp = await fetch('/api/health', { signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS) });
			if (resp.ok) {
				consecutiveFailures = 0;
				if (wasDisconnected) {
					// Server is back — reload the page to reset all state
					wasDisconnected = false;
					serverConnected = true;
					window.location.reload();
					return;
				}
				serverConnected = true;
				return;
			}
		} catch {
			// fall through to failure handling
		}
		consecutiveFailures += 1;
		if (consecutiveFailures >= FAILURES_BEFORE_DISCONNECT) {
			serverConnected = false;
			wasDisconnected = true;
		}
	}

	onMount(() => {
		if (isSetupRoute) return;
		loadSidebar();
		checkDocker();
		const interval = setInterval(() => {
			loadSidebar();
			checkConnection();
			if (!dockerAvailable) checkDocker();
		}, 5000);
		return () => clearInterval(interval);
	});

	// Sidebar link helper
	const linkClass = (active: boolean) =>
		`flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm transition-colors ${active
			? 'bg-[hsl(var(--sidebar-active))] text-foreground font-medium'
			: 'text-muted-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] hover:text-foreground'}`;

	const sectionHeader = 'text-[11px] font-semibold text-muted-foreground uppercase tracking-widest';

	const plusButton = 'flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] text-sm transition-colors';
</script>

{#if isSetupRoute}
	{@render children()}
{:else}
	<div class="flex h-screen">
		<!-- Sidebar -->
		<aside class="flex flex-col transition-all duration-200 ease-in-out {sidebarCollapsed ? 'w-12' : 'w-64'}" style="background: hsl(var(--sidebar))">
			<!-- Header -->
			<div class="flex h-11 items-center {sidebarCollapsed ? 'justify-center px-0' : 'gap-2 px-4'}">
				{#if sidebarCollapsed}
					<button onclick={toggleSidebar} class="flex items-center justify-center h-7 w-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent/50 transition-colors" title="Expand sidebar">
						<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5" /></svg>
					</button>
				{:else}
					<img src="/icon-32.png" alt="xpressclaw" class="h-5 w-5 rounded flex-shrink-0" />
					<span class="text-xs font-medium text-muted-foreground flex-1">xpressclaw</span>
					<button onclick={toggleSidebar} class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent/50 transition-colors" title="Collapse sidebar">
						<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" /></svg>
					</button>
				{/if}
			</div>

			<!-- Tab-dependent sidebar content (hidden when collapsed) -->
			<div class="flex-1 overflow-y-auto {sidebarCollapsed ? 'hidden' : ''}">

				{#if activeTab === 'agents'}
					<!-- Native sessions -->
					<div class="px-3 pt-1">
						<div class="flex items-center justify-between px-1 pb-1.5">
							<span class={sectionHeader}>Sessions</span>
							<a href="/setup?mode=add-session" class={plusButton} title="Add session">+</a>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						<a href="/agents" class={linkClass($page.url.pathname === '/agents')}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M18 18.72a9.094 9.094 0 003.741-.479 3 3 0 00-4.682-2.72m.94 3.198A5.995 5.995 0 0012 12.75a5.995 5.995 0 00-5.058 2.772M15 6.75a3 3 0 11-6 0 3 3 0 016 0z" /></svg>
							<span>All sessions</span>
						</a>
						{#each agentList as agent}
							{@const active = isAgentActive(agent.id, $page.url.pathname)}
							<a href="/agents/{agent.id}" class={linkClass(active)}>
								<span class="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted text-[10px] font-semibold">{harnessMark(agent.backend)}</span>
								<span class="truncate">{agent.title || agent.name}</span>
							</a>
						{:else}
							<a href="/setup?mode=add-session" class="block rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-muted-foreground hover:border-primary/40 hover:text-foreground">Create your first session</a>
						{/each}
					</div>

				{:else if activeTab === 'tasks'}
					<!-- TASKS TAB: Tasks, Schedules -->

					<div class="px-3 pt-1">
						<div class="px-1 pb-1.5">
							<span class={sectionHeader}>Work</span>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						<a href="/tasks" class={linkClass(isActive('/tasks', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4" /></svg>
							<span>Tasks</span>
						</a>
						<a href="/schedules" class={linkClass(isActive('/schedules', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
							<span>Schedules</span>
						</a>
					</div>

				{:else if activeTab === 'workflows'}
					<!-- Multi-session workflows -->

					<div class="px-3 pt-1">
						<div class="px-1 pb-1.5">
							<span class={sectionHeader}>Automation</span>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						<a href="/workflows" class={linkClass(isActive('/workflows', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" /></svg>
							<span>Workflows</span>
						</a>
					</div>

				{:else if activeTab === 'settings'}
					<!-- SETTINGS TAB -->

					<div class="px-3 pt-1">
						<div class="px-1 pb-1.5">
							<span class={sectionHeader}>Configuration</span>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						<a href="/settings" class={linkClass($page.url.pathname === '/settings')}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15.75 6a3.75 3.75 0 11-7.5 0 3.75 3.75 0 017.5 0zM4.501 20.118a7.5 7.5 0 0114.998 0A17.933 17.933 0 0112 21.75c-2.676 0-5.216-.584-7.499-1.632z" /></svg>
							<span>Profile</span>
						</a>
						<a href="/settings/server" class={linkClass(isActive('/settings/server', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M5.25 14.25h13.5m-13.5 0a3 3 0 01-3-3m3 3a3 3 0 100 6h13.5a3 3 0 100-6m-16.5-3a3 3 0 013-3h13.5a3 3 0 013 3m-19.5 0a4.5 4.5 0 01.9-2.7L5.737 5.1a3.375 3.375 0 012.7-1.35h7.126c1.062 0 2.062.5 2.7 1.35l2.587 3.45a4.5 4.5 0 01.9 2.7m0 0a3 3 0 01-3 3m0 3h.008v.008h-.008v-.008zm0-6h.008v.008h-.008v-.008zm-3 6h.008v.008h-.008v-.008zm0-6h.008v.008h-.008v-.008z" /></svg>
							<span>Server</span>
						</a>
						<a href="/settings/connectors" class={linkClass(isActive('/settings/connectors', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m13.35-.622l1.757-1.757a4.5 4.5 0 00-6.364-6.364l-4.5 4.5a4.5 4.5 0 001.242 7.244" /></svg>
							<span>Connectors</span>
						</a>
					</div>
				{/if}

			</div>

			<!-- Bottom Tab Bar -->
			<div class="border-t border-border/50 px-1 py-2">
				<div class="{sidebarCollapsed ? 'flex flex-col items-center gap-1' : 'flex items-center justify-around'}">
					{#each tabs as tab}
						{@const active = activeTab === tab.id}
						<a
							href={tab.id === 'agents' ? '/' : tab.id === 'tasks' ? '/tasks' : tab.id === 'workflows' ? '/workflows' : '/settings'}
							class="flex flex-col items-center gap-1 rounded-lg px-3 py-1.5 text-[10px] transition-colors {active
								? 'text-primary'
								: 'text-muted-foreground hover:text-foreground'}"
							title={tab.label}
						>
							<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d={tab.icon} />
							</svg>
							{#if !sidebarCollapsed}<span>{tab.label}</span>{/if}
						</a>
					{/each}
				</div>
			</div>
		</aside>

		<!-- Main content -->
		<main class="flex-1 overflow-hidden">
			<div class="h-full overflow-y-auto flex flex-col">
				{@render children()}
			</div>
		</main>
	</div>

	<!-- Server disconnected overlay -->
	{#if !serverConnected}
		<div class="fixed inset-0 z-[200] flex items-center justify-center bg-black/70 backdrop-blur-sm">
			<div class="mx-4 w-full max-w-xs rounded-xl border border-border bg-card p-6 shadow-2xl text-center space-y-3">
				<div class="inline-flex h-10 w-10 items-center justify-center rounded-full bg-amber-500/10">
					<svg class="h-5 w-5 text-amber-500 animate-pulse" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
						<path stroke-linecap="round" stroke-linejoin="round" d="M8.288 15.038a5.25 5.25 0 017.424 0M5.106 11.856c3.807-3.808 9.98-3.808 13.788 0M1.924 8.674c5.565-5.565 14.587-5.565 20.152 0M12 20.25h.008v.008H12v-.008z" />
					</svg>
				</div>
				<h3 class="text-sm font-semibold">Reconnecting...</h3>
				<p class="text-xs text-muted-foreground">Lost connection to the server. Will reconnect automatically.</p>
			</div>
		</div>
	{/if}

	<!-- Docker unavailable modal -->
	{#if !dockerAvailable && !isSetupRoute}
		<div class="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm">
			<div class="mx-4 w-full max-w-sm rounded-xl border border-border bg-card p-6 shadow-2xl space-y-4">
				<div class="flex items-center gap-3">
					<div class="w-10 h-10 rounded-full bg-amber-500/10 flex items-center justify-center">
						<svg xmlns="http://www.w3.org/2000/svg" class="w-5 h-5 text-amber-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
							<path stroke-linecap="round" stroke-linejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" />
						</svg>
					</div>
					<div>
						<h3 class="text-sm font-semibold">Docker is not running</h3>
						<p class="text-xs text-muted-foreground">
							{#if !dockerInstalled}
								Docker Desktop is not installed. Native workers need Docker to run.
							{:else}
								Docker Desktop is installed but not running. Start it to run queued work.
							{/if}
						</p>
					</div>
				</div>
				<div class="flex justify-end gap-2">
					<button onclick={() => { dockerAvailable = true; }}
						class="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-secondary transition-colors">
						Dismiss
					</button>
					{#if dockerCanStart}
						<button onclick={startDocker} disabled={dockerStarting}
							class="rounded-lg bg-primary px-3 py-1.5 text-xs text-primary-foreground hover:bg-primary/90 disabled:opacity-50 transition-colors">
							{#if dockerStarting}Starting Docker...{:else}Start Docker{/if}
						</button>
					{/if}
				</div>
			</div>
		</div>
	{/if}
{/if}
