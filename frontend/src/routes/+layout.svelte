<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { conversations, agents } from '$lib/api';
	import type { Conversation, Agent } from '$lib/api';
	import { agentAvatar } from '$lib/utils';

	const bottomNav = [
		{ href: '/dashboard', label: 'Apps', icon: 'M4 6a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2H6a2 2 0 01-2-2V6zm10 0a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2h-2a2 2 0 01-2-2V6zM4 16a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2H6a2 2 0 01-2-2v-2zm10 0a2 2 0 012-2h2a2 2 0 012 2v2a2 2 0 01-2 2h-2a2 2 0 01-2-2v-2z' },
		{ href: '/tasks', label: 'Tasks', icon: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4' },
		{ href: '/schedules', label: 'Schedules', icon: 'M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z' },
		{ href: '/budget', label: 'Budget', icon: 'M12 8c-1.657 0-3 .895-3 2s1.343 2 3 2 3 .895 3 2-1.343 2-3 2m0-8c1.11 0 2.08.402 2.599 1M12 8V7m0 1v8m0 0v1m0-1c-1.11 0-2.08-.402-2.599-1M21 12a9 9 0 11-18 0 9 9 0 0118 0z' },
	];

	const moreNav = [
		{ href: '/memory', label: 'Knowledge' },
		{ href: '/procedures', label: 'Procedures' },
		{ href: '/settings', label: 'Settings' },
	];

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

	let { children } = $props();

	async function loadSidebar() {
		const [c, a] = await Promise.all([
			conversations.list().catch(() => []),
			agents.list().catch(() => [])
		]);
		convList = c;
		agentList = a;
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

	function convAgent(conv: Conversation): Agent | undefined {
		const agentId = conv.participants.find(p => p.participant_type === 'agent')?.participant_id;
		return agentId ? agentList.find(a => a.id === agentId) : undefined;
	}
</script>

{#if isSetupRoute}
	{@render children()}
{:else}
	<div class="flex h-screen">
		<!-- Sidebar -->
		<aside class="flex w-64 flex-col" style="background: hsl(var(--sidebar))">
			<!-- Titlebar drag region with logo (padded for macOS traffic lights) -->
			<div class="flex h-11 items-center gap-2 pl-[76px] pr-4" data-tauri-drag-region>
				<img src="/icon-32.png" alt="xpressclaw" class="h-5 w-5 rounded pointer-events-none" />
				<span class="text-xs font-medium text-muted-foreground pointer-events-none">xpressclaw</span>
			</div>

			<!-- Apps Section -->
			<div class="px-3 pb-1">
				<div class="flex items-center justify-between px-1 pb-1.5">
					<span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-widest">Apps</span>
				</div>
				<a href="/dashboard" class="flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm transition-colors
					{isActive('/dashboard', $page.url.pathname)
						? 'bg-[hsl(var(--sidebar-active))] text-foreground font-medium'
						: 'text-muted-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] hover:text-foreground'}">
					<svg class="h-4 w-4 flex-shrink-0" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 6A2.25 2.25 0 016 3.75h2.25A2.25 2.25 0 0110.5 6v2.25a2.25 2.25 0 01-2.25 2.25H6a2.25 2.25 0 01-2.25-2.25V6zm9.75 0A2.25 2.25 0 0115.75 3.75H18A2.25 2.25 0 0120.25 6v2.25A2.25 2.25 0 0118 10.5h-2.25a2.25 2.25 0 01-2.25-2.25V6zM3.75 15.75A2.25 2.25 0 016 13.5h2.25a2.25 2.25 0 012.25 2.25V18a2.25 2.25 0 01-2.25 2.25H6A2.25 2.25 0 013.75 18v-2.25zm9.75 0A2.25 2.25 0 0115.75 13.5H18a2.25 2.25 0 012.25 2.25V18A2.25 2.25 0 0118 20.25h-2.25A2.25 2.25 0 0113.5 18v-2.25z" /></svg>
					<span>Dashboard</span>
				</a>
			</div>

			<!-- Conversations Section -->
			<div class="flex items-center justify-between px-4 pt-2 pb-1.5">
				<span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-widest">Conversations</span>
				<a
					href="/"
					class="flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] text-sm transition-colors"
					title="New conversation"
				>+</a>
			</div>

			<!-- Conversation list -->
			<div class="flex-1 overflow-y-auto px-2 space-y-0.5">
				{#each convList as conv}
					{@const active = isConvActive(conv.id, $page.url.pathname)}
					{@const agent = convAgent(conv)}
					<a
						href="/conversations/{conv.id}"
						class="flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm transition-colors {active
							? 'bg-[hsl(var(--sidebar-active))] text-foreground font-medium'
							: 'text-muted-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] hover:text-foreground'}"
					>
						{#if agent}
							<img src={agentAvatar(agent)} alt="" class="h-5 w-5 rounded-full flex-shrink-0 object-cover" />
						{:else}
							<span class="flex h-5 w-5 items-center justify-center rounded-full text-xs flex-shrink-0 bg-muted">{convIcon(conv)}</span>
						{/if}
						<span class="truncate">{convLabel(conv)}</span>
					</a>
				{:else}
					<div class="px-2 py-4 text-center text-xs text-muted-foreground">
						No conversations yet
					</div>
				{/each}
			</div>

			<!-- Agents Section -->
			<div class="border-t border-border/50">
				<div class="flex items-center justify-between px-4 pt-2.5 pb-1.5">
					<span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-widest">Agents</span>
					<a
						href="/setup?mode=add-agent"
						class="flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] text-sm transition-colors"
						title="Add agent"
					>+</a>
				</div>
				<div class="px-2 pb-2 space-y-0.5 max-h-44 overflow-y-auto">
					{#each agentList as agent}
						{@const active = isAgentActive(agent.id, $page.url.pathname)}
						<a
							href="/agents/{agent.id}"
							class="flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm transition-colors {active
								? 'bg-[hsl(var(--sidebar-active))] text-foreground font-medium'
								: 'text-muted-foreground hover:bg-[hsl(var(--sidebar-active)/.5)] hover:text-foreground'}"
						>
							<img src={agentAvatar(agent)} alt="" class="h-5 w-5 rounded-full flex-shrink-0 object-cover" />
							<span class="truncate">{agent.name}</span>
						</a>
					{/each}
				</div>
			</div>

			<!-- More nav links (small) -->
			<div class="border-t border-border/50 px-2 py-1.5">
				{#each moreNav as item}
					{@const active = isActive(item.href, $page.url.pathname)}
					<a
						href={item.href}
						class="block rounded-md px-2.5 py-1 text-xs transition-colors {active
							? 'text-foreground font-medium'
							: 'text-muted-foreground hover:text-foreground'}"
					>{item.label}</a>
				{/each}
			</div>

			<!-- Bottom Navigation Bar -->
			<div class="border-t border-border/50 px-2 py-2">
				<div class="flex items-center justify-around">
					{#each bottomNav as item}
						{@const active = isActive(item.href, $page.url.pathname)}
						<a
							href={item.href}
							class="flex flex-col items-center gap-1 rounded-lg px-3 py-1.5 text-[10px] transition-colors {active
								? 'text-primary'
								: 'text-muted-foreground hover:text-foreground'}"
							title={item.label}
						>
							<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.5" viewBox="0 0 24 24">
								<path stroke-linecap="round" stroke-linejoin="round" d={item.icon} />
							</svg>
							<span>{item.label}</span>
						</a>
					{/each}
				</div>
			</div>
		</aside>

		<!-- Main content -->
		<main class="flex-1 overflow-auto">
			<!-- Drag region for main area titlebar -->
			<div class="h-11 w-full" data-tauri-drag-region></div>
			<div class="h-[calc(100%-2.75rem)] overflow-auto">
				{@render children()}
			</div>
		</main>
	</div>
{/if}
