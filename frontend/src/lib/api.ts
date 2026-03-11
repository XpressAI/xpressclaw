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
