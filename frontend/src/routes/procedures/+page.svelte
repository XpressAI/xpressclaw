<script lang="ts">
	import { onMount } from 'svelte';
	import { procedures } from '$lib/api';
	import type { Sop } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import BlockConnector from '$lib/components/blocks/BlockConnector.svelte';
	import yaml from 'js-yaml';

	let sopList = $state<Sop[]>([]);
	let loading = $state(true);
	let showCreate = $state(false);
	let editingId = $state<string | null>(null);
	let form = $state({ name: '', description: '' });
	let toast = $state<{ message: string; type: 'success' | 'error' } | null>(null);
	let confirmDeleteId = $state<string | null>(null);

	// Procedure editor state
	interface ProcStep {
		name: string;
		description: string;
		tools: string[];
		optional: boolean;
		expanded: boolean;
	}
	let editSummary = $state('');
	let editTools = $state<string[]>([]);
	let editInputs = $state<{ name: string; description: string; required: boolean }[]>([]);
	let editOutputs = $state<{ name: string; description: string }[]>([]);
	let editSteps = $state<ProcStep[]>([]);
	let saving = $state(false);
	let showYaml = $state(false);
	let yamlContent = $state('');

	onMount(() => load());

	async function load() {
		sopList = await procedures.list().catch(() => []);
		loading = false;
	}

	function showToast(message: string, type: 'success' | 'error') {
		toast = { message, type }; setTimeout(() => { toast = null; }, 3000);
	}

	async function create() {
		if (!form.name) return;
		try {
			const content = yaml.dump({
				summary: form.description || 'New procedure',
				tools: [], inputs: [], outputs: [],
				steps: [{ name: 'Step 1', description: 'First step' }]
			});
			const sop = await procedures.create({ name: form.name, description: form.description, content });
			showCreate = false;
			form = { name: '', description: '' };
			await load();
			startEditing(sop);
		} catch (e) { showToast(`Create failed: ${e}`, 'error'); }
	}

	function startEditing(sop: Sop) {
		editingId = sop.id;
		try {
			const parsed = yaml.load(sop.content) as any;
			editSummary = parsed?.summary || '';
			editTools = parsed?.tools || [];
			editInputs = (parsed?.inputs || []).map((i: any) => ({ name: i.name, description: i.description || '', required: i.required ?? true }));
			editOutputs = (parsed?.outputs || []).map((o: any) => ({ name: o.name, description: o.description || '' }));
			editSteps = (parsed?.steps || []).map((s: any) => ({
				name: s.name, description: s.description || '', tools: s.tools || [], optional: s.optional ?? false, expanded: false
			}));
		} catch {
			editSummary = ''; editTools = []; editInputs = []; editOutputs = [];
			editSteps = [{ name: 'Step 1', description: '', tools: [], optional: false, expanded: true }];
		}
	}

	function stepsToYaml(): string {
		return yaml.dump({
			summary: editSummary,
			tools: editTools.filter(t => t),
			inputs: editInputs.filter(i => i.name),
			outputs: editOutputs.filter(o => o.name),
			steps: editSteps.map(s => {
				const step: Record<string, unknown> = { name: s.name, description: s.description };
				if (s.tools.length) step.tools = s.tools;
				if (s.optional) step.optional = true;
				return step;
			})
		}, { lineWidth: -1, noRefs: true });
	}

	async function save() {
		if (!editingId) return;
		const sop = sopList.find(s => s.id === editingId);
		if (!sop) return;
		saving = true;
		try {
			const content = stepsToYaml();
			await procedures.update(sop.name, { content });
			showToast('Saved', 'success');
			await load();
		} catch (e) { showToast(`Save failed: ${e}`, 'error'); }
		saving = false;
	}

	async function deleteProcedure(id: string) {
		const sop = sopList.find(s => s.id === id);
		if (!sop) return;
		try {
			await procedures.delete(sop.name);
			confirmDeleteId = null;
			if (editingId === id) editingId = null;
			await load();
		} catch {}
	}

	function addStep() {
		editSteps = [...editSteps, { name: `Step ${editSteps.length + 1}`, description: '', tools: [], optional: false, expanded: true }];
	}

	function removeStep(idx: number) {
		editSteps = editSteps.filter((_, i) => i !== idx);
	}

	function moveStep(from: number, to: number) {
		if (from === to) return;
		const s = [...editSteps];
		const [item] = s.splice(from, 1);
		s.splice(to > from ? to - 1 : to, 0, item);
		editSteps = s;
	}

	let dragIdx = $state<number | null>(null);
	let dragOverIdx = $state<number | null>(null);
