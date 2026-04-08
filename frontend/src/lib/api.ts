const BASE = '';

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	const res = await fetch(`${BASE}${path}`, {
		headers: { 'Content-Type': 'application/json' },
		...init
	});
	if (!res.ok) {
		const body = await res.json().catch(() => ({ error: res.statusText }));
		throw new Error(body.error || res.statusText);
	}
	if (res.status === 204 || res.headers.get('content-length') === '0') return undefined as T;
	const text = await res.text();
	if (!text) return undefined as T;
	return JSON.parse(text);
}

// -- Conversations --

export interface Conversation {
	id: string;
	title: string | null;
	icon: string | null;
	created_at: string;
	updated_at: string;
	last_message_at: string | null;
	participants: ConversationParticipant[];
}

export interface ConversationParticipant {
	participant_type: string;
	participant_id: string;
	joined_at: string;
}

export interface ConversationMessage {
	id: number;
	conversation_id: string;
	sender_type: string;
	sender_id: string;
	sender_name: string | null;
	content: string;
	message_type: string;
	created_at: string;
}

export interface StreamCallbacks {
	onUserMessage?: (msg: ConversationMessage) => void;
	onThinking?: (agentId: string) => void;
	onChunk?: (agentId: string, content: string) => void;
	onAgentMessage?: (msg: ConversationMessage) => void;
	onError?: (agentId: string | null, error: string) => void;
	onDone?: () => void;
}

