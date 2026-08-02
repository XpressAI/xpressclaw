<script lang="ts">
	export type WorkflowInputType = 'string' | 'number' | 'boolean' | 'json';
	export interface WorkflowInputDefinition {
		type: WorkflowInputType;
		description?: string;
		required?: boolean;
		default?: unknown;
	}
	export interface WorkflowScheduleDefinition {
		cron: string;
		inputs: Record<string, unknown>;
	}

	let {
		inputs = {},
		schedule = null,
		onupdate = (_inputs: Record<string, WorkflowInputDefinition>, _schedule: WorkflowScheduleDefinition | null) => {},
		onvalidationchange = (_message: string) => {},
	}: {
		inputs?: Record<string, WorkflowInputDefinition>;
		schedule?: WorkflowScheduleDefinition | null;
		onupdate?: (inputs: Record<string, WorkflowInputDefinition>, schedule: WorkflowScheduleDefinition | null) => void;
		onvalidationchange?: (message: string) => void;
	} = $props();

	let expanded = $state(true);
	let valueErrors = $state<Record<string, string>>({});
	let validationMessage = $derived(Object.values(valueErrors)[0] ?? '');

	$effect(() => onvalidationchange(validationMessage));

	function uniqueInputName(): string {
		let suffix = Object.keys(inputs).length + 1;
		while (inputs[`input_${suffix}`]) suffix += 1;
		return `input_${suffix}`;
	}

	function addInput() {
		const name = uniqueInputName();
		onupdate({ ...inputs, [name]: { type: 'string', required: true } }, schedule);
	}

	function renameInput(oldName: string, rawName: string) {
		const name = rawName.trim().replace(/[^a-zA-Z0-9_]+/g, '_').replace(/^_+|_+$/g, '');
		if (!name || name === oldName || inputs[name]) return;
		const nextInputs: Record<string, WorkflowInputDefinition> = {};
		for (const [key, definition] of Object.entries(inputs)) {
			nextInputs[key === oldName ? name : key] = definition;
		}
		let nextSchedule = schedule;
		if (schedule && Object.hasOwn(schedule.inputs, oldName)) {
			const { [oldName]: value, ...remaining } = schedule.inputs;
			nextSchedule = { ...schedule, inputs: { ...remaining, [name]: value } };
		}
		valueErrors = Object.fromEntries(Object.entries(valueErrors).map(([key, message]) => [
			key.endsWith(`:${oldName}`) ? `${key.slice(0, -(oldName.length))}${name}` : key,
			message,
		]));
		onupdate(nextInputs, nextSchedule);
	}

	function updateInput(name: string, update: Partial<WorkflowInputDefinition>) {
		onupdate({ ...inputs, [name]: { ...inputs[name], ...update } }, schedule);
	}

	function changeInputType(name: string, type: WorkflowInputType) {
		const next = { ...inputs[name], type };
		delete next.default;
		const nextSchedule = schedule ? { ...schedule, inputs: { ...schedule.inputs } } : null;
		if (nextSchedule) delete nextSchedule.inputs[name];
		clearValueErrors(name);
		onupdate({ ...inputs, [name]: next }, nextSchedule);
	}

	function removeInput(name: string) {
		const { [name]: _, ...remainingInputs } = inputs;
		const nextSchedule = schedule ? { ...schedule, inputs: { ...schedule.inputs } } : null;
		if (nextSchedule) delete nextSchedule.inputs[name];
		clearValueErrors(name);
		onupdate(remainingInputs, nextSchedule);
	}

	function setTriggerMode(mode: string) {
		if (mode === 'schedule') {
			onupdate(inputs, schedule ?? { cron: '0 9 * * *', inputs: {} });
		} else {
			valueErrors = Object.fromEntries(Object.entries(valueErrors).filter(([key]) => !key.startsWith('schedule:')));
			onupdate(inputs, null);
		}
	}

	function updateSchedule(update: Partial<WorkflowScheduleDefinition>) {
		if (!schedule) return;
		onupdate(inputs, { ...schedule, ...update });
	}

	function setScheduleInput(name: string, value: unknown, present = true) {
		if (!schedule) return;
		const next = { ...schedule.inputs };
		if (present) next[name] = value;
		else delete next[name];
		updateSchedule({ inputs: next });
	}

	function clearValueErrors(name: string) {
		valueErrors = Object.fromEntries(Object.entries(valueErrors).filter(([key]) => !key.endsWith(`:${name}`)));
	}

	function parseJson(key: string, raw: string, apply: (value: unknown, present?: boolean) => void) {
		if (!raw.trim()) {
			const { [key]: _, ...remaining } = valueErrors;
			valueErrors = remaining;
			apply(undefined, false);
			return;
		}
		try {
			apply(JSON.parse(raw), true);
			const { [key]: _, ...remaining } = valueErrors;
			valueErrors = remaining;
		} catch {
			valueErrors = { ...valueErrors, [key]: 'JSON values must contain valid JSON before the workflow can be saved.' };
		}
	}

	function setJsonDefault(name: string, raw: string) {
		parseJson(`default:${name}`, raw, (value, present) => updateInput(name, {
			default: present !== false ? value : undefined,
		}));
	}

	function setScheduledJson(name: string, raw: string) {
		parseJson(`schedule:${name}`, raw, (value, present) => setScheduleInput(name, value, present !== false));
	}

	function displayValue(value: unknown, type: WorkflowInputType): string {
		if (value === undefined) return '';
		if (type === 'json') {
			try { return JSON.stringify(value, null, 2); } catch { return ''; }
		}
		return String(value);
	}
