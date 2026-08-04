<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { tasks, agents, sessions, workspaces } from '$lib/api';
	import type { AcpCommand, AcpConfigOption, AcpModeState, Task, TaskMessage, Agent, WorkAttempt, SessionEvent, ImageAttachmentUpload, GitChange, WorkspaceGitStatus } from '$lib/api';
	import { timeAgo } from '$lib/utils';
	import { renderContent } from '$lib/formatMessage';
	import ActivityEventRow from '$lib/components/ActivityEventRow.svelte';
	import ImageAttachmentPreviews from '$lib/components/ImageAttachmentPreviews.svelte';
	import { clearComposerDraft, loadComposerDraft, saveComposerDraft } from '$lib/composerDrafts';
	import { appendImageFiles, imageDataUrl, IMAGE_FILE_ACCEPT, MAX_IMAGE_ATTACHMENTS, pastedImageFiles, shouldHandleImagePaste } from '$lib/imageAttachments';

	let { taskId, compact = false }: { taskId: string; compact?: boolean } = $props();
	const messageDraftScope = () => `task.${taskId}`;

	interface ElicitationOption {
		const?: unknown;
		title?: string;
		description?: string;
	}

	interface ElicitationProperty {
		type?: string;
		title?: string;
		description?: string;
		default?: unknown;
		oneOf?: ElicitationOption[];
		enum?: unknown[];
		items?: { anyOf?: ElicitationOption[]; enum?: unknown[] };
	}

	interface ElicitationField {
		key: string;
		question: string;
		property: ElicitationProperty;
		customKey: string | null;
		customProperty: ElicitationProperty | null;
	}

	interface PendingElicitation {
		id: string;
		eventId: number;
		attemptId: string | null;
		message: string;
		fields: ElicitationField[];
	}

	interface ContextUsage {
		used: number;
		size: number;
		percent: number;
	}

	type TranscriptItem =
		| {
			kind: 'message';
			key: string;
			timestamp: string;
			role: string;
			content: string;
			attachments: { id?: string; name: string; src: string }[];
			sequence: number;
		}
		| {
			kind: 'activity';
			key: string;
			timestamp: string;
			event: SessionEvent;
			sequence: number;
		};

	let task = $state<Task | null>(null);
	let messages = $state<TaskMessage[]>([]);
	let attempts = $state<WorkAttempt[]>([]);
	let activityEvents = $state<SessionEvent[]>([]);
	let subtaskList = $state<Task[]>([]);
	let agentList = $state<Agent[]>([]);
	let allTasks = $state<Task[]>([]);
	let workspaceGit = $state<WorkspaceGitStatus | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(true);
	let editing = $state(false);
	let editTitle = $state('');
	let editDesc = $state('');
	let editAgentId = $state('');
	let editPriority = $state(0);
	let editDeps = $state<string[]>([]);
	let messageInput = $state('');
	let messageDraftReady = $state(false);
	let messageSending = $state(false);
	let interrupting = $state(false);
	let messageAttachments = $state<ImageAttachmentUpload[]>([]);
	let messageAttachmentError = $state('');
	let messageImageInput = $state<HTMLInputElement>();
	let messageImagePreviews = $derived(messageAttachments.map((attachment) => ({
		name: attachment.name,
		src: imageDataUrl(attachment),
	})));
	let sessionEvents = $state<SessionEvent[]>([]);
	let configOptions = $state<AcpConfigOption[]>([]);
	let selectedConfig = $state<Record<string, string | boolean>>({});
	let availableCommands = $state<AcpCommand[]>([]);
	let modelMenuOpen = $state(false);
	let messageInputFocused = $state(false);
	let slashMenuDismissed = $state(false);
	let selectedCommandIndex = $state(0);
	let elicitationAnswers = $state<Record<string, Record<string, unknown>>>({});
	let elicitationPage = $state<Record<string, number>>({});
	let elicitationSending = $state(false);
	let locallyResolvedElicitations = $state<Record<string, boolean>>({});
	let controlsInitializedFor = '';
	let composing = $state(false);
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let messagesEl = $state<HTMLDivElement>();
	let composerEl = $state<HTMLDivElement>();
	let prevMessageCount = 0;
	let lastActivityEventId = 0;
	let hasEarlierActivity = $state(false);
	let loadingEarlierActivity = $state(false);
	let initialLoad = true;
	let followLatest = $state(true);
	let showJumpToLatest = $state(false);
	let lastTranscriptScrollTop = 0;
	let lastWorkspaceGitRefresh = 0;

	$effect(() => {
		if (messageDraftReady) saveComposerDraft(messageDraftScope(), messageInput);
	});

	let availableDeps = $derived(
		allTasks.filter(t => t.id !== task?.id && t.status !== 'completed' && t.status !== 'cancelled')
	);
	let collapsedActivityEvents = $derived(collapseToolActivity(activityEvents));
	let primaryActivityEvents = $derived(
		collapsedActivityEvents.filter(event => {
			const mirrorsTaskReply = event.payload?.item_type === 'agent_message' && messages.some(message =>
				message.role === 'assistant' && (
					message.content === event.summary ||
					(event.summary.length >= 200 && message.content.startsWith(event.summary.slice(0, 180)))
				)
			);
			return !['artifact_created', 'attempt_completed', 'elicitation_pending', 'elicitation_resolved', 'usage'].includes(event.event_type) &&
				(event.event_type !== 'runner_progress' || event.payload?.item_type === 'agent_message') &&
				!mirrorsTaskReply;
		})
	);
	let technicalActivityEvents = $derived(
		collapsedActivityEvents.filter(event =>
			event.event_type === 'runner_progress' && event.payload?.item_type !== 'agent_message'
		)
	);
	let activityTimelineEvents = $derived(
		[...primaryActivityEvents, ...technicalActivityEvents].sort((left, right) => left.id - right.id)
	);
	let transcriptItems = $derived((() => {
		const items: TranscriptItem[] = [];
		const taskPrompt = task?.description?.trim();
		const promptAlreadyPersisted = taskPrompt && messages.some(message =>
			message.role === 'user' && message.content.trim() === taskPrompt
		);

		if (task && taskPrompt && !promptAlreadyPersisted) {
			items.push({
				kind: 'message',
				key: `task-prompt:${task.id}`,
				timestamp: task.created_at,
				role: 'user',
				content: taskPrompt,
				attachments: [],
				sequence: -1,
			});
		}
		items.push(...messages.map((message): TranscriptItem => ({
			kind: 'message',
			key: `message:${message.id}`,
			timestamp: message.timestamp,
			role: message.role,
			content: message.content,
			attachments: (message.attachments ?? []).map((attachment) => ({
				id: attachment.id,
				name: attachment.name,
				src: `/api/tasks/${encodeURIComponent(taskId)}/messages/${message.id}/attachments/${encodeURIComponent(attachment.id)}`,
			})),
			sequence: message.id,
		})));
		items.push(...activityTimelineEvents.map((event): TranscriptItem => ({
			kind: 'activity',
			key: `activity:${event.id}`,
			timestamp: event.created_at,
			event,
			sequence: event.id,
		})));

		return items.sort(compareTranscriptItems);
	})());
	let pendingElicitations = $derived((() => {
		const resolved = new Set(activityEvents
			.filter(event => event.event_type === 'elicitation_resolved')
			.map(event => String(event.payload?.elicitationId ?? ''))
			.filter(Boolean));
		const liveAttempts = new Set(attempts
			.filter(attempt => !['completed', 'failed', 'cancelled', 'interrupted'].includes(attempt.status))
			.map(attempt => attempt.id));
		return activityEvents
			.filter(event => event.event_type === 'elicitation_pending')
			.filter(event => {
				const id = String(event.payload?.elicitationId ?? '');
				return id && !resolved.has(id) && !locallyResolvedElicitations[id]
					&& (!event.attempt_id || liveAttempts.has(event.attempt_id));
			})
			.map(parsePendingElicitation)
			.filter((item): item is PendingElicitation => item !== null);
	})());
	let pendingElicitation = $derived(pendingElicitations.at(-1) ?? null);
	let runningAttempt = $derived(
		attempts.find(attempt => ['preparing', 'running', 'waiting_for_input', 'review'].includes(attempt.status)) ?? null
	);
	let queuedAttempt = $derived(attempts.find(attempt => attempt.status === 'queued') ?? null);
	let activeAttempt = $derived(runningAttempt ?? queuedAttempt);
	let usageAttempt = $derived(activeAttempt ?? attempts[0] ?? null);
	let contextUsage = $derived(contextUsageFor(usageAttempt, activityEvents));
	let latestAttemptResult = $derived(attempts.find(attempt => attempt.result)?.result ?? null);
	let latestResult = $derived(
		latestAttemptResult && !messages.some(message =>
			message.role === 'assistant' && message.content === latestAttemptResult
		)
			? latestAttemptResult
			: null
	);
	let latestError = $derived(attempts.find(attempt => attempt.error_message)?.error_message ?? null);
	let messagePlaceholder = $derived(
		!task?.agent_id
			? 'Assign an agent to chat about this task'
			: pendingElicitation
				? 'Answer the agent’s question above...'
			: task.status === 'waiting_for_input'
				? 'Reply to the worker...'
				: ['completed', 'blocked', 'cancelled'].includes(task.status)
					? 'Ask a follow-up or request a correction...'
					: 'Send additional context...'
	);
	let slashCommandQuery = $derived((() => {
		if (!messageInput.startsWith('/')) return null;
		const commandText = messageInput.slice(1);
		if (/\s/.test(commandText)) return null;
		return commandText.toLowerCase();
	})());
	let filteredCommands = $derived(
		slashCommandQuery === null
			? []
			: availableCommands.filter((command) => command.name.toLowerCase().includes(slashCommandQuery!))
	);
	let slashMenuOpen = $derived(
		messageInputFocused && !slashMenuDismissed && slashCommandQuery !== null && availableCommands.length > 0
	);
	let modelOption = $derived(configOptions.find(isModelOption) ?? null);
	let reasoningOption = $derived(configOptions.find(isReasoningOption) ?? null);
	let modelConfigOptions = $derived(configOptions.filter(isAdditionalModelOption));
	let hasModelMenu = $derived(Boolean(modelOption || reasoningOption || modelConfigOptions.length > 0));
	let otherConfigOptions = $derived(
		configOptions.filter((option) => !isModelOption(option) && !isReasoningOption(option) && !isAdditionalModelOption(option))
	);

	function transcriptTimestamp(value: string): number {
		const normalized = value.includes('T') ? value : `${value.replace(' ', 'T')}Z`;
		const parsed = Date.parse(normalized);
		return Number.isNaN(parsed) ? 0 : parsed;
	}

	function transcriptRank(item: TranscriptItem): number {
		if (item.kind === 'activity') return 1;
		return item.role === 'user' ? 0 : 2;
	}

	function compareTranscriptItems(left: TranscriptItem, right: TranscriptItem): number {
		return transcriptTimestamp(left.timestamp) - transcriptTimestamp(right.timestamp)
			|| transcriptRank(left) - transcriptRank(right)
			|| left.sequence - right.sequence;
	}

	function toolCallKey(event: SessionEvent): string | null {
		const toolCallId = event.payload?.toolCallId;
		if (typeof toolCallId !== 'string' || !toolCallId) return null;
		return `${event.attempt_id ?? ''}:${toolCallId}`;
	}

	function mergeToolContent(original: unknown, update: unknown): unknown {
		if (!Array.isArray(update)) return original;
		if (!Array.isArray(original)) return update;
		const updatedDiffPaths = new Set(update.flatMap(item =>
			typeof item === 'object' && item !== null && 'type' in item && item.type === 'diff' && 'path' in item && typeof item.path === 'string'
				? [item.path]
				: []
		));
		const retainedDiffs = original.filter(item =>
			typeof item === 'object' && item !== null && 'type' in item && item.type === 'diff'
				&& (!('path' in item) || typeof item.path !== 'string' || !updatedDiffPaths.has(item.path))
		);
		return [...retainedDiffs, ...update];
	}

	/** Collapse the append-only ACP start/update pair into one visible tool row. */
	function collapseToolActivity(events: SessionEvent[]): SessionEvent[] {
		const collapsed: SessionEvent[] = [];
		const toolIndexes = new Map<string, number>();
		for (const event of events) {
			const key = toolCallKey(event);
			if (event.event_type === 'tool_call') {
				collapsed.push({ ...event, payload: { ...event.payload } });
				if (key) toolIndexes.set(key, collapsed.length - 1);
				continue;
			}
			if (event.event_type !== 'tool_call_update') {
				collapsed.push(event);
				continue;
			}

			const existingIndex = key ? toolIndexes.get(key) : undefined;
			if (existingIndex !== undefined) {
				const existing = collapsed[existingIndex];
				const content = mergeToolContent(existing.payload.content, event.payload.content);
				const summary = event.summary.replace(/^Completed\s+/, '') || existing.summary;
				collapsed[existingIndex] = {
					...existing,
					summary,
					payload: {
						...existing.payload,
						...event.payload,
						sessionUpdate: 'tool_call',
						updatedAt: event.created_at,
						...(content === undefined ? {} : { content }),
					},
				};
				continue;
			}

			// A paged response can contain the update while its start event is on
			// an earlier page. Keep one useful row until that page is loaded.
			const title = typeof event.payload?.title === 'string'
				? event.payload.title
				: event.summary.replace(/^Completed\s+/, '');
			collapsed.push({
				...event,
				event_type: 'tool_call',
				summary: title,
				payload: { ...event.payload, title, sessionUpdate: 'tool_call', updatedAt: event.created_at },
			});
			if (key) toolIndexes.set(key, collapsed.length - 1);
		}
		return collapsed;
	}

	function contextUsageFor(attempt: WorkAttempt | null, events: SessionEvent[]): ContextUsage | null {
		if (!attempt) return null;
		let used = attempt.context_used;
		let size = attempt.context_size;
		if (typeof used !== 'number' || !Number.isFinite(used) || typeof size !== 'number' || !Number.isFinite(size)) {
			const legacyUsage = [...events].reverse().find(event =>
				event.event_type === 'usage' && event.attempt_id === attempt.id
			);
			used = typeof legacyUsage?.payload.used === 'number' ? legacyUsage.payload.used : null;
			size = typeof legacyUsage?.payload.size === 'number' ? legacyUsage.payload.size : null;
		}
		if (used === null || size === null || used < 0 || size <= 0) return null;
		return {
			used,
			size,
			percent: Math.min(100, Math.max(0, (used / size) * 100)),
		};
	}

	function formatTokens(value: number): string {
		return new Intl.NumberFormat('en-US').format(value);
	}

	function selectChoices(option: AcpConfigOption): { value: string; name: string; description?: string | null }[] {
		if (!Array.isArray(option.options)) return [];
		return option.options.flatMap((entry) => 'options' in entry ? entry.options : [entry]);
	}

	function isModelOption(option: AcpConfigOption): boolean {
		return option.id === 'model' || option.category === 'model';
	}

	function isReasoningOption(option: AcpConfigOption): boolean {
		return option.category === 'thought_level'
			|| option.id === 'reasoning_effort'
			|| option.id === 'thought_level';
	}

	function isAdditionalModelOption(option: AcpConfigOption): boolean {
		return option.category === 'model_config' && !isModelOption(option) && !isReasoningOption(option);
	}

	function selectedValue(option: AcpConfigOption): string | boolean {
		return selectedConfig[option.id] ?? option.currentValue;
	}

	function selectedChoiceName(option: AcpConfigOption): string {
		const value = String(selectedValue(option));
		return selectChoices(option).find((choice) => choice.value === value)?.name ?? value;
	}

	function setConfigOption(option: AcpConfigOption, value: string | boolean) {
		selectedConfig = { ...selectedConfig, [option.id]: value };
	}

	function isRecord(value: unknown): value is Record<string, unknown> {
		return typeof value === 'object' && value !== null && !Array.isArray(value);
	}

	function parsePendingElicitation(event: SessionEvent): PendingElicitation | null {
		const id = typeof event.payload.elicitationId === 'string' ? event.payload.elicitationId : null;
		const message = typeof event.payload.message === 'string' ? event.payload.message : 'The agent needs your input.';
		const schema = isRecord(event.payload.requestedSchema) ? event.payload.requestedSchema : null;
		const properties = schema && isRecord(schema.properties) ? schema.properties : null;
		if (!id || event.payload.mode !== 'form' || !properties) return null;

		const entries = Object.entries(properties)
			.filter((entry): entry is [string, Record<string, unknown>] => isRecord(entry[1]));
		const propertyMap = Object.fromEntries(entries) as Record<string, ElicitationProperty>;
		const mainEntries = entries.filter(([key]) => {
			if (!key.endsWith('_custom')) return true;
			return !propertyMap[key.slice(0, -'_custom'.length)];
		});
		const fields = mainEntries.map(([key, property], index): ElicitationField => {
			const typedProperty = property as ElicitationProperty;
			const customKey = propertyMap[`${key}_custom`] ? `${key}_custom` : null;
			const question = typeof typedProperty.description === 'string' && typedProperty.description.trim()
				? typedProperty.description
				: mainEntries.length === 1 ? message : typedProperty.title || `Question ${index + 1}`;
			return {
				key,
				question,
				property: typedProperty,
				customKey,
				customProperty: customKey ? propertyMap[customKey] : null,
			};
		});
		if (fields.length === 0) return null;
		return { id, eventId: event.id, attemptId: event.attempt_id, message, fields };
	}

	function optionsFor(field: ElicitationField): { value: unknown; title: string; description?: string }[] {
		const titled = field.property.type === 'array'
			? field.property.items?.anyOf
			: field.property.oneOf;
		if (Array.isArray(titled)) {
			return titled.map(option => ({
				value: option.const,
				title: option.title || String(option.const ?? ''),
				description: option.description,
			}));
		}
		const values = field.property.type === 'array'
			? field.property.items?.enum
			: field.property.enum;
		return Array.isArray(values)
			? values.map(value => ({ value, title: String(value) }))
			: [];
	}

	function answersFor(elicitationId: string): Record<string, unknown> {
		return elicitationAnswers[elicitationId] ?? {};
	}

	function setElicitationAnswer(elicitationId: string, key: string, value: unknown) {
		elicitationAnswers = {
			...elicitationAnswers,
			[elicitationId]: { ...answersFor(elicitationId), [key]: value },
		};
	}

	function optionSelected(elicitationId: string, field: ElicitationField, value: unknown): boolean {
		const selected = answersFor(elicitationId)[field.key];
		return field.property.type === 'array'
			? Array.isArray(selected) && selected.some(item => Object.is(item, value))
			: Object.is(selected, value);
	}

	function toggleElicitationOption(elicitationId: string, field: ElicitationField, value: unknown) {
		if (field.property.type !== 'array') {
			setElicitationAnswer(elicitationId, field.key, value);
			return;
		}
		const current = answersFor(elicitationId)[field.key];
		const selected = Array.isArray(current) ? current : [];
		setElicitationAnswer(elicitationId, field.key,
			selected.some(item => Object.is(item, value))
				? selected.filter(item => !Object.is(item, value))
				: [...selected, value]);
	}

	function displayElicitationAnswer(elicitation: PendingElicitation, field: ElicitationField): string {
		const answers = answersFor(elicitation.id);
		const custom = field.customKey ? answers[field.customKey] : undefined;
		if (typeof custom === 'string' && custom.trim()) return custom.trim();
		const value = answers[field.key];
		if (Array.isArray(value)) return value.map(String).join(', ');
		if (value === undefined || value === null || value === '') return 'Skipped';
		return String(value);
	}

	function elicitationResponseMessage(elicitation: PendingElicitation): string {
		return elicitation.fields
			.map(field => `${field.question}\n${displayElicitationAnswer(elicitation, field)}`)
			.join('\n\n');
	}

	async function respondToElicitation(elicitation: PendingElicitation, action: 'accept' | 'decline' | 'cancel') {
		if (!task || elicitationSending) return;
		elicitationSending = true;
		try {
			await tasks.respondToElicitation(task.id, elicitation.id, {
				action,
				...(action === 'accept' ? {
					content: answersFor(elicitation.id),
					message: elicitationResponseMessage(elicitation),
				} : {}),
			});
			locallyResolvedElicitations = { ...locallyResolvedElicitations, [elicitation.id]: true };
			await poll();
		} catch (e) {
			alert(String(e));
		} finally {
			elicitationSending = false;
		}
	}

	function validConfigOptions(value: unknown): AcpConfigOption[] {
		if (!Array.isArray(value)) return [];
		return value.filter((option): option is AcpConfigOption => {
			if (typeof option !== 'object' || option === null) return false;
			const item = option as Record<string, unknown>;
			return typeof item.id === 'string' && typeof item.name === 'string'
				&& (item.type === 'select' || item.type === 'boolean');
		});
	}

	function legacyModeOption(value: unknown): AcpConfigOption | null {
		if (typeof value !== 'object' || value === null) return null;
		const modes = value as unknown as AcpModeState;
		if (typeof modes.currentModeId !== 'string' || !Array.isArray(modes.availableModes)) return null;
		return {
			id: 'mode', name: 'Mode', category: 'mode', type: 'select', currentValue: modes.currentModeId,
			options: modes.availableModes.map((mode) => ({ value: mode.id, name: mode.name, description: mode.description }))
		};
	}

	function refreshHarnessControls(events: SessionEvent[], agentId: string) {
		const advertised = [...events].reverse().find((event) => event.event_type === 'session_config_options');
		const options = validConfigOptions(advertised?.payload.config_options);
		if (!options.some((option) => option.category === 'mode' || option.id === 'mode')) {
			const mode = legacyModeOption(advertised?.payload.modes);
			if (mode) options.unshift(mode);
		}
		const latestMode = [...events].reverse().find((event) => event.event_type === 'session_mode');
		const latestModeId = typeof latestMode?.payload.modeId === 'string'
			? latestMode.payload.modeId
			: typeof latestMode?.payload.mode_id === 'string' ? latestMode.payload.mode_id : null;
		if (latestModeId) {
			const mode = options.find((option) => option.category === 'mode' || option.id === 'mode');
			if (mode) mode.currentValue = latestModeId;
		}
		configOptions = options;

		const commandsEvent = [...events].reverse().find((event) => event.event_type === 'available_commands');
		const commands = commandsEvent?.payload.available_commands;
		availableCommands = Array.isArray(commands)
			? commands.filter((command): command is AcpCommand => typeof command === 'object' && command !== null && typeof (command as Record<string, unknown>).name === 'string' && typeof (command as Record<string, unknown>).description === 'string')
			: [];

		if (controlsInitializedFor !== agentId) {
			const configured = agentList.find((agent) => agent.id === agentId)?.config?.runner?.session_config ?? {};
			selectedConfig = { ...Object.fromEntries(options.map((option) => [option.id, option.currentValue])), ...configured };
			controlsInitializedFor = agentId;
		}
	}

	function chooseCommand(command: AcpCommand) {
		messageInput = `${command.name.startsWith('/') ? command.name : `/${command.name}`} `;
		slashMenuDismissed = false;
		selectedCommandIndex = 0;
		setTimeout(() => composerEl?.querySelector<HTMLTextAreaElement>('textarea')?.focus(), 0);
	}

	function handleComposerPointerDown(event: PointerEvent) {
		if (modelMenuOpen && composerEl && !composerEl.contains(event.target as Node)) {
			modelMenuOpen = false;
		}
	}

	onMount(async () => {
		document.addEventListener('pointerdown', handleComposerPointerDown);
		messageInput = loadComposerDraft(messageDraftScope());
		messageDraftReady = true;
		await load();
		loading = false;
		// Auto-poll while task is in progress
		pollTimer = setInterval(async () => {
			if (task && (task.status === 'in_progress' || task.status === 'pending' || task.status === 'waiting_for_input')) {
				await poll();
			}
		}, 3000);
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		document.removeEventListener('pointerdown', handleComposerPointerDown);
	});

	async function load() {
		try {
			const id = taskId;
			const [loadedTask, loadedAgents, loadedMessages, activity] = await Promise.all([
				tasks.get(id),
				agents.list().catch(() => []),
				tasks.messages(id),
				tasks.activity(id),
			]);
			task = loadedTask;
			agentList = loadedAgents;
			messages = loadedMessages;
			attempts = activity.attempts;
			activityEvents = activity.events;
			hasEarlierActivity = activity.has_more_before;
			prevMessageCount = messages.length;
			lastActivityEventId = activityEvents.at(-1)?.id ?? 0;
			if (loadedTask.agent_id) {
				const [events, git] = await Promise.all([
					sessions.events(loadedTask.agent_id).catch(() => []),
					workspaces.gitStatus(loadedTask.agent_id).catch(() => null),
				]);
				sessionEvents = events;
				workspaceGit = git;
				lastWorkspaceGitRefresh = Date.now();
				refreshHarnessControls(sessionEvents, loadedTask.agent_id);
			} else {
				sessionEvents = []; configOptions = []; availableCommands = []; workspaceGit = null;
			}
			try {
				const sub = await tasks.subtasks(id);
				subtaskList = sub.tasks;
			} catch { subtaskList = []; }
			try {
				const all = await tasks.list();
				allTasks = all.tasks;
			} catch { allTasks = []; }
			if (initialLoad) {
				initialLoad = false;
				scrollToBottom(true);
			}
		} catch (e) {
			error = String(e);
		}
	}

	/** Poll semantic task activity without exposing the runner's terminal. */
	async function poll() {
		try {
			const id = taskId;
			const [newTask, newMessages, newActivity] = await Promise.all([
				tasks.get(id),
				tasks.messages(id),
				tasks.activity(id, { after: lastActivityEventId || undefined }),
			]);
			// Update task status/details in-place
			task = newTask;
			attempts = newActivity.attempts;
			let shouldScroll = false;
			// Only update messages and scroll if count changed
			if (newMessages.length !== prevMessageCount) {
				messages = newMessages;
				prevMessageCount = newMessages.length;
				shouldScroll = true;
			}
			if (newActivity.events.length > 0) {
				const known = new Set(activityEvents.map(event => event.id));
				activityEvents = [
					...activityEvents,
					...newActivity.events.filter(event => !known.has(event.id)),
				];
				lastActivityEventId = activityEvents.at(-1)?.id ?? lastActivityEventId;
				shouldScroll = true;
			}
			if (newTask.agent_id) {
				const latestSessionEvents = await sessions.events(newTask.agent_id).catch(() => sessionEvents);
				if (latestSessionEvents.length !== sessionEvents.length || latestSessionEvents.at(-1)?.id !== sessionEvents.at(-1)?.id) {
					sessionEvents = latestSessionEvents;
					refreshHarnessControls(sessionEvents, newTask.agent_id);
				}
				if (Date.now() - lastWorkspaceGitRefresh >= 10_000) {
					workspaceGit = await workspaces.gitStatus(newTask.agent_id).catch(() => workspaceGit);
					lastWorkspaceGitRefresh = Date.now();
				}
			}
			if (shouldScroll) scrollToBottom();
			try {
				const sub = await tasks.subtasks(id);
				subtaskList = sub.tasks;
			} catch { subtaskList = []; }
		} catch { /* ignore poll errors */ }
	}

	async function loadEarlierActivity() {
		const oldestId = activityEvents.at(0)?.id;
		if (!oldestId || loadingEarlierActivity || !hasEarlierActivity) return;
		loadingEarlierActivity = true;
		const previousHeight = messagesEl?.scrollHeight ?? 0;
		const previousTop = messagesEl?.scrollTop ?? 0;
		try {
			const older = await tasks.activity(taskId, { before: oldestId });
			const known = new Set(activityEvents.map((event) => event.id));
			activityEvents = [
				...older.events.filter((event) => !known.has(event.id)),
				...activityEvents,
			];
			hasEarlierActivity = older.has_more_before;
			requestAnimationFrame(() => requestAnimationFrame(() => {
				if (!messagesEl) return;
				messagesEl.scrollTop = previousTop + (messagesEl.scrollHeight - previousHeight);
				lastTranscriptScrollTop = messagesEl.scrollTop;
			}));
		} catch (e) {
			error = String(e);
		} finally {
			loadingEarlierActivity = false;
		}
	}

	function handleTranscriptScroll() {
		if (!messagesEl) return;
		const currentTop = messagesEl.scrollTop;
		const nearBottom = messagesEl.scrollHeight - currentTop - messagesEl.clientHeight <= 24;
		if (currentTop < lastTranscriptScrollTop - 1) {
			followLatest = false;
		} else if (nearBottom) {
			followLatest = true;
		}
		lastTranscriptScrollTop = currentTop;
		showJumpToLatest = !followLatest;
	}

	function scrollToBottom(force = false) {
		if (!force && !followLatest) {
			showJumpToLatest = true;
			return;
		}
		followLatest = true;
		showJumpToLatest = false;
		setTimeout(() => {
			if (!messagesEl) return;
			messagesEl.scrollTop = messagesEl.scrollHeight;
			lastTranscriptScrollTop = messagesEl.scrollTop;
		}, 50);
	}

	function jumpToLatest() {
		scrollToBottom(true);
	}

	async function sendTaskMessage(immediate = false) {
		if ((!messageInput.trim() && messageAttachments.length === 0) || !task || pendingElicitation) return;
		const content = messageInput.trim();
		const attachments = messageAttachments;
		messageAttachmentError = '';
		modelMenuOpen = false;
		slashMenuDismissed = false;
		messageSending = true;
		followLatest = true;
		showJumpToLatest = false;
		try {
			await tasks.addMessage(task.id, 'user', content, {
				configOptions: selectedConfig,
				attachments,
				delivery: immediate ? 'immediate' : 'after_tool',
			});
			messageInput = '';
			messageAttachments = [];
			clearComposerDraft(messageDraftScope());
			await poll();
			scrollToBottom(true);
		} catch (e) {
			messageAttachmentError = e instanceof Error ? e.message : String(e);
		} finally {
			messageSending = false;
		}
	}

	async function interruptAgent() {
		if (!task?.agent_id || !runningAttempt || interrupting || messageSending) return;
		if (!pendingElicitation && (messageInput.trim() || messageAttachments.length > 0)) {
			await sendTaskMessage(true);
			return;
		}
		interrupting = true;
		try {
			await sessions.interruptAttempt(task.agent_id, runningAttempt.id);
			await poll();
		} catch (e) {
			messageAttachmentError = e instanceof Error ? e.message : String(e);
		} finally {
			interrupting = false;
		}
	}

	async function addMessageImages(files: File[]) {
		try {
			messageAttachments = await appendImageFiles(messageAttachments, files);
			messageAttachmentError = '';
		} catch (e) {
			messageAttachmentError = e instanceof Error ? e.message : String(e);
		}
	}

	function handleMessageImageInput(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		void addMessageImages(Array.from(input.files ?? [])).finally(() => (input.value = ''));
	}

	function handleMessagePaste(event: ClipboardEvent) {
		if (!shouldHandleImagePaste(event)) return;
		event.preventDefault();
		void pastedImageFiles(event)
			.then(addMessageImages)
			.catch((e) => (messageAttachmentError = e instanceof Error ? e.message : String(e)));
	}

	function handleMessageKeydown(e: KeyboardEvent) {
		if (slashMenuOpen && filteredCommands.length > 0) {
			if (e.key === 'ArrowDown') {
				e.preventDefault();
				selectedCommandIndex = (selectedCommandIndex + 1) % filteredCommands.length;
				return;
			}
			if (e.key === 'ArrowUp') {
				e.preventDefault();
				selectedCommandIndex = (selectedCommandIndex - 1 + filteredCommands.length) % filteredCommands.length;
				return;
			}
			if (e.key === 'Enter' || e.key === 'Tab') {
				e.preventDefault();
				chooseCommand(filteredCommands[Math.min(selectedCommandIndex, filteredCommands.length - 1)]);
				return;
			}
		}
		if (e.key === 'Escape' && slashMenuOpen) {
			e.preventDefault();
			slashMenuDismissed = true;
			return;
		}
		if (e.key === 'Escape' && modelMenuOpen) {
			e.preventDefault();
			modelMenuOpen = false;
			return;
		}
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !composing && e.keyCode !== 229) {
			e.preventDefault();
			sendTaskMessage();
		}
	}

	function handleMessageInput() {
		selectedCommandIndex = 0;
		slashMenuDismissed = false;
		modelMenuOpen = false;
	}

	async function updateStatus(status: string) {
		if (!task) return;
		try {
			task = await tasks.updateStatus(task.id, status);
			await load();
		} catch (e) {
			alert(String(e));
		}
	}

	function startEditing() {
		if (!task) return;
		editTitle = task.title;
		editDesc = task.description || '';
		editAgentId = task.agent_id || '';
		editPriority = task.priority;
		editDeps = task.depends_on ? [...task.depends_on] : [];
		editing = true;
	}

	function toggleEditDep(id: string) {
		if (editDeps.includes(id)) {
			editDeps = editDeps.filter(d => d !== id);
		} else {
			editDeps = [...editDeps, id];
		}
	}

	async function saveEdit() {
		if (!task) return;
		try {
			// Update task fields
			await tasks.update(task.id, {
				title: editTitle,
				description: editDesc || undefined,
				agent_id: editAgentId || undefined,
				priority: editPriority,
			});
			// Add new dependencies
			const currentDeps = task.depends_on || [];
			for (const depId of editDeps) {
				if (!currentDeps.includes(depId)) {
					await tasks.addDependency(task.id, depId).catch(() => {});
				}
			}
			editing = false;
			await load();
		} catch (e) {
			console.error('Save failed:', e);
		}
	}

	function statusColor(status: string): string {
		switch (status) {
			case 'completed': return 'text-emerald-400';
			case 'in_progress': return 'text-blue-400';
			case 'pending': return 'text-amber-400';
			case 'blocked': return 'text-red-400';
			case 'waiting_for_input': return 'text-orange-400';
			case 'cancelled': return 'text-muted-foreground';
			default: return 'text-muted-foreground';
		}
	}

	function statusBg(status: string): string {
		switch (status) {
			case 'completed': return 'bg-emerald-500/10 border-emerald-500/30';
			case 'in_progress': return 'bg-blue-500/10 border-blue-500/30';
			case 'pending': return 'bg-amber-500/10 border-amber-500/30';
			case 'blocked': return 'bg-red-500/10 border-red-500/30';
			case 'waiting_for_input': return 'bg-orange-500/10 border-orange-500/30';
			default: return 'bg-muted/10 border-border';
		}
	}

	function priorityLabel(p: number): string {
		if (p >= 3) return 'Urgent';
		if (p >= 2) return 'High';
		if (p >= 1) return 'Normal';
		return 'Low';
	}

	function sessionLabel(id: string): string {
		const session = agentList.find(agent => agent.id === id);
		return session?.title || session?.name || id;
	}

	function taskLabel(id: string): string {
		return allTasks.find((candidate) => candidate.id === id)?.title ?? id;
	}

	function changedFileStatus(change: GitChange): string {
		if (change.status === '??') return 'U';
		if (change.status.includes('R')) return 'R';
		if (change.status.includes('A')) return 'A';
		if (change.status.includes('D')) return 'D';
		if (change.status.includes('C')) return 'C';
		return 'M';
	}

	function workspaceFileUrl(agentId: string, path?: string): string {
		const base = `/agents/${encodeURIComponent(agentId)}?tab=files`;
		return path ? `${base}&path=${encodeURIComponent(path)}` : base;
	}

	function startsFreshConversation(): boolean {
		if (!task?.context || typeof task.context !== 'object') return false;
		return (task.context as Record<string, unknown>).session_mode === 'new';
	}