export const conversations = {
	list: (limit = 50) => request<Conversation[]>(`/api/conversations?limit=${limit}`),
	get: (id: string) => request<Conversation>(`/api/conversations/${id}`),
	create: (data: { title?: string; icon?: string; participant_ids?: string[] }) =>
		request<Conversation>('/api/conversations', { method: 'POST', body: JSON.stringify(data) }),
	update: (id: string, data: { title?: string; icon?: string }) =>
		request<Conversation>(`/api/conversations/${id}`, { method: 'PATCH', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/conversations/${id}`, { method: 'DELETE' }),
	stop: (id: string, agentId?: string) => {
		const params = agentId ? `?agent_id=${encodeURIComponent(agentId)}` : '';
		return request<void>(`/api/conversations/${id}/stop${params}`, { method: 'POST' });
	},
	messages: (id: string, limit = 50, beforeId?: number) => {
		const params = new URLSearchParams({ limit: String(limit) });
		if (beforeId) params.set('before_id', String(beforeId));
		return request<ConversationMessage[]>(`/api/conversations/${id}/messages?${params}`);
	},
	sendMessage: (id: string, content: string, senderName?: string) =>
		request<ConversationMessage[]>(`/api/conversations/${id}/messages`, {
			method: 'POST',
			body: JSON.stringify({ content, sender_name: senderName })
		}),
	streamMessage: (id: string, content: string, senderName: string | undefined, callbacks: StreamCallbacks): (() => void) => {
		const controller = new AbortController();

		(async () => {
			try {
				const res = await fetch(`${BASE}/api/conversations/${id}/messages/stream`, {
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ content, sender_name: senderName }),
					signal: controller.signal
				});
				if (!res.ok) {
					const body = await res.json().catch(() => ({ error: res.statusText }));
					callbacks.onError?.(null, body.error || res.statusText);
					return;
				}

				const reader = res.body?.getReader();
				if (!reader) return;

				const decoder = new TextDecoder();
				let buffer = '';
				let currentEvent = '';

				while (true) {
					const { done, value } = await reader.read();
					if (done) break;

					buffer += decoder.decode(value, { stream: true });

					// Process complete SSE messages (separated by \n\n)
					while (buffer.includes('\n\n')) {
						const idx = buffer.indexOf('\n\n');
						const block = buffer.slice(0, idx);
						buffer = buffer.slice(idx + 2);

						let eventType = '';
						let data = '';
						for (const line of block.split('\n')) {
							if (line.startsWith('event:')) eventType = line.slice(6).trim();
							else if (line.startsWith('data:')) data = line.slice(5).trim();
						}

						if (!data || data === 'ping') continue;

						try {
							const parsed = JSON.parse(data);
							switch (eventType) {
								case 'user_message':
									callbacks.onUserMessage?.(parsed);
									break;
								case 'thinking':
									callbacks.onThinking?.(parsed.agent_id);
									break;
								case 'chunk':
									callbacks.onChunk?.(parsed.agent_id, parsed.content);
									break;
								case 'agent_message':
									callbacks.onAgentMessage?.(parsed);
									break;
								case 'error':
									callbacks.onError?.(parsed.agent_id ?? null, parsed.error);
									break;
								case 'done':
									callbacks.onDone?.();
									break;
							}
						} catch { /* skip unparseable */ }
					}
				}

				callbacks.onDone?.();
			} catch (e) {
				if (!controller.signal.aborted) {
					callbacks.onError?.(null, e instanceof Error ? e.message : String(e));
				}
			}
		})();

		return () => controller.abort();
	},
	/** Subscribe to conversation events via SSE (ADR-019).
	 * Replays missed messages from DB, then streams live events.
	 * Returns a cleanup function to close the connection. */
	subscribeEvents: (id: string, afterMessageId: number, callbacks: StreamCallbacks): { cancel: () => void; ready: Promise<void> } => {
		const url = `${BASE}/api/conversations/${id}/events?after=${afterMessageId}`;
		const eventSource = new EventSource(url);

		// Resolves when the SSE connection is established
		let resolveReady: () => void;
		const ready = new Promise<void>(r => { resolveReady = r; });
		eventSource.addEventListener('open', () => resolveReady());

		eventSource.addEventListener('thinking', (e) => {
			try {
				const d = JSON.parse(e.data);
				callbacks.onThinking?.(d.agent_id);
			} catch {}
		});
		eventSource.addEventListener('chunk', (e) => {
			try {
				const d = JSON.parse(e.data);
				callbacks.onChunk?.(d.agent_id, d.content);
			} catch {}
		});
		eventSource.addEventListener('agent_message', (e) => {
			try {
				const data = JSON.parse(e.data);
				// Live events are wrapped: {type, message: {...}}
				// Replayed events are raw: {id, content, ...}
				const msg = data.message ?? data;
				callbacks.onAgentMessage?.(msg);
			} catch {}
		});
		eventSource.addEventListener('error', (e) => {
			if (e instanceof MessageEvent) {
				try { const d = JSON.parse(e.data); callbacks.onError?.(d.agent_id ?? null, d.error); } catch {}
			}
		});
		eventSource.addEventListener('done', () => {
			callbacks.onDone?.();
		});

		return { cancel: () => eventSource.close(), ready };
	},
	addParticipant: (id: string, participantType: string, participantId: string) =>
		request<void>(`/api/conversations/${id}/participants`, {
			method: 'POST',
			body: JSON.stringify({ participant_type: participantType, participant_id: participantId })
		}),
	removeParticipant: (id: string, participantId: string) =>
		request<void>(`/api/conversations/${id}/participants/${participantId}`, { method: 'DELETE' })
};

// -- Agents --

export interface Agent {
	id: string;
	name: string;
	backend: string;
	status: string;
	desired_status: string;
	observed_status: string;
	container_id: string | null;
	config?: {
		display_name?: string | null;
		role_title?: string | null;
		responsibilities?: string | null;
		avatar?: string | null;
		role?: string;
		model?: string | null;
		tools?: string[];
		skills?: string[];
		volumes?: string[];
		idle_prompt?: string | null;
	};
	created_at: string;
	started_at: string | null;
	stopped_at: string | null;
	error_message: string | null;
	restart_count: number;
}

export const agents = {
	list: () => request<Agent[]>('/api/agents'),
	get: (id: string) => request<Agent>(`/api/agents/${id}`),
	start: (id: string) => request<Agent>(`/api/agents/${id}/start`, { method: 'POST', body: '{}' }),
	stop: (id: string) => request<Agent>(`/api/agents/${id}/stop`, { method: 'POST', body: '{}' }),
	delete: (id: string) => request<void>(`/api/agents/${id}`, { method: 'DELETE' }),
	updateConfig: (id: string, data: {
		display_name?: string | null;
		role_title?: string | null;
		responsibilities?: string | null;
		avatar?: string | null;
		role?: string;
		model?: string;
		llm?: { provider: string | null; api_key: string | null; base_url: string | null };
		tools?: string[];
		skills?: string[];
		volumes?: string[];
		budget?: {
			daily: string | null;
			monthly: string | null;
			per_task: string | null;
			on_exceeded: string;
			fallback_model: string;
			warn_at_percent: number;
		} | null;
		rate_limit?: {
			requests_per_minute: number;
			tokens_per_minute: number;
			concurrent_requests: number;
		} | null;
		wake_on?: { schedule: string | null; event: string | null; condition: string | null }[];
		hooks?: { before_message: string[]; after_message: string[] };
		idle_prompt?: string | null;
	}) => request<{ agent: LiveConfig['agents'][0]; needs_restart: boolean }>(
		`/api/agents/${id}/config`, { method: 'PATCH', body: JSON.stringify(data) }
	),
	logs: (id: string, tail = 100) =>
		request<{ logs: string }>(`/api/agents/${id}/logs?tail=${tail}`)
};

// -- Tasks --

export interface Task {
	id: string;
	title: string;
	description: string | null;
	status: string;
	priority: number;
	agent_id: string | null;
	parent_task_id: string | null;
	sop_id: string | null;
	created_at: string;
	updated_at: string;
	completed_at: string | null;
	context: unknown;
	depends_on?: string[];
	dependents?: string[];
	blocked_by?: string[];
	ready?: boolean;
}

export interface TaskCounts {
	pending: number;
	in_progress: number;
	waiting_for_input: number;
	blocked: number;
	completed: number;
	cancelled: number;
}

export const tasks = {
	list: (status?: string, agentId?: string) => {
		const params = new URLSearchParams();
		if (status) params.set('status', status);
		if (agentId) params.set('agent_id', agentId);
		const qs = params.toString();
		return request<{ tasks: Task[]; counts: TaskCounts }>(`/api/tasks${qs ? `?${qs}` : ''}`);
	},
	get: (id: string) => request<Task>(`/api/tasks/${id}`),
	create: (data: { title: string; description?: string; agent_id?: string; priority?: number }) =>
		request<Task>('/api/tasks', { method: 'POST', body: JSON.stringify(data) }),
	update: (id: string, data: { title?: string; description?: string; agent_id?: string; priority?: number }) =>
		request<Task>(`/api/tasks/${id}`, {
			method: 'PATCH',
			body: JSON.stringify(data)
		}),
	updateStatus: (id: string, status: string) =>
		request<Task>(`/api/tasks/${id}/status`, {
			method: 'PATCH',
			body: JSON.stringify({ status })
		}),
	delete: (id: string) => request<void>(`/api/tasks/${id}`, { method: 'DELETE' }),
	messages: (id: string) => request<TaskMessage[]>(`/api/tasks/${id}/messages`),
	addMessage: (id: string, role: string, content: string) =>
		request<TaskMessage>(`/api/tasks/${id}/messages`, {
			method: 'POST',
			body: JSON.stringify({ role, content })
		}),
	subtasks: (id: string) => request<{ tasks: Task[]; counts: TaskCounts }>(`/api/tasks?parent_task_id=${id}`),
	createBatch: (data: { tasks: { ref: string; title: string; description?: string; agent_id?: string; depends_on?: string[] }[]; parent_task_id?: string }) =>
		request<Task[]>('/api/tasks/batch', { method: 'POST', body: JSON.stringify(data) }),
	addDependency: (taskId: string, dependsOn: string) =>
		request<{ task_id: string; depends_on: string }>(`/api/tasks/${taskId}/dependencies`, {
			method: 'POST',
			body: JSON.stringify({ depends_on: dependsOn })
		}),
};

export interface TaskMessage {
	id: number;
	task_id: string;
	role: string;
	content: string;
	timestamp: string;
}

// -- Memory --

export interface Memory {
	id: string;
	content: string;
	summary: string;
	source: string;
	layer: string;
	agent_id: string | null;
	tags: string[];
	created_at: string;
	accessed_at: string;
	access_count: number;
}

export interface MemorySearchResult {
	memory: Memory;
	relevance_score: number;
	source: string;
}

export interface MemoryStats {
	zettelkasten: { total_memories: number; total_links: number; total_tags: number };
	vector: { embedding_count: number; dimension: number; model: string };
}

export const memory = {
	list: (limit = 50, agentId?: string) => {
		const params = new URLSearchParams({ limit: String(limit) });
		if (agentId) params.set('agent_id', agentId);
		return request<MemorySearchResult[]>(`/api/memory?${params}`);
	},
	get: (id: string) => request<Memory>(`/api/memory/${id}`),
	search: (q: string, limit = 10) =>
		request<MemorySearchResult[]>(`/api/memory/search?q=${encodeURIComponent(q)}&limit=${limit}`),
	create: (data: { content: string; summary: string; source: string; tags?: string[] }) =>
		request<Memory>('/api/memory', { method: 'POST', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/memory/${id}`, { method: 'DELETE' }),
	stats: () => request<MemoryStats>('/api/memory/stats'),
	related: (id: string) => request<MemorySearchResult[]>(`/api/memory/${id}/related`)
};

// -- Schedules --

export interface Schedule {
	id: string;
	name: string;
	cron: string;
	agent_id: string;
	title: string;
	description: string | null;
	enabled: boolean;
	last_run: string | null;
	run_count: number;
	created_at: string;
}

export const schedules = {
	list: (agentId?: string) => {
		const params = agentId ? `?agent_id=${agentId}` : '';
		return request<Schedule[]>(`/api/schedules${params}`);
	},
	get: (id: string) => request<Schedule>(`/api/schedules/${id}`),
	create: (data: {
		name: string;
		cron: string;
		agent_id: string;
		title: string;
		description?: string;
	}) => request<Schedule>('/api/schedules', { method: 'POST', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/schedules/${id}`, { method: 'DELETE' }),
	enable: (id: string) => request<Schedule>(`/api/schedules/${id}/enable`, { method: 'POST' }),
	disable: (id: string) => request<Schedule>(`/api/schedules/${id}/disable`, { method: 'POST' }),
	trigger: (id: string) => request<Task>(`/api/schedules/${id}/trigger`, { method: 'POST' })
};

// -- Procedures (SOPs) --

export interface Sop {
	id: string;
	name: string;
	description: string | null;
	content: string;
	triggers: string | null;
	created_at: string;
	updated_at: string;
	created_by: string | null;
	version: number;
	parsed: {
		summary?: string;
		tools?: string[];
		inputs?: { name: string; description: string; required: boolean; default?: string }[];
		outputs?: { name: string; description: string }[];
		steps?: { name: string; description: string; tools?: string[]; optional: boolean }[];
	} | null;
}

export const procedures = {
	list: () => request<Sop[]>('/api/procedures'),
	get: (name: string) => request<Sop>(`/api/procedures/${name}`),
	create: (data: { name: string; description?: string; content: string }) =>
		request<Sop>('/api/procedures', { method: 'POST', body: JSON.stringify(data) }),
	update: (name: string, data: { description?: string; content?: string }) =>
		request<Sop>(`/api/procedures/${name}`, { method: 'PUT', body: JSON.stringify(data) }),
	delete: (name: string) => request<void>(`/api/procedures/${name}`, { method: 'DELETE' }),
	run: (name: string, data: { agent_id: string; inputs?: Record<string, string> }) =>
		request<Task>(`/api/procedures/${name}/run`, { method: 'POST', body: JSON.stringify(data) })
};

// -- Budget --

export interface BudgetSummary {
	global: {
		daily_limit: number | null;
		monthly_limit: number | null;
		daily_spent: number;
		monthly_spent: number;
		total_spent: number;
	};
	agents: {
		agent_id: string;
		daily_spent: number;
		monthly_spent: number;
		total_spent: number;
		is_paused: boolean;
	}[];
}

export interface UsageRecord {
	id: number;
	agent_id: string;
	timestamp: string;
	model: string;
	input_tokens: number;
	output_tokens: number;
	cost_usd: number;
	operation: string | null;
}

export const budget = {
	summary: () => request<BudgetSummary>('/api/budget'),
	usage: (agentId?: string, limit?: number) => {
		const params = new URLSearchParams();
		if (agentId) params.set('agent_id', agentId);
		if (limit) params.set('limit', String(limit));
		const qs = params.toString();
		return request<UsageRecord[]>(`/api/budget/usage${qs ? `?${qs}` : ''}`);
	},
	resume: (agentId: string) =>
		request<{ agent_id: string; is_paused: boolean; resumed: boolean }>(
			`/api/budget/${agentId}/resume`, { method: 'POST' }
		)
};

// -- Activity --

export interface ActivityEvent {
	id: number;
	timestamp: string;
	agent_id: string | null;
	event_type: string;
	event_data: unknown;
}

export const activity = {
	list: (limit = 50) => request<ActivityEvent[]>(`/api/activity?limit=${limit}`)
};

// -- Health --

export const health = {
	check: () => request<{ status: string; version: string; git_hash: string }>('/api/health')
};

// -- Setup --

export interface SetupStatus {
	setup_complete: boolean;
}

export interface DockerStatus {
	available: boolean;
	error: string | null;
}

export interface SystemInfo {
	total_memory_gb: number;
	available_memory_gb: number;
	cpu_count: number;
	gpu: {
		available: boolean;
		name: string | null;
		vram_gb: number | null;
	};
	os: string;
	arch: string;
}

export interface OllamaInfo {
	available: boolean;
	models: { name: string; size: number | null }[];
	error: string | null;
}

export interface ModelOption {
	model: string;
	display_name: string;
	ram_required_gb: number;
	suitable: boolean;
}

export interface ModelRecommendation {
	model: string;
	embedding_model: string;
	reason: string;
	all_options: ModelOption[];
}

export interface AgentPreset {
	id: string;
	name: string;
	description: string;
	icon: string;
	role: string;
	backend: string;
	default_tools: string[];
	default_mcp_servers: Record<string, { type: string; command?: string; args?: string[]; env?: Record<string, string>; url?: string }>;
	recommended_llm: string;
}

export interface LiveConfig {
	llm: {
		default_provider: string;
		has_openai_key: boolean;
		openai_base_url: string | null;
		has_anthropic_key: boolean;
		local_model: string | null;
	};
	agents: {
		name: string;
		backend: string;
		display_name?: string | null;
		role_title?: string | null;
		responsibilities?: string | null;
		avatar?: string | null;
		role: string;
		model: string | null;
		llm?: { provider: string | null; api_key: string | null; base_url: string | null };
		tools: string[];
		skills: string[];
		volumes: string[];
		budget?: { daily: string | null; monthly: string | null; per_task: string | null; on_exceeded: string; fallback_model: string; warn_at_percent: number };
		rate_limit?: { requests_per_minute: number; tokens_per_minute: number; concurrent_requests: number };
		wake_on?: { schedule: string | null; event: string | null; condition: string | null }[];
		hooks?: { before_message: string[]; after_message: string[] };
		idle_prompt?: string | null;
	}[];
	system: { budget: { daily: string; monthly: string | null; on_exceeded: string } };
	mcp_servers: string[];
}

export interface DownloadStatus {
	status: 'Idle' | 'Downloading' | 'Complete' | 'Error';
	filename: string;
	downloaded_bytes: number;
	total_bytes: number;
	error: string | null;
}

export const setup = {
	status: () => request<SetupStatus>('/api/setup/status'),
	getConfig: () => request<LiveConfig>('/api/setup/config'),
	checkDocker: () => request<DockerStatus>('/api/setup/check-docker'),
	systemInfo: () => request<SystemInfo>('/api/setup/system-info'),
	checkOllama: () => request<OllamaInfo>('/api/setup/check-ollama'),
	recommendModel: () => request<ModelRecommendation>('/api/setup/recommend-model'),
	validateKey: (provider: string, apiKey: string, baseUrl?: string) =>
		request<{ valid: boolean; error?: string; models?: { id: string }[] }>('/api/setup/validate-key', {
			method: 'POST',
			body: JSON.stringify({ provider, api_key: apiKey, base_url: baseUrl })
		}),
	presets: () => request<AgentPreset[]>('/api/setup/presets'),
	complete: (data: {
		llm: { provider: string; api_key?: string; base_url?: string; local_model?: string; local_base_url?: string; use_embedded?: boolean };
		agents: { name: string; preset?: string; role?: string; role_title?: string; responsibilities?: string; model?: string; tools?: string[]; volumes?: string[] }[];
		mcp_servers?: Record<string, unknown>;
		isolation?: string;
	}) =>
		request<{ success: boolean; downloading: boolean; config_path: string }>('/api/setup/complete', {
			method: 'POST',
			body: JSON.stringify(data)
		}),
	downloadStatus: () => request<DownloadStatus>('/api/setup/download-status'),
	addAgent: (data: {
		name: string; preset?: string; role?: string; model?: string;
		backend?: string; tools?: string[]; volumes?: string[];
		mcp_servers?: Record<string, unknown>;
	}) => request<{ success: boolean; agent: string }>('/api/setup/add-agent', {
		method: 'POST',
		body: JSON.stringify(data)
	})
};

export interface UserProfile {
	name: string;
	avatar: string | null;
}

export const settings = {
	getProfile: () => request<UserProfile>('/api/settings/profile'),
	putProfile: (profile: UserProfile) =>
		request<UserProfile>('/api/settings/profile', {
			method: 'PUT',
			body: JSON.stringify(profile)
		})
};

export interface App {
	id: string;
	title: string;
	icon: string | null;
	description: string | null;
	agent_id: string;
	conversation_id: string | null;
	container_id: string | null;
	port: number;
	source_version: number;
	status: string;
	created_at: string;
	updated_at: string;
}

export const apps = {
	list: () => request<App[]>('/api/apps'),
	get: (id: string) => request<App>(`/api/apps/${id}`),
	create: (data: { id: string; title: string; icon?: string; description?: string; agent_id: string; port?: number }) =>
		request<App>('/api/apps', { method: 'POST', body: JSON.stringify(data) }),
	delete: (id: string) => request<{ deleted: boolean }>(`/api/apps/${id}`, { method: 'DELETE' }),
};

// -- Connectors --

export interface Connector {
	id: string;
	name: string;
	connector_type: string;
	config: Record<string, unknown>;
	enabled: boolean;
	status: string;
	error_message: string | null;
	created_at: string;
	updated_at: string;
}

export interface Channel {
	id: string;
	connector_id: string;
	name: string;
	channel_type: string;
	config: Record<string, unknown>;
	agent_id: string | null;
	enabled: boolean;
	created_at: string;
}

export const connectors = {
	list: () => request<Connector[]>('/api/connectors'),
	create: (data: { name: string; connector_type: string; config: Record<string, unknown> }) =>
		request<Connector>('/api/connectors', { method: 'POST', body: JSON.stringify(data) }),
	get: (id: string) => request<Connector>(`/api/connectors/${id}`),
	update: (id: string, data: Partial<{ name: string; config: Record<string, unknown>; enabled: boolean }>) =>
		request<Connector>(`/api/connectors/${id}`, { method: 'PATCH', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/connectors/${id}`, { method: 'DELETE' }),
	test: (id: string) => request<{ ok: boolean; error?: string }>(`/api/connectors/${id}/test`, { method: 'POST' }),
	channels: (id: string) => request<Channel[]>(`/api/connectors/${id}/channels`),
	createChannel: (connectorId: string, data: { name: string; channel_type?: string; config?: Record<string, unknown>; agent_id?: string }) =>
		request<Channel>(`/api/connectors/${connectorId}/channels`, { method: 'POST', body: JSON.stringify(data) }),
	updateChannel: (connectorId: string, channelId: string, data: Partial<{ agent_id: string | null; config: Record<string, unknown> }>) =>
		request<Channel>(`/api/connectors/${connectorId}/channels/${channelId}`, { method: 'PATCH', body: JSON.stringify(data) }),
	deleteChannel: (connectorId: string, channelId: string) =>
		request<void>(`/api/connectors/${connectorId}/channels/${channelId}`, { method: 'DELETE' }),
};

// -- Workflows --

export interface Workflow {
	id: string;
	name: string;
	description: string | null;
	yaml_content: string;
	enabled: boolean;
	version: number;
	created_at: string;
	updated_at: string;
}

export interface WorkflowInstance {
	id: string;
	workflow_id: string;
	status: string;
	current_flow: string;
	current_step_index: number;
	trigger_data: string | null;
	variable_store: string;
	loop_state: string | null;
	started_at: string;
	completed_at: string | null;
	error_message: string | null;
	step_executions?: StepExecution[];
}

export interface StepExecution {
	id: string;
	instance_id: string;
	flow_name: string;
	step_id: string;
	task_id: string | null;
	status: string;
	input_context: string | null;
	output: string | null;
	attempt: number;
	started_at: string | null;
	completed_at: string | null;
}

export const workflows = {
	list: () => request<Workflow[]>('/api/workflows'),
	create: (data: { name: string; description?: string; yaml_content: string }) =>
		request<Workflow>('/api/workflows', { method: 'POST', body: JSON.stringify(data) }),
	get: (id: string) => request<Workflow>(`/api/workflows/${id}`),
	update: (id: string, data: { name: string; yaml_content: string; description?: string }) =>
		request<Workflow>(`/api/workflows/${id}`, { method: 'PUT', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/workflows/${id}`, { method: 'DELETE' }),
	enable: (id: string) => request<Workflow>(`/api/workflows/${id}/enable`, { method: 'POST' }),
	disable: (id: string) => request<Workflow>(`/api/workflows/${id}/disable`, { method: 'POST' }),
	run: (id: string, triggerData?: Record<string, unknown>) =>
		request<WorkflowInstance>(`/api/workflows/${id}/run`, { method: 'POST', body: JSON.stringify(triggerData || {}) }),
	instances: (id: string) => request<WorkflowInstance[]>(`/api/workflows/${id}/instances`),
	getInstance: (instanceId: string) => request<WorkflowInstance>(`/api/workflows/instances/${instanceId}`),
	cancelInstance: (instanceId: string) => request<void>(`/api/workflows/instances/${instanceId}/cancel`, { method: 'POST' }),
};
