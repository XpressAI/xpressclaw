<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { conversations, agents } from '$lib/api';
	import type { Conversation, Agent } from '$lib/api';

	const nav = [
		{ href: '/dashboard', label: 'Dashboard', icon: '⊞' },
		{ href: '/agents', label: 'Agents', icon: '◉' },
		{ href: '/tasks', label: 'Tasks', icon: '☐' },
		{ href: '/memory', label: 'Knowledge', icon: '✿' },
		{ href: '/schedules', label: 'Schedules', icon: '⏱' },
		{ href: '/procedures', label: 'Procedures', icon: '≡' },
		{ href: '/budget', label: 'Budget', icon: '◆' },
		{ href: '/settings', label: 'Settings', icon: '⚙' }
	];

	function isActive(href: string, pathname: string): boolean {
		if (href === '/dashboard') return pathname === '/dashboard';
		return pathname.startsWith(href);
	}

	function isConvActive(id: string, pathname: string): boolean {
		return pathname === `/conversations/${id}`;
	}

	let isSetupRoute = $derived($page.url.pathname.startsWith('/setup'));

	let convList = $state<Conversation[]>([]);
	let agentList = $state<Agent[]>([]);
	let showNewChat = $state(false);
	let newChatTitle = $state('');
	let selectedAgents = $state<Set<string>>(new Set());

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
		// Skip sidebar loading during setup
		if (isSetupRoute) return;

		loadSidebar();
		// Refresh sidebar periodically
		const interval = setInterval(loadSidebar, 10000);
		return () => clearInterval(interval);
	});

	async function createConversation() {
		const agentIds = [...selectedAgents];
		const conv = await conversations.create({
			title: newChatTitle || undefined,
			participant_ids: agentIds
		});
		showNewChat = false;
		newChatTitle = '';
		selectedAgents = new Set();
		await loadSidebar();
		goto(`/conversations/${conv.id}`);
	}

	function toggleAgent(id: string) {
		const next = new Set(selectedAgents);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		selectedAgents = next;
	}

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
</script>

{#if isSetupRoute}
	<!-- Setup wizard: full-screen, no sidebar -->
	{@render children()}
{:else}
	<div class="flex h-screen">
		<!-- Sidebar -->
		<aside class="flex w-60 flex-col border-r border-border bg-card">
			<!-- Header -->
			<div class="flex h-14 items-center gap-2 border-b border-border px-4">
				<img src="/icon-32.png" alt="xpressclaw" class="h-7 w-7 rounded-md" />
				<span class="text-sm font-semibold">xpressclaw</span>
			</div>

			<!-- Conversations Section -->
			<div class="flex items-center justify-between px-3 pt-3 pb-1">
				<span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Conversations</span>
				<button
					onclick={() => (showNewChat = !showNewChat)}
					class="flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground text-sm"
					title="New conversation"
				>+</button>
			</div>

			{#if showNewChat}
				<div class="mx-2 mb-2 rounded-lg border border-border bg-background p-2 space-y-2">
					<input
						type="text"
						bind:value={newChatTitle}
						placeholder="Conversation name..."
						class="w-full rounded-md border border-border bg-card px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-ring"
					/>
					<div class="text-xs text-muted-foreground">Add agents:</div>
					<div class="space-y-1 max-h-28 overflow-y-auto">
						{#each agentList as agent}
							<label class="flex items-center gap-2 text-xs cursor-pointer hover:bg-accent/50 rounded px-1 py-0.5">
								<input
									type="checkbox"
									checked={selectedAgents.has(agent.id)}
									onchange={() => toggleAgent(agent.id)}
									class="rounded border-border"
								/>
								<span class="truncate">{agent.name}</span>
							</label>
						{/each}
					</div>
					<div class="flex gap-1">
						<button
							onclick={createConversation}
							class="flex-1 rounded-md bg-primary px-2 py-1 text-xs text-primary-foreground hover:bg-primary/90"
						>Create</button>
						<button
							onclick={() => (showNewChat = false)}
							class="rounded-md border border-border px-2 py-1 text-xs hover:bg-accent"
						>Cancel</button>
					</div>
				</div>
			{/if}

			<!-- Conversation list -->
			<div class="flex-1 overflow-y-auto px-2 space-y-0.5">
				{#each convList as conv}
					{@const active = isConvActive(conv.id, $page.url.pathname)}
					<a
						href="/conversations/{conv.id}"
						class="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors {active
							? 'bg-accent text-accent-foreground font-medium'
							: 'text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground'}"
					>
						<span class="text-xs flex-shrink-0">{convIcon(conv)}</span>
						<span class="truncate">{convLabel(conv)}</span>
					</a>
				{:else}
					<div class="px-2 py-4 text-center text-xs text-muted-foreground">
						No conversations yet
					</div>
				{/each}
			</div>

			<!-- Agents Section -->
			<div class="border-t border-border">
				<div class="px-3 pt-2 pb-1">
					<span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Agents</span>
				</div>
				<div class="px-2 pb-1 space-y-0.5 max-h-28 overflow-y-auto">
					{#each agentList as agent}
						<a
							href="/agents/{agent.id}"
							class="flex items-center gap-2 rounded-md px-2 py-1 text-xs transition-colors text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground"
						>
							<span class="h-1.5 w-1.5 rounded-full flex-shrink-0 {agent.status === 'running' ? 'bg-emerald-400' : 'bg-muted-foreground/30'}"></span>
							<span class="truncate">{agent.name}</span>
						</a>
					{/each}
				</div>
			</div>

			<!-- Navigation Section -->
			<div class="border-t border-border">
				<nav class="p-1.5 space-y-0.5">
					{#each nav as item}
						{@const active = isActive(item.href, $page.url.pathname)}
						<a
							href={item.href}
							class="flex items-center gap-2 rounded-md px-2 py-1 text-xs transition-colors {active
								? 'bg-accent text-accent-foreground font-medium'
								: 'text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground'}"
						>
							<span class="w-3 text-center opacity-60">{item.icon}</span>
							{item.label}
						</a>
					{/each}
				</nav>
			</div>

			<div class="border-t border-border p-2 text-xs text-muted-foreground text-center">
				v0.1.0
			</div>
		</aside>

		<!-- Main content -->
		<main class="flex-1 overflow-auto">
			{@render children()}
		</main>
	</div>
{/if}