</script>

<div class="flex min-h-0 h-full flex-col">
	<!-- Header -->
	<div class="shrink-0 border-b border-border px-3 py-3 sm:px-6 sm:py-4">
		<div class="flex items-center gap-2 text-sm text-muted-foreground mb-2">
			<a href="/tasks" class="hover:text-foreground">Tasks</a>
			<span>/</span>
			<span class="text-foreground truncate">{task?.title ?? '...'}</span>
		</div>

		{#if error}
			<div class="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">{error}</div>
		{:else if task}
			<div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
				<div class="min-w-0">
					<h1 class="text-lg font-bold sm:text-xl">{task.title}</h1>
					<div class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs sm:text-sm">
						<span class="flex items-center gap-1.5">
							<span class="h-2 w-2 rounded-full {task.status === 'in_progress' ? 'animate-pulse' : ''}
								{task.status === 'completed' ? 'bg-emerald-400' :
								 task.status === 'in_progress' ? 'bg-blue-400' :
								 task.status === 'pending' ? 'bg-amber-400' :
								 task.status === 'waiting_for_input' ? 'bg-orange-400' :
								 task.status === 'blocked' ? 'bg-red-400' :
								 'bg-muted-foreground'}"></span>
							<span class="{statusColor(task.status)}">{task.status.replaceAll('_', ' ')}</span>
						</span>
						{#if task.agent_id}
							<span class="text-muted-foreground">{sessionLabel(task.agent_id)}</span>
						{/if}
						<span class="text-muted-foreground">{priorityLabel(task.priority)}</span>
						<span class="text-xs text-muted-foreground">{timeAgo(task.created_at)}</span>
					</div>
				</div>
				<div class="flex shrink-0 gap-2 overflow-x-auto">
					{#if task.status !== 'completed' && task.status !== 'cancelled'}
						<button onclick={startEditing}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">
							Edit
						</button>
					{/if}
					{#if task.status === 'pending'}
						<button onclick={() => updateStatus('in_progress')}
							class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
							Start
						</button>
					{/if}
					{#if ['in_progress', 'pending', 'waiting_for_input', 'blocked'].includes(task.status)}
						<button onclick={() => updateStatus('completed')}
							class="rounded-md border border-emerald-500/50 px-3 py-1.5 text-xs font-medium text-emerald-400 hover:bg-emerald-500/10">
							Complete
						</button>
						<button onclick={() => updateStatus('cancelled')}
							class="rounded-md border border-border px-3 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent">
							Cancel
						</button>
					{/if}
				</div>
			</div>
		{/if}
	</div>

	{#if editing && task}
		<div class="shrink-0 space-y-3 overflow-y-auto border-b border-border bg-card/50 px-3 py-3 sm:px-6 sm:py-4">
			<input type="text" bind:value={editTitle} placeholder="Task title..."
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring" />
			<textarea bind:value={editDesc} placeholder="Description..." rows="2"
				class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring resize-none"></textarea>
			<div class="flex flex-col gap-3 sm:flex-row">
				<div class="flex-1">
					<div class="text-xs text-muted-foreground mb-1">Agent</div>
					<select bind:value={editAgentId}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
						<option value="">Unassigned</option>
						{#each agentList as agent}
							<option value={agent.id}>{agent.title || agent.name}</option>
						{/each}
					</select>
				</div>
				<div class="w-24">
					<div class="text-xs text-muted-foreground mb-1">Priority</div>
					<select bind:value={editPriority}
						class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring">
						<option value={0}>Normal</option>
						<option value={5}>High</option>
						<option value={10}>Urgent</option>
					</select>
				</div>
			</div>
			{#if availableDeps.length > 0}
				<div>
					<div class="text-xs text-muted-foreground mb-1">Depends on</div>
					<div class="flex flex-wrap gap-1.5 max-h-24 overflow-y-auto">
						{#each availableDeps as dep}
							<button type="button" onclick={() => toggleEditDep(dep.id)}
								class="rounded-md border px-2 py-1 text-xs transition-colors
									{editDeps.includes(dep.id)
										? 'border-primary bg-primary/10 text-primary'
										: 'border-border text-muted-foreground hover:border-primary/50'}">
								{dep.title}
							</button>
						{/each}
					</div>
				</div>
			{/if}
			<div class="flex gap-2">
				<button onclick={saveEdit}
					class="rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90">
					Save
				</button>
				<button onclick={() => (editing = false)}
					class="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-accent">
					Cancel
				</button>
			</div>
		</div>
	{/if}

	{#if loading}
		<div class="flex-1 flex items-center justify-center text-muted-foreground text-sm">Loading...</div>
	{:else if task}
		<div class="flex min-h-0 flex-1 overflow-hidden">
			<!-- Left: conversation -->
			<div class="flex-1 flex flex-col overflow-hidden">
				<div bind:this={messagesEl} onscroll={handleTranscriptScroll} data-task-transcript-scroll class="flex-1 space-y-4 overflow-y-auto px-3 py-4 sm:px-6 sm:py-5">
					{#if hasEarlierActivity}
						<div class="flex justify-center">
							<button type="button" onclick={loadEarlierActivity} disabled={loadingEarlierActivity}
								class="rounded-full border border-border/70 px-3 py-1 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50">
								{loadingEarlierActivity ? 'Loading earlier activity…' : 'Load earlier activity'}
							</button>
						</div>
					{/if}
					<div class="flex flex-wrap items-center gap-2 text-[11px] text-muted-foreground">
						<span class="rounded-full border border-border bg-secondary/40 px-2 py-1">{startsFreshConversation() ? 'Fresh conversation' : 'Continues agent conversation'}</span>
						{#if task.depends_on && task.depends_on.length > 0}<span>Continues its dependency</span>{/if}
					</div>

					<!-- Dependencies -->
					{#if task.blocked_by && task.blocked_by.length > 0}
						<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
							<div class="text-xs font-medium text-amber-500 mb-1">Blocked by</div>
							<div class="space-y-1">
								{#each task.blocked_by as blockerId}
									<a href="/tasks/{blockerId}" class="block text-xs text-amber-400 hover:underline">
										{taskLabel(blockerId)}
									</a>
								{/each}
							</div>
						</div>
					{/if}
					{#if task.depends_on && task.depends_on.length > 0}
						<div class="rounded-lg border border-border/50 p-3 text-sm">
							<div class="text-xs font-medium text-muted-foreground mb-1">Dependencies</div>
							<div class="space-y-1">
								{#each task.depends_on as depId}
									<a href="/tasks/{depId}" class="block text-xs text-muted-foreground hover:underline">
										{#if task.blocked_by?.includes(depId)}⏳{:else}✅{/if} {taskLabel(depId)}
									</a>
								{/each}
							</div>
						</div>
					{/if}

					<!-- One chronological transcript: prompts, agent replies, and native activity. -->
					{#if transcriptItems.length > 0}
						<div class="space-y-0.5" data-task-transcript>
							{#each transcriptItems as item (item.key)}
								{#if item.kind === 'message'}
									{@const isSystem = item.role === 'system'}
									{@const isAssistant = item.role === 'assistant'}
									<div
										class="flex gap-3 py-2 {isSystem ? '' : isAssistant ? '' : 'flex-row-reverse'}"
										data-transcript-kind="message"
										data-message-role={item.role}
										data-transcript-timestamp={item.timestamp}
									>
										<div class="flex-shrink-0 h-7 w-7 rounded-full flex items-center justify-center text-xs font-bold
											{isSystem ? 'bg-muted text-muted-foreground' :
											 isAssistant ? 'bg-accent text-accent-foreground' :
											 'bg-primary text-primary-foreground'}">
											{#if isSystem}S{:else if isAssistant}A{:else}U{/if}
										</div>
										<div class="max-w-[80%]">
											<div class="flex items-center gap-2 mb-0.5">
												<span class="text-xs font-medium {isSystem ? 'text-muted-foreground' : ''}">{item.role}</span>
												<span class="text-xs text-muted-foreground">{timeAgo(item.timestamp)}</span>
											</div>
											<div class="rounded-lg px-3 py-2 text-sm prose prose-invert prose-sm max-w-none
											{isSystem ? 'bg-muted/50 text-muted-foreground text-xs italic' :
											 isAssistant ? 'bg-accent text-accent-foreground' :
											 'bg-primary text-primary-foreground'}">
											{@html renderContent(item.content, { openLinksInNewWindow: isAssistant })}
											<ImageAttachmentPreviews attachments={item.attachments} message />
										</div>
										</div>
									</div>
								{:else}
									<div data-transcript-kind="activity" data-transcript-timestamp={item.timestamp}>
										<ActivityEventRow event={item.event} />
									</div>
								{/if}
							{/each}
						</div>
					{/if}

					{#if subtaskList.length > 0}
					<section class="rounded-lg border border-border/60 bg-card/30 p-3 {compact ? '' : 'lg:hidden'}">
							<div class="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">Steps ({subtaskList.filter((step) => step.status === 'completed').length}/{subtaskList.length})</div>
							<div class="space-y-2">
								{#each subtaskList as subtask}
									<div class="flex items-start gap-2 text-sm"><span class="mt-0.5 {subtask.status === 'completed' ? 'text-emerald-400' : subtask.status === 'in_progress' ? 'text-blue-400' : 'text-muted-foreground'}">{subtask.status === 'completed' ? '✓' : subtask.status === 'in_progress' ? '●' : '○'}</span><span class={subtask.status === 'completed' ? 'text-muted-foreground line-through' : ''}>{subtask.title}</span></div>
								{/each}
							</div>
						</section>
					{/if}

					{#if latestResult}
						<section class="rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-4">
							<div class="mb-2 flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-emerald-400">
								<span class="h-2 w-2 rounded-full bg-emerald-400"></span>
								Result
							</div>
							<div class="prose prose-invert prose-sm max-w-none">{@html renderContent(latestResult, { openLinksInNewWindow: true })}</div>
						</section>
					{:else if latestError}
						<section class="rounded-lg border border-red-500/30 bg-red-500/5 p-4">
							<div class="mb-2 text-xs font-medium uppercase tracking-wide text-red-400">Attempt failed</div>
							<div class="whitespace-pre-wrap text-sm text-red-200">{latestError}</div>
						</section>
					{/if}

					{#if transcriptItems.length === 0 && !latestResult && !latestError}
						<div class="flex h-full items-center justify-center text-sm text-muted-foreground">
							<div class="space-y-1 text-center">
								<div class="text-3xl">&#x1f4cb;</div>
								<div>No activity yet</div>
							</div>
						</div>
					{/if}

					{#if pendingElicitation}
						{@const question = pendingElicitation}
						{@const questionPage = elicitationPage[question.id] ?? 0}
						{@const isReviewPage = questionPage >= question.fields.length}
						<section class="rounded-xl border border-orange-500/35 bg-orange-500/5 shadow-sm">
							<div class="flex items-center justify-between gap-3 border-b border-orange-500/20 px-4 py-3">
								<div class="flex min-w-0 items-center gap-2">
									<span class="h-2 w-2 shrink-0 animate-pulse rounded-full bg-orange-400"></span>
									<span class="truncate text-xs font-semibold uppercase tracking-wide text-orange-300">Agent question</span>
								</div>
								<span class="shrink-0 text-xs text-muted-foreground">
									{isReviewPage ? 'Review' : `${questionPage + 1} / ${question.fields.length}`}
								</span>
							</div>

							<div class="space-y-4 p-4">
								{#if isReviewPage}
									<div>
										<h3 class="text-sm font-semibold text-foreground">Review your answers</h3>
										<p class="mt-1 text-xs text-muted-foreground">These answers go directly back to the active agent turn.</p>
									</div>
									<div class="space-y-2">
										{#each question.fields as field}
											<div class="rounded-lg border border-border/70 bg-background/50 px-3 py-2.5">
												<div class="text-xs text-muted-foreground">{field.property.title || field.question}</div>
												<div class="mt-1 whitespace-pre-wrap text-sm font-medium text-foreground">{displayElicitationAnswer(question, field)}</div>
											</div>
										{/each}
									</div>
									<div class="flex flex-wrap items-center gap-2 pt-1">
										<button type="button" onclick={() => elicitationPage = { ...elicitationPage, [question.id]: Math.max(0, question.fields.length - 1) }}
											class="rounded-lg border border-border px-3 py-2 text-xs font-medium hover:bg-accent">Back</button>
										<button type="button" onclick={() => respondToElicitation(question, 'decline')} disabled={elicitationSending}
											class="rounded-lg px-3 py-2 text-xs font-medium text-muted-foreground hover:bg-accent disabled:opacity-50">Skip</button>
										<button type="button" onclick={() => respondToElicitation(question, 'accept')} disabled={elicitationSending}
											class="ml-auto rounded-lg bg-primary px-4 py-2 text-xs font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
											{elicitationSending ? 'Sending…' : 'Send answers'}
										</button>
									</div>
								{:else}
									{@const field = question.fields[questionPage]}
									{@const fieldOptions = optionsFor(field)}
									<div>
										{#if field.property.title}
											<div class="mb-1 text-xs font-semibold uppercase tracking-wide text-orange-300">{field.property.title}</div>
										{/if}
										<div class="whitespace-pre-wrap text-sm font-medium leading-relaxed text-foreground">{field.question}</div>
									</div>

									{#if fieldOptions.length > 0}
										<div class="grid gap-2">
											{#each fieldOptions as option}
												{@const selected = optionSelected(question.id, field, option.value)}
												<button type="button" onclick={() => toggleElicitationOption(question.id, field, option.value)}
													class="flex w-full items-start gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors {selected ? 'border-primary/60 bg-primary/15' : 'border-border/70 bg-background/50 hover:border-primary/35 hover:bg-accent/50'}">
													<span class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center border {field.property.type === 'array' ? 'rounded' : 'rounded-full'} {selected ? 'border-primary bg-primary text-primary-foreground' : 'border-muted-foreground/50'}">
														{#if selected}<span class="text-[10px] leading-none">✓</span>{/if}
													</span>
													<span class="min-w-0">
														<span class="block text-sm font-medium text-foreground">{option.title}</span>
														{#if option.description}<span class="mt-0.5 block text-xs leading-relaxed text-muted-foreground">{option.description}</span>{/if}
													</span>
												</button>
											{/each}
										</div>
									{:else if field.property.type === 'boolean'}
										<label class="flex items-center gap-3 rounded-lg border border-border/70 bg-background/50 px-3 py-3 text-sm">
											<input type="checkbox" checked={Boolean(answersFor(question.id)[field.key] ?? field.property.default ?? false)}
												onchange={(event) => setElicitationAnswer(question.id, field.key, event.currentTarget.checked)} />
											<span>{field.property.title || 'Yes'}</span>
										</label>
									{:else if field.property.type === 'number' || field.property.type === 'integer'}
										<input type="number" value={String(answersFor(question.id)[field.key] ?? field.property.default ?? '')}
											oninput={(event) => setElicitationAnswer(question.id, field.key, event.currentTarget.value === '' ? undefined : Number(event.currentTarget.value))}
											class="w-full rounded-lg border border-input bg-background px-3 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-ring" />
									{:else}
										<textarea rows="3" value={String(answersFor(question.id)[field.key] ?? field.property.default ?? '')}
											oninput={(event) => setElicitationAnswer(question.id, field.key, event.currentTarget.value)}
											placeholder="Type your answer..."
											class="w-full resize-y rounded-lg border border-input bg-background px-3 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-ring"></textarea>
									{/if}

									{#if field.customKey}
										<div>
											<label for="elicitation-custom-{question.id}-{questionPage}" class="mb-1.5 block text-xs font-medium text-muted-foreground">Other answer</label>
											<textarea id="elicitation-custom-{question.id}-{questionPage}" rows="2"
												value={String(answersFor(question.id)[field.customKey] ?? '')}
												oninput={(event) => field.customKey && setElicitationAnswer(question.id, field.customKey, event.currentTarget.value)}
												placeholder={field.customProperty?.description || 'Type a different answer...'}
												class="w-full resize-y rounded-lg border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"></textarea>
										</div>
									{/if}

									<div class="flex flex-wrap items-center gap-2 pt-1">
										{#if questionPage > 0}
											<button type="button" onclick={() => elicitationPage = { ...elicitationPage, [question.id]: questionPage - 1 }}
												class="rounded-lg border border-border px-3 py-2 text-xs font-medium hover:bg-accent">Back</button>
										{/if}
										<button type="button" onclick={() => respondToElicitation(question, 'decline')} disabled={elicitationSending}
											class="rounded-lg px-3 py-2 text-xs font-medium text-muted-foreground hover:bg-accent disabled:opacity-50">Skip</button>
										<button type="button" onclick={() => elicitationPage = { ...elicitationPage, [question.id]: questionPage + 1 }}
											class="ml-auto rounded-lg bg-primary px-4 py-2 text-xs font-semibold text-primary-foreground hover:bg-primary/90">
											{questionPage < question.fields.length - 1 ? 'Next' : 'Review'}
										</button>
									</div>
								{/if}
							</div>
						</section>
					{/if}

					<!-- Live indicator -->
					{#if runningAttempt}
						<div class="flex items-center gap-2 text-xs text-muted-foreground">
							<span class="h-2 w-2 rounded-full bg-blue-400 animate-pulse"></span>
							{queuedAttempt
								? 'New guidance queued; switching at the next safe break...'
								: 'The agent is working on this task...'}
						</div>
					{:else if queuedAttempt}
						<div class="flex items-center gap-2 text-xs text-muted-foreground">
							<span class="h-2 w-2 rounded-full bg-amber-400 animate-pulse"></span>
							The next worker turn is queued...
						</div>
					{:else if task.status === 'waiting_for_input'}
						<div class="flex items-center gap-2 text-xs text-orange-400">
							<span class="h-2 w-2 rounded-full bg-orange-400 animate-pulse"></span>
							Waiting for your response...
						</div>
					{:else if task.status === 'in_progress' && subtaskList.some(subtask => subtask.status !== 'completed')}
						<div class="flex items-center gap-2 text-xs text-amber-400">
							<span class="h-2 w-2 rounded-full bg-amber-400"></span>
							This task still has unfinished steps.
						</div>
					{/if}
				</div>
				{#if showJumpToLatest}
					<div class="relative z-20 h-0">
						<button
							type="button"
							onclick={jumpToLatest}
							class="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-border/80 bg-card/95 px-3 py-1.5 text-xs font-medium text-foreground shadow-lg backdrop-blur hover:bg-accent"
						>
							<svg class="h-3.5 w-3.5" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
							Jump to latest
						</button>
					</div>
				{/if}

				<!-- Message input -->
				<div class="shrink-0 border-t border-border bg-background px-3 pb-4 pt-3 sm:px-6 sm:pb-5 sm:pt-4">
					{#if task.status === 'waiting_for_input'}
						<div class="text-xs text-orange-400 mb-2">The agent needs additional input</div>
					{:else if !task.agent_id}
						<div class="text-xs text-muted-foreground mb-2">Assign an agent before sending a message</div>
					{/if}
					<div bind:this={composerEl} class="relative rounded-xl border border-border bg-secondary/35 transition-all focus-within:border-primary/45 focus-within:ring-1 focus-within:ring-primary/20">
						{#if slashMenuOpen}
							<div class="absolute bottom-full left-0 right-0 z-40 mb-2 max-h-72 overflow-y-auto rounded-xl border border-border bg-card p-1.5 shadow-2xl sm:right-auto sm:w-96">
								{#if filteredCommands.length > 0}
									{#each filteredCommands as command, index}
										<button
											type="button"
											onpointerenter={() => (selectedCommandIndex = index)}
											onclick={() => chooseCommand(command)}
											class="block w-full rounded-lg px-3 py-2 text-left transition-colors {index === selectedCommandIndex ? 'bg-accent' : 'hover:bg-accent/60'}"
										>
											<span class="block font-mono text-xs text-foreground">{command.name.startsWith('/') ? command.name : `/${command.name}`}</span>
											<span class="mt-0.5 block text-[11px] leading-snug text-muted-foreground">{command.description}</span>
										</button>
									{/each}
								{:else}
									<div class="px-3 py-3 text-xs text-muted-foreground">No matching commands</div>
								{/if}
							</div>
						{/if}

						{#if modelMenuOpen && hasModelMenu}
							<div class="absolute bottom-full left-0 right-0 z-30 mb-2 max-h-[65vh] overflow-y-auto rounded-xl border border-border bg-card p-2 shadow-2xl sm:right-auto sm:w-80">
								{#if modelOption}
									<div class="px-2 pb-1 pt-1 text-[10px] font-medium uppercase tracking-[0.14em] text-muted-foreground/60">Model</div>
									<div class="space-y-0.5">
										{#each selectChoices(modelOption) as choice}
											<button type="button" onclick={() => setConfigOption(modelOption!, choice.value)}
												class="flex w-full items-start gap-2 rounded-lg px-2 py-2 text-left transition-colors {String(selectedValue(modelOption!)) === choice.value ? 'bg-accent' : 'hover:bg-accent/60'}">
												<span class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full border {String(selectedValue(modelOption!)) === choice.value ? 'border-primary' : 'border-muted-foreground/35'}">
													{#if String(selectedValue(modelOption!)) === choice.value}<span class="h-2 w-2 rounded-full bg-primary"></span>{/if}
												</span>
												<span class="min-w-0"><span class="block text-xs text-foreground">{choice.name}</span>{#if choice.description}<span class="mt-0.5 block text-[11px] leading-snug text-muted-foreground">{choice.description}</span>{/if}</span>
											</button>
										{/each}
									</div>
								{/if}

								{#if reasoningOption}
									<div class="mx-2 my-2 border-t border-border/60"></div>
									<div class="px-2 pb-1 text-[10px] font-medium uppercase tracking-[0.14em] text-muted-foreground/60">Reasoning effort</div>
									<div class="flex flex-wrap gap-1 px-2 pb-1">
										{#each selectChoices(reasoningOption) as choice}
											<button type="button" onclick={() => setConfigOption(reasoningOption!, choice.value)} title={choice.description || choice.name}
												class="rounded-md px-2 py-1 text-[11px] transition-colors {String(selectedValue(reasoningOption!)) === choice.value ? 'bg-accent text-foreground' : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'}">
												{choice.name}
											</button>
										{/each}
									</div>
								{/if}

								{#each modelConfigOptions as option}
									<div class="mx-2 my-2 border-t border-border/60"></div>
									{#if option.type === 'boolean'}
										<button type="button" onclick={() => setConfigOption(option, !Boolean(selectedValue(option)))} class="flex w-full items-center justify-between rounded-lg px-2 py-2 text-left hover:bg-accent/60">
											<span><span class="block text-xs text-foreground">{option.name}</span>{#if option.description}<span class="mt-0.5 block text-[11px] text-muted-foreground">{option.description}</span>{/if}</span>
											<span class="relative h-4 w-7 rounded-full transition-colors {Boolean(selectedValue(option)) ? 'bg-primary' : 'bg-muted'}"><span class="absolute top-0.5 h-3 w-3 rounded-full bg-white transition-transform {Boolean(selectedValue(option)) ? 'translate-x-3.5' : 'translate-x-0.5'}"></span></span>
										</button>
									{:else}
										<div class="px-2 pb-1 text-[10px] font-medium uppercase tracking-[0.14em] text-muted-foreground/60">{option.name}</div>
										<div class="flex flex-wrap gap-1 px-2">
											{#each selectChoices(option) as choice}
												<button type="button" onclick={() => setConfigOption(option, choice.value)} class="rounded-md px-2 py-1 text-[11px] {String(selectedValue(option)) === choice.value ? 'bg-accent text-foreground' : 'text-muted-foreground hover:bg-accent/60'}">{choice.name}</button>
											{/each}
										</div>
									{/if}
								{/each}
							</div>
						{/if}

						<ImageAttachmentPreviews attachments={messageImagePreviews} onremove={(index) => (messageAttachments = messageAttachments.filter((_, itemIndex) => itemIndex !== index))} />

						<textarea
							id="task-message-input-{taskId}"
							bind:value={messageInput}
							oninput={handleMessageInput}
							onfocus={() => (messageInputFocused = true)}
							onblur={() => setTimeout(() => (messageInputFocused = false), 150)}
							onkeydown={handleMessageKeydown}
							onpaste={handleMessagePaste}
							oncompositionstart={() => (composing = true)}
							oncompositionend={() => setTimeout(() => (composing = false), 0)}
							placeholder={messagePlaceholder}
							rows={2}
							class="max-h-32 w-full resize-none rounded-xl bg-transparent px-4 pb-1 pt-3 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
							disabled={messageSending || interrupting || !task.agent_id || Boolean(pendingElicitation)}
						></textarea>
						{#if messageAttachmentError}<div class="px-4 pb-1 text-xs text-destructive">{messageAttachmentError}</div>{/if}

						<div class="flex min-h-9 items-center gap-2 px-3 pb-2">
							<input bind:this={messageImageInput} type="file" accept={IMAGE_FILE_ACCEPT} multiple onchange={handleMessageImageInput} class="hidden" />
							<button type="button" onclick={() => messageImageInput?.click()}
								disabled={messageSending || interrupting || !task.agent_id || Boolean(pendingElicitation) || messageAttachments.length >= MAX_IMAGE_ATTACHMENTS}
								aria-label="Attach images" title="Attach images (you can also paste)"
								class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-30">
								<svg class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>
							</button>
							{#if task.agent_id && (otherConfigOptions.length > 0 || hasModelMenu)}
								<div class="flex min-w-0 flex-1 items-center gap-3 overflow-x-auto scrollbar-hide">
									{#each otherConfigOptions as option}
										{#if option.type === 'boolean'}
											<button type="button" onclick={() => setConfigOption(option, !Boolean(selectedValue(option)))} title={option.description || option.name}
												class="shrink-0 text-xs transition-colors {Boolean(selectedValue(option)) ? 'text-foreground' : 'text-muted-foreground/60'} hover:text-foreground">
												{option.name}
											</button>
										{:else}
											<label class="relative shrink-0" title={option.description || option.name}>
												<span class="sr-only">{option.name}</span>
												<select value={String(selectedValue(option))} onchange={(event) => setConfigOption(option, event.currentTarget.value)}
											class="composer-value-select max-w-36 cursor-pointer appearance-none bg-transparent py-1 pl-0 pr-4 text-xs text-muted-foreground outline-none transition-colors hover:text-foreground">
													{#each selectChoices(option) as choice}<option value={choice.value}>{choice.name}</option>{/each}
												</select>
												<svg class="pointer-events-none absolute right-0 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
											</label>
										{/if}
									{/each}

									{#if hasModelMenu}
										<button type="button" onclick={() => (modelMenuOpen = !modelMenuOpen)} aria-expanded={modelMenuOpen} title="Model and reasoning effort"
											class="flex shrink-0 items-center gap-1 py-1 text-xs text-muted-foreground transition-colors hover:text-foreground">
											<span>{modelOption ? selectedChoiceName(modelOption) : 'Model settings'}</span>
											<svg class="h-3 w-3 text-muted-foreground/50" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m6 9 6 6 6-6" /></svg>
										</button>
									{/if}
								</div>
							{:else}
								<div class="flex-1"></div>
							{/if}
							{#if runningAttempt}
								<button
									type="button"
									onclick={interruptAgent}
									aria-label={!pendingElicitation && (messageInput.trim() || messageAttachments.length > 0) ? 'Interrupt and send now' : 'Interrupt agent now'}
									title={!pendingElicitation && (messageInput.trim() || messageAttachments.length > 0) ? 'Interrupt the current work and send this message now' : 'Interrupt the current work now'}
									disabled={messageSending || interrupting}
									class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-border text-muted-foreground transition-colors hover:border-destructive/50 hover:bg-destructive/10 hover:text-destructive disabled:cursor-not-allowed disabled:opacity-30"
								>
									{#if interrupting}
										<svg class="h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24" aria-hidden="true"><circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3"/><path class="opacity-75" fill="currentColor" d="M21 12a9 9 0 0 0-9-9v3a6 6 0 0 1 6 6z"/></svg>
									{:else}
										<svg class="h-3.5 w-3.5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="1.5"/></svg>
									{/if}
								</button>
							{/if}
							<button
								onclick={() => sendTaskMessage()}
								aria-label="Send message"
								disabled={(!messageInput.trim() && messageAttachments.length === 0) || messageSending || interrupting || !task.agent_id || Boolean(pendingElicitation)}
								class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground transition-colors hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-30"
							>
								<svg class="h-4 w-4" fill="currentColor" viewBox="0 0 24 24"><path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z"/></svg>
							</button>
						</div>
					</div>
				</div>
			</div>

			<!-- Right: details sidebar -->
			<div class="w-72 shrink-0 space-y-4 overflow-y-auto border-l border-border p-4 {compact ? 'hidden' : 'hidden lg:block'}">
				<!-- Details -->
				<div class="space-y-2">
					<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Details</h3>
					<dl class="space-y-1.5 text-sm">
						<div class="flex justify-between">
							<dt class="text-muted-foreground">ID</dt>
							<dd class="font-mono text-xs truncate max-w-[140px]">{task.id}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Status</dt>
							<dd class="{statusColor(task.status)}">{task.status.replaceAll('_', ' ')}</dd>
						</div>
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Priority</dt>
							<dd>{priorityLabel(task.priority)}</dd>
						</div>
						{#if task.agent_id}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Agent</dt>
								<dd><a href="/agents/{task.agent_id}" class="underline hover:text-foreground">{sessionLabel(task.agent_id)}</a></dd>
							</div>
						{/if}
						<div class="flex justify-between">
							<dt class="text-muted-foreground">Created</dt>
							<dd class="text-xs">{timeAgo(task.created_at)}</dd>
						</div>
						{#if task.completed_at}
							<div class="flex justify-between">
								<dt class="text-muted-foreground">Completed</dt>
								<dd class="text-xs">{timeAgo(task.completed_at)}</dd>
							</div>
						{/if}
					</dl>
				</div>

				{#if contextUsage}
					<div class="space-y-2" data-context-usage>
						<div class="flex items-center justify-between">
							<h3 class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Context window</h3>
							<span class="text-xs tabular-nums text-muted-foreground">{contextUsage.percent.toFixed(1)}%</span>
						</div>
						<div class="h-1.5 overflow-hidden rounded-full bg-secondary">
							<div class="h-full rounded-full bg-blue-500 transition-[width] duration-300" style:width={`${contextUsage.percent}%`}></div>
						</div>
						<div class="text-right text-xs tabular-nums text-muted-foreground">
							<span class="text-foreground">{formatTokens(contextUsage.used)}</span>
							<span> / {formatTokens(contextUsage.size)} tokens</span>
						</div>
					</div>
				{/if}

				{#if task.agent_id && workspaceGit}
					<div class="space-y-2" data-task-changed-files>
						<div class="flex items-center justify-between gap-2">
							<h3 class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Changed files</h3>
							<a href={workspaceFileUrl(task.agent_id)} class="text-[11px] text-muted-foreground hover:text-foreground">Open files</a>
						</div>
						{#if workspaceGit.repository && workspaceGit.files.length > 0}
							<div class="space-y-0.5">
								{#each workspaceGit.files.slice(0, 12) as change (change.path)}
									<a
										href={workspaceFileUrl(task.agent_id, change.path)}
										title={change.path}
										class="flex items-center gap-2 rounded px-1.5 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
									>
										<span class="w-3 shrink-0 font-mono text-[10px] text-amber-500">{changedFileStatus(change)}</span>
										<span class="truncate">{change.path}</span>
									</a>
								{/each}
								{#if workspaceGit.files.length > 12}
									<a href={workspaceFileUrl(task.agent_id)} class="block px-1.5 pt-1 text-[11px] text-muted-foreground hover:text-foreground">
										+{workspaceGit.files.length - 12} more
									</a>
								{/if}
							</div>
						{:else if workspaceGit.repository}
							<p class="text-xs text-muted-foreground">Working tree clean</p>
						{:else}
							<p class="text-xs text-muted-foreground">Not a Git repository</p>
						{/if}
					</div>
				{/if}

				<!-- Subtasks -->
				{#if subtaskList.length > 0}
					<div class="space-y-2">
						<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">
							Steps ({subtaskList.filter(s => s.status === 'completed').length}/{subtaskList.length})
						</h3>
						<div class="space-y-1.5">
							{#each subtaskList as sub}
								<div class="flex items-start gap-2 rounded p-1.5 text-sm">
									<span class="mt-0.5 flex-shrink-0 h-4 w-4 rounded border flex items-center justify-center
										{sub.status === 'completed'
											? 'bg-emerald-500/20 border-emerald-500 text-emerald-400'
											: sub.status === 'in_progress'
											? 'border-blue-400 text-blue-400'
											: 'border-muted-foreground/30'}">
										{#if sub.status === 'completed'}
											<svg class="h-3 w-3" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24">
												<path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
											</svg>
										{:else if sub.status === 'in_progress'}
											<span class="h-1.5 w-1.5 rounded-full bg-blue-400 animate-pulse"></span>
										{/if}
									</span>
									<div class="flex-1 min-w-0">
										<span class="block truncate {sub.status === 'completed' ? 'line-through text-muted-foreground' : ''}">{sub.title}</span>
										{#if sub.description}
											<span class="block text-xs text-muted-foreground mt-0.5 line-clamp-2">{sub.description}</span>
										{/if}
									</div>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if task.agent_id}
					<div class="space-y-2">
						<h3 class="text-xs font-medium text-muted-foreground uppercase tracking-wide">Agent</h3>
						<a href="/agents/{task.agent_id}" class="text-sm underline hover:text-foreground">Open agent</a>
					</div>
				{/if}
			</div>
		</div>
	{/if}
</div>
