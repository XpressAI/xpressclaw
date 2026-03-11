<script lang="ts">
	import { onMount } from 'svelte';
	import { schedules } from '$lib/api';
	import type { Schedule } from '$lib/api';
	import { timeAgo } from '$lib/utils';

	let scheduleList = $state<Schedule[]>([]);
	let loading = $state(true);
	let showCreate = $state(false);
	let form = $state({ name: '', cron: '', agent_id: '', title: '', description: '' });

	onMount(() => load());

	async function load() {
		scheduleList = await schedules.list().catch(() => []);
		loading = false;
	}

	async function create() {
		if (!form.name || !form.cron || !form.agent_id || !form.title) return;
		await schedules.create({
			name: form.name,
			cron: form.cron,
			agent_id: form.agent_id,
			title: form.title,
			description: form.description || undefined
		});
		form = { name: '', cron: '', agent_id: '', title: '', description: '' };
		showCreate = false;
		await load();
	}

	async function toggle(s: Schedule) {
		if (s.enabled) {
			await schedules.disable(s.id);
		} else {
			await schedules.enable(s.id);
		}
		await load();
	}

	async function trigger(id: string) {
		try {
			const task = await schedules.trigger(id);
			alert(`Task created: ${task.title}`);
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	async function remove(id: string) {
		if (!confirm('Delete this schedule?')) return;
		await schedules.delete(id);
		await load();
	}
</script>

<div class="p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-2xl font-bold">Schedules</h1>
			<p class="text-sm text-muted-foreground mt-1">
				{scheduleList.filter((s) => s.enabled).length} active of {scheduleList.length}
			</p>
		</div>
		<button
			onclick={() => (showCreate = !showCreate)}
			class="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 transition-colors"
		>
			New Schedule
		</button>
	</div>

	{#if showCreate}
		<div class="rounded-lg border border-border bg-card p-4 space-y-3">
			<div class="grid grid-cols-2 gap-3">
				<input type="text" placeholder="Name" bind:value={form.name} class="rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
				<input type="text" placeholder="Cron (e.g. 0 9 * * *)" bind:value={form.cron} class="rounded-md border border-input bg-background px-3 py-2 text-sm font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
				<input type="text" placeholder="Agent ID" bind:value={form.agent_id} class="rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
				<input type="text" placeholder="Task title (use {date}, {time})" bind:value={form.title} class="rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring" />
			</div>
			<textarea placeholder="Description (optional)" bind:value={form.description} rows="2" class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring resize-none"></textarea>
			<div class="flex gap-2">
				<button onclick={create} class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">Create</button>
				<button onclick={() => (showCreate = false)} class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">Cancel</button>
			</div>
		</div>
	{/if}

	{#if loading}
		<div class="text-sm text-muted-foreground">Loading...</div>
	{:else}
		<div class="space-y-2">
			{#each scheduleList as s}
				<div class="rounded-lg border border-border bg-card p-4 flex items-center gap-4">
					<button
						onclick={() => toggle(s)}
						class="h-5 w-9 rounded-full transition-colors {s.enabled ? 'bg-emerald-500' : 'bg-muted'} relative"
						title={s.enabled ? 'Disable' : 'Enable'}
					>
						<div class="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform {s.enabled ? 'translate-x-4' : 'translate-x-0.5'}"></div>
					</button>

					<div class="flex-1 min-w-0">
						<div class="text-sm font-semibold {!s.enabled ? 'text-muted-foreground' : ''}">{s.name}</div>
						<div class="text-xs text-muted-foreground mt-0.5">
							<code class="bg-muted px-1 py-0.5 rounded">{s.cron}</code>
							&middot; {s.agent_id}
							&middot; {s.run_count} runs
							{#if s.last_run}
								&middot; last: {timeAgo(s.last_run)}
							{/if}
						</div>
					</div>

					<div class="flex gap-2 shrink-0">
						<button
							onclick={() => trigger(s.id)}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent transition-colors"
						>
							Run Now
						</button>
						<button
							onclick={() => remove(s.id)}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/10 transition-colors"
						>
							Delete
						</button>
					</div>
				</div>
			{:else}
				<div class="rounded-lg border border-border bg-card p-8 text-center text-sm text-muted-foreground">
					No schedules configured
				</div>
			{/each}
		</div>
	{/if}
</div>
