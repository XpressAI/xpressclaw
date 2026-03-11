<script lang="ts">
	import '../app.css';
	import { page } from '$app/stores';

	const nav = [
		{ href: '/', label: 'Dashboard', icon: 'home' },
		{ href: '/agents', label: 'Agents', icon: 'bot' },
		{ href: '/tasks', label: 'Tasks', icon: 'list-checks' },
		{ href: '/memory', label: 'Memory', icon: 'brain' },
		{ href: '/schedules', label: 'Schedules', icon: 'clock' },
		{ href: '/procedures', label: 'Procedures', icon: 'file-text' },
		{ href: '/budget', label: 'Budget', icon: 'wallet' },
		{ href: '/settings', label: 'Settings', icon: 'settings' }
	];

	function isActive(href: string, pathname: string): boolean {
		if (href === '/') return pathname === '/';
		return pathname.startsWith(href);
	}

	let { children } = $props();
</script>

<div class="flex h-screen">
	<!-- Sidebar -->
	<aside class="flex w-56 flex-col border-r border-border bg-card">
		<div class="flex h-14 items-center gap-2 border-b border-border px-4">
			<div class="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground text-sm font-bold">
				x
			</div>
			<span class="text-sm font-semibold">xpressclaw</span>
		</div>

		<nav class="flex-1 space-y-1 p-2">
			{#each nav as item}
				{@const active = isActive(item.href, $page.url.pathname)}
				<a
					href={item.href}
					class="flex items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors {active
						? 'bg-accent text-accent-foreground font-medium'
						: 'text-muted-foreground hover:bg-accent/50 hover:text-accent-foreground'}"
				>
					<span class="w-4 text-center text-xs opacity-60">
						{#if item.icon === 'home'}&#9632;
						{:else if item.icon === 'bot'}&#9673;
						{:else if item.icon === 'list-checks'}&#9744;
						{:else if item.icon === 'brain'}&#10047;
						{:else if item.icon === 'clock'}&#9201;
						{:else if item.icon === 'file-text'}&#9776;
						{:else if item.icon === 'wallet'}&#9830;
						{:else if item.icon === 'settings'}&#9881;
						{/if}
					</span>
					{item.label}
				</a>
			{/each}
		</nav>

		<div class="border-t border-border p-3 text-xs text-muted-foreground">
			v0.1.0
		</div>
	</aside>

	<!-- Main content -->
	<main class="flex-1 overflow-auto">
		{@render children()}
	</main>
</div>
