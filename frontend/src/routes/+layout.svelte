<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { conversations, agents, apps as appsApi } from '$lib/api';
	import type { Conversation, Agent, App } from '$lib/api';
	import { agentAvatar } from '$lib/utils';

	// Bottom tabs per ADR-016
	const tabs = [
		{ id: 'agents', label: 'Agents', icon: 'M15 19.128a9.38 9.38 0 002.625.372 9.337 9.337 0 004.121-.952 4.125 4.125 0 00-7.533-2.493M15 19.128v-.003c0-1.113-.285-2.16-.786-3.07M15 19.128v.106A12.318 12.318 0 018.624 21c-2.331 0-4.512-.645-6.374-1.766l-.001-.109a6.375 6.375 0 0111.964-3.07M12 6.375a3.375 3.375 0 11-6.75 0 3.375 3.375 0 016.75 0zm8.25 2.25a2.625 2.625 0 11-5.25 0 2.625 2.625 0 015.25 0z' },
		{ id: 'tasks', label: 'Tasks', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4' },
		{ id: 'workflows', label: 'Workflows', icon: 'M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5' },
		{ id: 'settings', label: 'Settings', icon: 'M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z M15 12a3 3 0 11-6 0 3 3 0 016 0z' },
	];

	// Determine active tab from current route
	let activeTab = $derived(
		(() => {
			const p = $page.url.pathname;
			if (p.startsWith('/tasks') || p.startsWith('/schedules')) return 'tasks';
			if (p.startsWith('/procedures') || p.startsWith('/workflows')) return 'workflows';
			if (p.startsWith('/settings') || p.startsWith('/budget')) return 'settings';
			return 'agents'; // default: /, /dashboard, /conversations/*, /agents/*, /memory
		})()
	);

	function isActive(href: string, pathname: string): boolean {
		if (href === '/dashboard') return pathname === '/dashboard';
		return pathname.startsWith(href);
	}

	function isConvActive(id: string, pathname: string): boolean {
		return pathname === `/conversations/${id}`;
	}

	function isAgentActive(id: string, pathname: string): boolean {
		return pathname === `/agents/${id}`;
	}

	let isSetupRoute = $derived($page.url.pathname.startsWith('/setup'));

	let convList = $state<Conversation[]>([]);
	let agentList = $state<Agent[]>([]);
	let appList = $state<App[]>([]);

	let { children } = $props();

	async function loadSidebar() {
		const [c, a, ap] = await Promise.all([
			conversations.list().catch(() => []),
			agents.list().catch(() => []),
			appsApi.list().catch(() => [])
		]);
		convList = c;
		agentList = a;
		appList = ap;
	}

	onMount(() => {
		if (isSetupRoute) return;
		loadSidebar();
		const interval = setInterval(loadSidebar, 10000);
		return () => clearInterval(interval);
	});

	function convIcon(conv: Conversation): string {
		if (conv.icon) return conv.icon;
		const agentCount = conv.participants.filter(p => p.participant_type === 'agent').length;
		return agentCount > 1 ? '👥' : '💬';
	}

	function convLabel(conv: Conversation): string {
		if (conv.title) return conv.title;
		const agentNames = conv.participants
			.filter(p => p.participant_type === 'agent')
			.map(p => p.participant_id);
		return agentNames.length > 0 ? agentNames.join(', ') : 'New Chat';
	}

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
		<aside class="flex w-64 flex-col" style="background: hsl(var(--sidebar))">
			<!-- Header -->
			<div class="flex h-11 items-center gap-2 px-4">
				<img src="/icon-32.png" alt="xpressclaw" class="h-5 w-5 rounded" />
				<span class="text-xs font-medium text-muted-foreground">xpressclaw</span>
			</div>

			<!-- Tab-dependent sidebar content -->
			<div class="flex-1 overflow-y-auto">

				{#if activeTab === 'agents'}
					<!-- AGENTS TAB: Apps, Conversations, Agents, Knowledge -->

					<!-- Apps -->
					<div class="px-3 pb-1">
						<div class="flex items-center justify-between px-1 pb-1.5">
							<span class={sectionHeader}>Apps</span>
						</div>
						<a href="/dashboard" class={linkClass(isActive('/dashboard', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6A2.25 2.25 0 016 3.75h2.25A2.25 2.25 0 0110.5 6v2.25a2.25 2.25 0 01-2.25 2.25H6a2.25 2.25 0 01-2.25-2.25V6zm9.75 0A2.25 2.25 0 0115.75 3.75H18A2.25 2.25 0 0120.25 6v2.25A2.25 2.25 0 0118 10.5h-2.25a2.25 2.25 0 01-2.25-2.25V6zM3.75 15.75A2.25 2.25 0 016 13.5h2.25a2.25 2.25 0 012.25 2.25V18a2.25 2.25 0 01-2.25 2.25H6A2.25 2.25 0 013.75 18v-2.25zm9.75 0A2.25 2.25 0 0115.75 13.5H18a2.25 2.25 0 012.25 2.25V18A2.25 2.25 0 0118 20.25h-2.25A2.25 2.25 0 0113.5 18v-2.25z" /></svg>
							<span>Dashboard</span>
						</a>
						{#each appList as app}
							<a href="/apps/{app.id}" class={linkClass($page.url.pathname === `/apps/${app.id}`)}>
								<span class="text-sm flex-shrink-0">{app.icon ?? '📦'}</span>
								<span class="truncate">{app.title}</span>
							</a>
						{/each}
					</div>

					<!-- Conversations -->
					<div class="px-3 pt-2">
						<div class="flex items-center justify-between px-1 pb-1.5">
							<span class={sectionHeader}>Conversations</span>
							<a href="/" class={plusButton} title="New conversation">+</a>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						{#each convList as conv}
							{@const active = isConvActive(conv.id, $page.url.pathname)}
							<a href="/conversations/{conv.id}" class={linkClass(active)}>
								<span class="text-xs flex-shrink-0">{convIcon(conv)}</span>
								<span class="truncate">{convLabel(conv)}</span>
							</a>
						{:else}
							<div class="px-2 py-3 text-center text-xs text-muted-foreground">
								No conversations yet
							</div>
						{/each}
					</div>

					<!-- Agents -->
					<div class="px-3 pt-3">
						<div class="flex items-center justify-between px-1 pb-1.5">
							<span class={sectionHeader}>Agents</span>
							<a href="/setup?mode=add-agent" class={plusButton} title="Add agent">+</a>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						{#each agentList as agent}
							{@const active = isAgentActive(agent.id, $page.url.pathname)}
							<a href="/agents/{agent.id}" class={linkClass(active)}>
								<img src={agentAvatar(agent)} alt="" class="h-5 w-5 rounded-full flex-shrink-0 object-cover ring-2 {agent.status === 'running' ? 'ring-emerald-400' : agent.status === 'starting' ? 'ring-amber-400' : 'ring-red-400'}" />
								<span class="truncate">{agent.name}</span>
							</a>
						{/each}
					</div>

					<!-- Knowledge -->
					<div class="px-3 pt-3">
						<div class="px-1 pb-1.5">
							<span class={sectionHeader}>Knowledge</span>
						</div>
					</div>
					<div class="px-2">
						<a href="/memory" class={linkClass(isActive('/memory', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 6.042A8.967 8.967 0 006 3.75c-1.052 0-2.062.18-3 .512v14.25A8.987 8.987 0 016 18c2.305 0 4.408.867 6 2.292m0-14.25a8.966 8.966 0 016-2.292c1.052 0 2.062.18 3 .512v14.25A8.987 8.987 0 0018 18a8.967 8.967 0 00-6 2.292m0-14.25v14.25" /></svg>
							<span>Memory</span>
						</a>
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
					<!-- WORKFLOWS TAB: Procedures, Workflows -->

					<div class="px-3 pt-1">
						<div class="px-1 pb-1.5">
							<span class={sectionHeader}>Automation</span>
						</div>
					</div>
					<div class="px-2 space-y-0.5">
						<a href="/procedures" class={linkClass(isActive('/procedures', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 12h16.5m-16.5 3.75h16.5M3.75 19.5h16.5M5.625 4.5h12.75a1.875 1.875 0 010 3.75H5.625a1.875 1.875 0 010-3.75z" /></svg>
							<span>Procedures</span>
						</a>
						<a href="/workflows" class="flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm text-muted-foreground/50 cursor-default">
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M7.5 21L3 16.5m0 0L7.5 12M3 16.5h13.5m0-13.5L21 7.5m0 0L16.5 12M21 7.5H7.5" /></svg>
							<span>Workflows</span>
							<span class="text-[10px] text-muted-foreground/40 ml-auto">soon</span>
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
						<a href="/settings/llm" class={linkClass(isActive('/settings/llm', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M9.813 15.904L9 18.75l-.813-2.846a4.5 4.5 0 00-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 003.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 003.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 00-3.09 3.09zM18.259 8.715L18 9.75l-.259-1.035a3.375 3.375 0 00-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 002.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 002.455 2.456L21.75 6l-1.036.259a3.375 3.375 0 00-2.455 2.456z" /></svg>
							<span>LLM Providers</span>
						</a>
						<a href="/budget" class={linkClass(isActive('/budget', $page.url.pathname))}>
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 6v12m-3-2.818l.879.659c1.171.879 3.07.879 4.242 0 1.172-.879 1.172-2.303 0-3.182C13.536 12.219 12.768 12 12 12c-.725 0-1.45-.22-2.003-.659-1.106-.879-1.106-2.303 0-3.182s2.9-.879 4.006 0l.415.33M21 12a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
							<span>Budgets</span>
						</a>
						<a href="/settings/connectors" class="flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm text-muted-foreground/50 cursor-default">
							<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m13.35-.622l1.757-1.757a4.5 4.5 0 00-6.364-6.364l-4.5 4.5a4.5 4.5 0 001.242 7.244" /></svg>
							<span>Connectors</span>
							<span class="text-[10px] text-muted-foreground/40 ml-auto">soon</span>
						</a>
					</div>
				{/if}

			</div>

			<!-- Bottom Tab Bar -->
			<div class="border-t border-border/50 px-2 py-2">
				<div class="flex items-center justify-around">
					{#each tabs as tab}
						{@const active = activeTab === tab.id}
						<a
							href={tab.id === 'agents' ? '/' : tab.id === 'tasks' ? '/tasks' : tab.id === 'workflows' ? '/procedures' : '/settings'}
							class="flex flex-col items-center gap-1 rounded-lg px-3 py-1.5 text-[10px] transition-colors {active
								? 'text-primary'
								: 'text-muted-foreground hover:text-foreground'}"
							title={tab.label}
						>
							<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d={tab.icon} />
							</svg>
							<span>{tab.label}</span>
						</a>
					{/each}
				</div>
			</div>
		</aside>

		<!-- Main content -->
		<main class="flex-1 overflow-auto">
			{@render children()}
		</main>
	</div>
{/if}