</script>

<div data-workflow-configuration class="rounded-lg border border-border/60 bg-card border-l-[3px] border-l-violet-500">
	<div class="flex items-center gap-2 px-3 py-2">
		<span class="text-[10px] font-bold tracking-wider text-violet-400">START</span>
		<div class="min-w-0 flex-1">
			<div class="text-sm font-medium text-foreground">Inputs & trigger</div>
			<div class="truncate text-[10px] text-muted-foreground">
				{Object.keys(inputs).length} input{Object.keys(inputs).length === 1 ? '' : 's'} · {schedule ? schedule.cron : 'manual runs'}
			</div>
		</div>
		<button type="button" onclick={() => (expanded = !expanded)} aria-label={expanded ? 'Collapse workflow configuration' : 'Expand workflow configuration'} class="text-muted-foreground hover:text-foreground">
			<svg class="h-3.5 w-3.5 transition-transform {expanded ? 'rotate-180' : ''}" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19.5 8.25l-7.5 7.5-7.5-7.5" /></svg>
		</button>
	</div>

	{#if expanded}
		<div class="space-y-4 border-t border-border/40 px-3 py-3">
			<div class="grid gap-3 sm:grid-cols-[12rem_1fr]">
				<label class="text-[10px] font-medium text-muted-foreground">AUTOMATIC TRIGGER
					<select value={schedule ? 'schedule' : 'manual'} onchange={(event) => setTriggerMode(event.currentTarget.value)} class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 text-xs text-foreground">
						<option value="manual">Manual only</option>
						<option value="schedule">Cron schedule</option>
					</select>
				</label>
				{#if schedule}
					<label class="text-[10px] font-medium text-muted-foreground">CRON (SERVER-LOCAL TIME)
						<input aria-label="Workflow cron schedule" type="text" value={schedule.cron} oninput={(event) => updateSchedule({ cron: event.currentTarget.value })} placeholder="0 9 * * 1" class="mt-1 w-full rounded border border-input bg-background px-2 py-1.5 font-mono text-xs text-foreground" />
					</label>
				{:else}
					<p class="self-end pb-1 text-[10px] leading-relaxed text-muted-foreground">The Run button is always available. Add a schedule when this workflow should also start on its own.</p>
				{/if}
			</div>

			<div class="space-y-2">
				<div class="flex items-center justify-between gap-3">
					<div>
						<div class="text-[10px] font-medium text-muted-foreground">RUN INPUTS</div>
						<p class="mt-0.5 text-[10px] text-muted-foreground">Inputs become template variables such as <code>@goal</code>. Defaults are optional for manual runs.</p>
					</div>
					<button type="button" onclick={addInput} class="shrink-0 rounded border border-border px-2 py-1 text-[10px] font-medium hover:bg-accent">+ Input</button>
				</div>

				{#if Object.keys(inputs).length === 0}
					<div class="rounded border border-dashed border-border px-3 py-2 text-[10px] text-muted-foreground">This workflow runs without input. Add an input to pass a goal, repository URL, options, or structured JSON into each run.</div>
				{:else}
					{#each Object.entries(inputs) as [name, definition]}
						<div class="space-y-2 rounded border border-border/60 bg-background/40 p-2">
							<div class="grid gap-2 sm:grid-cols-[1fr_8rem_auto_auto]">
								<label class="text-[9px] font-medium text-muted-foreground">NAME
									<input aria-label="Workflow input name" type="text" value={name} onchange={(event) => renameInput(name, event.currentTarget.value)} class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs font-mono" />
								</label>
								<label class="text-[9px] font-medium text-muted-foreground">TYPE
									<select aria-label="Workflow input type" value={definition.type} onchange={(event) => changeInputType(name, event.currentTarget.value as WorkflowInputType)} class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs">
										<option value="string">Text</option><option value="number">Number</option><option value="boolean">Yes / no</option><option value="json">JSON</option>
									</select>
								</label>
								<label class="mt-4 flex items-center gap-1.5 text-[10px] text-muted-foreground"><input type="checkbox" checked={definition.required ?? false} onchange={(event) => updateInput(name, { required: event.currentTarget.checked })} /> Required</label>
								<button type="button" onclick={() => removeInput(name)} aria-label="Remove workflow input" class="mt-4 text-xs text-muted-foreground hover:text-destructive">Remove</button>
							</div>
							<input aria-label="Workflow input description" type="text" value={definition.description ?? ''} oninput={(event) => updateInput(name, { description: event.currentTarget.value || undefined })} placeholder="What should the caller provide?" class="w-full rounded border border-input bg-background px-2 py-1 text-xs" />

							<div class="grid gap-2 {schedule ? 'sm:grid-cols-2' : ''}">
								<label class="text-[9px] font-medium text-muted-foreground">DEFAULT (OPTIONAL)
									{#if definition.type === 'boolean'}
										<select aria-label="Workflow input default" value={definition.default === undefined ? '' : String(definition.default)} onchange={(event) => updateInput(name, { default: event.currentTarget.value === '' ? undefined : event.currentTarget.value === 'true' })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs"><option value="">No default</option><option value="true">Yes</option><option value="false">No</option></select>
									{:else if definition.type === 'json'}
										<textarea aria-label="Workflow input default" rows="2" value={displayValue(definition.default, definition.type)} onchange={(event) => setJsonDefault(name, event.currentTarget.value)} placeholder={'{"key":"value"}'} class="mt-1 w-full resize-y rounded border border-input bg-background px-2 py-1 font-mono text-xs"></textarea>
									{:else}
										<input aria-label="Workflow input default" type={definition.type === 'number' ? 'number' : 'text'} value={displayValue(definition.default, definition.type)} onchange={(event) => updateInput(name, { default: event.currentTarget.value === '' ? undefined : definition.type === 'number' ? Number(event.currentTarget.value) : event.currentTarget.value })} class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs" />
									{/if}
								</label>

								{#if schedule}
									<label class="text-[9px] font-medium text-muted-foreground">SCHEDULED VALUE {definition.required && definition.default === undefined ? '(REQUIRED)' : '(OPTIONAL OVERRIDE)'}
										{#if definition.type === 'boolean'}
											<select aria-label="Scheduled workflow input" value={Object.hasOwn(schedule.inputs, name) ? String(schedule.inputs[name]) : ''} onchange={(event) => setScheduleInput(name, event.currentTarget.value === 'true', event.currentTarget.value !== '')} class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs"><option value="">Use default</option><option value="true">Yes</option><option value="false">No</option></select>
										{:else if definition.type === 'json'}
											<textarea aria-label="Scheduled workflow input" rows="2" value={displayValue(schedule.inputs[name], definition.type)} onchange={(event) => setScheduledJson(name, event.currentTarget.value)} placeholder="Use default" class="mt-1 w-full resize-y rounded border border-input bg-background px-2 py-1 font-mono text-xs"></textarea>
										{:else}
											<input aria-label="Scheduled workflow input" type={definition.type === 'number' ? 'number' : 'text'} value={displayValue(schedule.inputs[name], definition.type)} onchange={(event) => setScheduleInput(name, definition.type === 'number' ? Number(event.currentTarget.value) : event.currentTarget.value, event.currentTarget.value !== '')} placeholder="Use default" class="mt-1 w-full rounded border border-input bg-background px-2 py-1 text-xs" />
										{/if}
									</label>
								{/if}
							</div>
						</div>
					{/each}
				{/if}
			</div>
			{#if validationMessage}<p class="text-[10px] text-destructive">{validationMessage}</p>{/if}
		</div>
	{/if}
</div>
