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
	if (res.status === 204) return undefined as T;
	return res.json();
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
	container_id: string | null;
	config: string;
	created_at: string;
	started_at: string | null;
	stopped_at: string | null;
	error_message: string | null;
}

export const agents = {
	list: () => request<Agent[]>('/api/agents'),
	get: (id: string) => request<Agent>(`/api/agents/${id}`),
	start: (id: string) => request<Agent>(`/api/agents/${id}/start`, { method: 'POST' }),
	stop: (id: string) => request<Agent>(`/api/agents/${id}/stop`, { method: 'POST' }),
	delete: (id: string) => request<void>(`/api/agents/${id}`, { method: 'DELETE' })
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
	updateStatus: (id: string, status: string) =>
		request<Task>(`/api/tasks/${id}/status`, {
			method: 'PATCH',
			body: JSON.stringify({ status })
		}),
	delete: (id: string) => request<void>(`/api/tasks/${id}`, { method: 'DELETE' })
};

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
	list: (limit = 50) => request<MemorySearchResult[]>(`/api/memory?limit=${limit}`),
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
	list: () => request<Schedule[]>('/api/schedules'),
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
	}
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
	check: () => request<{ status: string; version: string }>('/api/health')
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
}

export interface LiveConfig {
	llm: {
		default_provider: string;
		has_openai_key: boolean;
		openai_base_url: string | null;
		has_anthropic_key: boolean;
		local_model: string | null;
	};
	agents: { name: string; backend: string; role: string; model: string | null; tools: string[] }[];
	system: { budget: { daily: string; monthly: string | null; on_exceeded: string } };
	mcp_servers: string[];
}

export const setup = {
	status: () => request<SetupStatus>('/api/setup/status'),
	getConfig: () => request<LiveConfig>('/api/setup/config'),
	checkDocker: () => request<DockerStatus>('/api/setup/check-docker'),
	systemInfo: () => request<SystemInfo>('/api/setup/system-info'),
	checkOllama: () => request<OllamaInfo>('/api/setup/check-ollama'),
	recommendModel: () => request<ModelRecommendation>('/api/setup/recommend-model'),
	validateKey: (provider: string, apiKey: string, baseUrl?: string) =>
		request<{ valid: boolean; error?: string }>('/api/setup/validate-key', {
			method: 'POST',
			body: JSON.stringify({ provider, api_key: apiKey, base_url: baseUrl })
		}),
	presets: () => request<AgentPreset[]>('/api/setup/presets'),
	complete: (data: {
		llm: { provider: string; api_key?: string; base_url?: string; local_model?: string };
		agents: { name: string; preset?: string; role?: string; tools?: string[] }[];
		mcp_servers?: Record<string, unknown>;
	}) =>
		request<{ success: boolean; config_path: string }>('/api/setup/complete', {
			method: 'POST',
			body: JSON.stringify(data)
		})
};