</script>

<div class="flex h-full overflow-hidden">
	<!-- Left: Procedure list -->
	<div class="w-72 flex-shrink-0 border-r border-border overflow-y-auto">
		<div class="p-4">
			<div class="flex items-center justify-between mb-3">
				<h1 class="text-lg font-bold">Procedures</h1>
				<button onclick={() => { showCreate = true; }}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">New</button>
			</div>

			{#if showCreate}
				<div class="rounded-lg border border-border bg-card p-3 space-y-2 mb-3">
					<input type="text" placeholder="Name" bind:value={form.name}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
					<input type="text" placeholder="Description" bind:value={form.description}
						class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
					<div class="flex gap-2">
						<button onclick={create} class="rounded bg-primary px-2.5 py-1 text-[10px] text-primary-foreground">Create</button>
						<button onclick={() => (showCreate = false)} class="rounded border border-border px-2.5 py-1 text-[10px]">Cancel</button>
					</div>
				</div>
			{/if}

			{#if loading}
				<div class="text-xs text-muted-foreground">Loading...</div>
			{:else}
				<div class="space-y-1">
					{#each sopList as sop}
						<div class="group flex items-center gap-1">
							<button onclick={() => startEditing(sop)}
								class="flex-1 text-left rounded-lg px-3 py-2 text-xs transition-colors
									{editingId === sop.id ? 'bg-accent text-foreground font-medium' : 'hover:bg-accent/50 text-muted-foreground'}">
								<div class="font-medium text-foreground">{sop.name}</div>
								{#if sop.description}
									<div class="text-[10px] text-muted-foreground truncate">{sop.description}</div>
								{/if}
							</button>
							{#if confirmDeleteId === sop.id}
								<span class="flex items-center gap-1 text-[10px] pr-1">
									<button onclick={() => deleteProcedure(sop.id)} class="text-destructive hover:underline">del</button>
									<button onclick={() => (confirmDeleteId = null)} class="text-muted-foreground hover:underline">no</button>
								</span>
							{:else}
								<button onclick={() => (confirmDeleteId = sop.id)}
									class="opacity-0 group-hover:opacity-100 text-muted-foreground/30 hover:text-destructive pr-1">
									<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
								</button>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</div>

	<!-- Right: Procedure editor -->
	<div class="flex-1 flex flex-col overflow-hidden">
		{#if editingId}
			{@const sop = sopList.find(s => s.id === editingId)}
			<!-- Toolbar -->
			<div class="flex items-center gap-3 border-b border-border bg-card px-4 py-2 flex-shrink-0">
				<span class="text-sm font-semibold text-foreground">{sop?.name || 'Procedure'}</span>
				<span class="text-xs text-muted-foreground">{sop?.description || ''}</span>
				<div class="flex-1"></div>
				<button onclick={() => { showYaml = !showYaml; if (showYaml) yamlContent = stepsToYaml(); }}
					class="rounded-md border border-border px-3 py-1.5 text-xs font-medium {showYaml ? 'bg-primary text-primary-foreground' : 'hover:bg-accent'}">YAML</button>
				<button onclick={save} disabled={saving}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{saving ? 'Saving...' : 'Save'}</button>
			</div>

			<div class="flex-1 overflow-y-auto relative">
				{#if showYaml}
					<div class="absolute inset-0 z-20 flex flex-col bg-background/95 backdrop-blur-sm">
						<div class="flex items-center justify-between px-4 py-2 border-b border-border">
							<span class="text-xs font-medium text-muted-foreground">YAML</span>
							<div class="flex gap-2">
								<button onclick={() => {
									try {
										const parsed = yaml.load(yamlContent) as any;
										editSummary = parsed?.summary || '';
										editSteps = (parsed?.steps || []).map((s: any) => ({ name: s.name, description: s.description || '', tools: s.tools || [], optional: s.optional ?? false, expanded: false }));
										editInputs = (parsed?.inputs || []).map((i: any) => ({ name: i.name, description: i.description || '', required: i.required ?? true }));
										editOutputs = (parsed?.outputs || []).map((o: any) => ({ name: o.name, description: o.description || '' }));
									} catch {}
									showYaml = false;
								}} class="rounded-md bg-primary px-3 py-1 text-xs text-primary-foreground">Apply</button>
								<button onclick={() => (showYaml = false)} class="rounded-md border border-border px-3 py-1 text-xs hover:bg-accent">Close</button>
							</div>
						</div>
						<textarea bind:value={yamlContent} class="flex-1 w-full bg-transparent p-4 font-mono text-xs text-foreground resize-none focus:outline-none" spellcheck="false"></textarea>
					</div>
				{/if}

				<div class="max-w-2xl mx-auto py-4 px-4">
					<!-- Summary -->
					<div class="mb-4">
						<label class="block text-[10px] font-medium text-muted-foreground mb-1">SUMMARY</label>
						<input type="text" bind:value={editSummary}
							class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" placeholder="What does this procedure do?" />
					</div>

					<!-- Inputs/Outputs -->
					<div class="grid grid-cols-2 gap-3 mb-4">
						<div>
							<div class="flex items-center justify-between mb-1">
								<label class="text-[10px] font-medium text-muted-foreground">INPUTS</label>
								<button onclick={() => { editInputs = [...editInputs, { name: '', description: '', required: true }]; }}
									class="text-[10px] text-primary hover:underline">+ Add</button>
							</div>
							{#each editInputs as inp, i}
								<div class="flex items-center gap-1 mb-1">
									<input type="text" bind:value={editInputs[i].name} placeholder="name"
										class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-mono" />
									<button onclick={() => { editInputs = editInputs.filter((_, j) => j !== i); }}
										class="text-muted-foreground/30 hover:text-destructive text-xs">x</button>
								</div>
							{/each}
						</div>
						<div>
							<div class="flex items-center justify-between mb-1">
								<label class="text-[10px] font-medium text-muted-foreground">OUTPUTS</label>
								<button onclick={() => { editOutputs = [...editOutputs, { name: '', description: '' }]; }}
									class="text-[10px] text-primary hover:underline">+ Add</button>
							</div>
							{#each editOutputs as out, i}
								<div class="flex items-center gap-1 mb-1">
									<input type="text" bind:value={editOutputs[i].name} placeholder="name"
										class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-xs font-mono" />
									<button onclick={() => { editOutputs = editOutputs.filter((_, j) => j !== i); }}
										class="text-muted-foreground/30 hover:text-destructive text-xs">x</button>
								</div>
							{/each}
						</div>
					</div>

					<!-- Steps -->
					<div class="flex items-center text-[10px] font-medium text-muted-foreground/50 uppercase tracking-wider pb-1 px-1">
						<span class="w-10">Step</span>
						<span class="flex-1">Instructions</span>
					</div>

					{#each editSteps as step, idx}
						{#if idx > 0}
							<BlockConnector />
						{/if}

						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div class="flex items-start gap-0"
							draggable="true"
							ondragstart={(e) => { e.dataTransfer?.setData('text/plain', String(idx)); dragIdx = idx; }}
							ondragover={(e) => { e.preventDefault(); dragOverIdx = idx; }}
							ondragleave={() => { if (dragOverIdx === idx) dragOverIdx = null; }}
							ondrop={(e) => { e.preventDefault(); dragOverIdx = null; const from = parseInt(e.dataTransfer?.getData('text/plain') || ''); if (!isNaN(from)) moveStep(from, idx); }}
							ondragend={() => { dragIdx = null; dragOverIdx = null; }}
						>
							<div class="w-10 pt-2.5 text-right pr-3 text-xs font-mono text-muted-foreground/40 select-none cursor-grab active:cursor-grabbing">
								{String(idx + 1).padStart(2, '0')}
							</div>
							<div class="flex-1 {dragOverIdx === idx ? 'ring-2 ring-primary/30 rounded-lg' : ''}">
								<div class="group rounded-lg border border-border/60 bg-card border-l-[3px] border-l-blue-500">
									<div class="flex items-center gap-2 px-3 py-2">
										<span class="text-[10px] font-bold tracking-wider text-blue-400">STEP</span>
										<span class="text-sm font-medium text-foreground flex-1 truncate">{step.name}</span>
										{#if step.optional}
											<span class="text-[10px] text-muted-foreground bg-muted rounded px-1.5 py-0.5">optional</span>
										{/if}
										<button onclick={() => { editSteps[idx].expanded = !editSteps[idx].expanded; editSteps = editSteps; }}
											class="text-muted-foreground hover:text-foreground">
											<svg class="h-3.5 w-3.5 transition-transform {step.expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
										</button>
										<button onclick={() => removeStep(idx)}
											class="text-muted-foreground/30 hover:text-destructive opacity-0 group-hover:opacity-100">
											<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" /></svg>
										</button>
									</div>

									{#if step.expanded}
										<div class="border-t border-border/40 px-3 py-3 space-y-3">
											<div>
												<label class="block text-[10px] font-medium text-muted-foreground mb-1">NAME</label>
												<input type="text" bind:value={editSteps[idx].name}
													class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs" />
											</div>
											<div>
												<label class="block text-[10px] font-medium text-muted-foreground mb-1">DESCRIPTION</label>
												<textarea bind:value={editSteps[idx].description} rows="3"
													class="w-full rounded border border-input bg-background px-2 py-1.5 text-xs resize-none"
													placeholder="What should happen in this step?"></textarea>
											</div>
											<div class="flex items-center gap-4">
												<label class="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer">
													<input type="checkbox" bind:checked={editSteps[idx].optional} class="rounded" />
													Optional step
												</label>
											</div>
										</div>
									{/if}
								</div>
							</div>
						</div>
					{/each}

					<!-- Add step -->
					<BlockConnector />
					<div class="flex items-center gap-0">
						<div class="w-10"></div>
						<button onclick={addStep}
							class="rounded-lg border border-dashed border-border hover:border-blue-500/50 hover:bg-blue-950/20 px-4 py-2 text-xs text-muted-foreground hover:text-blue-300 transition-all flex items-center gap-1.5">
							<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 4.5v15m7.5-7.5h-15" /></svg>
							Add Step
						</button>
					</div>
				</div>
			</div>
		{:else}
			<div class="flex-1 flex items-center justify-center text-muted-foreground text-sm">
				Select a procedure or create a new one
			</div>
		{/if}
	</div>

	<!-- Toast -->
	{#if toast}
		<div class="fixed bottom-4 right-4 z-50 rounded-lg border px-4 py-2.5 text-xs font-medium shadow-lg
			{toast.type === 'success' ? 'border-emerald-800/60 bg-emerald-950/80 text-emerald-300' : 'border-red-800/60 bg-red-950/80 text-red-300'}">
			{toast.message}
		</div>
	{/if}
</div>
