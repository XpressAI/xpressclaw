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

// -- Agents --

export interface Agent {
	id: string;
	name: string;
	title: string;
	backend: string;
	project_id: string | null;
	status: string;
	desired_status: string;
	observed_status: string;
	container_id: string | null;
	config?: {
		model?: string | null;
		runner?: NativeRunnerConfig;
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
		model?: string;
		llm?: { provider: string | null; api_key: string | null; base_url: string | null };
		runner?: NativeRunnerConfig;
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

// -- Projects and conversations --

export interface Project {
	id: string;
	name: string;
	description: string | null;
	icon: string | null;
	created_at: string;
	updated_at: string;
	agent_ids: string[];
	conversation_count: number;
	task_count: number;
}

export interface ConversationParticipant {
	participant_type: 'user' | 'agent';
	participant_id: string;
	joined_at: string;
}

export interface ConversationAttachment {
	id: string;
	message_id: number;
	name: string;
	mime_type: string;
	size: number;
	source_task_id: string | null;
	created_at: string;
}

export interface ConversationMessage {
	id: number;
	conversation_id: string;
	sender_type: 'user' | 'agent' | 'system';
	sender_id: string;
	sender_name: string | null;
	content: string;
	message_type: string;
	linked_task_id: string | null;
	metadata: Record<string, unknown>;
	attachments?: ConversationAttachment[];
	created_at: string;
}

export interface Conversation {
	id: string;
	project_id: string | null;
	title: string | null;
	icon: string | null;
	created_at: string;
	updated_at: string;
	last_message_at: string | null;
	participants: ConversationParticipant[];
}

export interface ConversationTurn {
	id: string;
	conversation_id: string;
	agent_id: string;
	status: string;
	error_message: string | null;
	context_used: number | null;
	context_size: number | null;
	queued_at: string;
	started_at: string | null;
	completed_at: string | null;
}

export interface ConversationMessageUpload {
	name: string;
	mime_type: string;
	data: string;
}

export const projects = {
	list: () => request<Project[]>('/api/projects'),
	get: (id: string) => request<Project>(`/api/projects/${encodeURIComponent(id)}`),
	create: (data: { name: string; description?: string; icon?: string }) =>
		request<Project>('/api/projects', { method: 'POST', body: JSON.stringify(data) }),
	update: (id: string, data: { name?: string; description?: string; icon?: string }) =>
		request<Project>(`/api/projects/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify(data) }),
	assignAgent: (id: string, agentId: string) =>
		request<Project>(`/api/projects/${encodeURIComponent(id)}/agents/${encodeURIComponent(agentId)}`, { method: 'PUT', body: '{}' }),
	tasks: (id: string) => request<Task[]>(`/api/projects/${encodeURIComponent(id)}/tasks`),
};

export const conversations = {
	list: (projectId?: string, limit = 100) => {
		const params = new URLSearchParams({ limit: String(limit) });
		if (projectId) params.set('project_id', projectId);
		return request<Conversation[]>(`/api/conversations?${params}`);
	},
	get: (id: string) => request<Conversation>(`/api/conversations/${encodeURIComponent(id)}`),
	create: (data: { project_id: string; title?: string; icon?: string; participant_ids?: string[] }) =>
		request<Conversation>('/api/conversations', { method: 'POST', body: JSON.stringify(data) }),
	update: (id: string, data: { title?: string; icon?: string }) =>
		request<Conversation>(`/api/conversations/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/conversations/${encodeURIComponent(id)}`, { method: 'DELETE' }),
	addParticipant: (id: string, agentId: string) =>
		request<void>(`/api/conversations/${encodeURIComponent(id)}/participants`, { method: 'POST', body: JSON.stringify({ agent_id: agentId }) }),
	removeParticipant: (id: string, agentId: string) =>
		request<void>(`/api/conversations/${encodeURIComponent(id)}/participants/${encodeURIComponent(agentId)}`, { method: 'DELETE' }),
	messages: (id: string, limit = 100, beforeId?: number) => {
		const params = new URLSearchParams({ limit: String(limit) });
		if (beforeId !== undefined) params.set('before_id', String(beforeId));
		return request<ConversationMessage[]>(`/api/conversations/${encodeURIComponent(id)}/messages?${params}`);
	},
	sendMessage: (id: string, content: string, attachments: ConversationMessageUpload[] = []) =>
		request<{ message: ConversationMessage; queued_agents: string[] }>(`/api/conversations/${encodeURIComponent(id)}/messages`, {
			method: 'POST',
			body: JSON.stringify({ content, attachments }),
		}),
	tasks: (id: string) => request<Task[]>(`/api/conversations/${encodeURIComponent(id)}/tasks`),
	createTask: (id: string, data: { title: string; description?: string; agent_id?: string; workflow_id?: string; workflow_inputs?: Record<string, unknown>; priority?: number }) =>
		request<Task | { workflow_instance_id: string }>(`/api/conversations/${encodeURIComponent(id)}/tasks`, {
			method: 'POST',
			body: JSON.stringify(data),
		}),
	turns: (id: string) => request<ConversationTurn[]>(`/api/conversations/${encodeURIComponent(id)}/turns`),
	attachmentUrl: (conversationId: string, attachmentId: string) => `/api/conversations/${encodeURIComponent(conversationId)}/attachments/${encodeURIComponent(attachmentId)}`,
};

// -- Logical sessions and native work attempts --

export interface NativeRunnerConfig {
	kind: string;
	image: string;
	workspace: string | null;
	project_name: string | null;
	model: string | null;
	session_config: Record<string, string | boolean>;
	mcp_servers: string[];
	environment: Record<string, string>;
	startup_commands: string[];
	command: string[];
	subscription_auth: boolean;
	ssh_agent_forwarding: boolean;
	container_engine: 'none' | 'host';
}

export interface LogicalSession {
	id: string;
	agent_id: string;
	title: string | null;
	status: string;
	latest_summary: string | null;
	created_at: string;
	updated_at: string;
}

export interface WorkAttempt {
	id: string;
	session_id: string;
	task_id: string | null;
	queue_id: number | null;
	kind: string;
	runner: string;
	status: string;
	prompt: string;
	native_session_id: string | null;
	container_id: string | null;
	result: string | null;
	error_message: string | null;
	created_at: string;
	started_at: string | null;
	completed_at: string | null;
	context_used: number | null;
	context_size: number | null;
}

export interface SessionEvent {
	id: number;
	session_id: string;
	attempt_id: string | null;
	task_id: string | null;
	source_type: string;
	source_id: string | null;
	event_type: string;
	summary: string;
	payload: Record<string, unknown>;
	created_at: string;
}

export interface AcpConfigChoice {
	value: string;
	name: string;
	description?: string | null;
}

export interface AcpConfigGroup {
	group: string;
	name: string;
	options: AcpConfigChoice[];
}

export interface AcpConfigOption {
	id: string;
	name: string;
	description?: string | null;
	category?: string | null;
	type: 'select' | 'boolean';
	currentValue: string | boolean;
	options?: AcpConfigChoice[] | AcpConfigGroup[];
}

export interface AcpMode {
	id: string;
	name: string;
	description?: string | null;
}

export interface AcpModeState {
	currentModeId: string;
	availableModes: AcpMode[];
}

export interface AcpCommand {
	name: string;
	description: string;
	input?: { hint: string } | null;
}

export interface AttemptArtifact {
	id: string;
	attempt_id: string;
	session_id: string;
	artifact_type: string;
	title: string;
	content: string | null;
	uri: string | null;
	metadata: Record<string, unknown>;
	created_at: string;
}

export interface SessionOverview {
	session: LogicalSession;
	active_attempts: WorkAttempt[];
	queued_attempts: WorkAttempt[];
	recent_attempts: WorkAttempt[];
	recent_events: SessionEvent[];
	artifacts: AttemptArtifact[];
}

export interface RunnerReadiness {
	protocol: string;
	ready: boolean;
	docker_available: boolean;
	container_runtime: 'docker' | 'podman' | null;
	container_runtime_version: string | null;
	kind: string;
	image: string;
	runtime_image: string | null;
	image_present: boolean;
	workspace: string;
	workspace_present: boolean;
	model: string | null;
	container_engine: 'none' | 'host';
	container_engine_available: boolean;
	container_engine_socket: string | null;
	ssh_agent_forwarding: boolean;
	ssh_agent_available: boolean;
	ssh_agent_socket: string | null;
	command_present: boolean;
	subscription_auth: boolean;
	auth_present: boolean;
	issues: string[];
}

export interface ImageAttachmentUpload {
	name: string;
	mime_type: string;
	data: string;
}

export const sessions = {
	get: (id: string) => request<SessionOverview>(`/api/sessions/${id}`),
	readiness: (id: string) => request<RunnerReadiness>(`/api/sessions/${id}/readiness`),
	prepare: (id: string) => request<RunnerReadiness>(`/api/sessions/${id}/readiness`, {
		method: 'POST',
		body: '{}'
	}),
	events: (id: string, after?: number) => {
		const query = after ? `?after=${after}` : '';
		return request<SessionEvent[]>(`/api/sessions/${id}/events${query}`);
	},
	attempts: (id: string, status?: string) => {
		const query = status ? `?status=${encodeURIComponent(status)}` : '';
		return request<WorkAttempt[]>(`/api/sessions/${id}/attempts${query}`);
	},
	sendMessage: (id: string, content: string, options?: { priority?: number; newSession?: boolean; configOptions?: Record<string, string | boolean>; attachments?: ImageAttachmentUpload[] }) =>
		request<{ event: SessionEvent; task: Task; attempt_id: string; queued: boolean }>(
			`/api/sessions/${id}/messages`,
			{ method: 'POST', body: JSON.stringify({
				content,
				priority: options?.priority,
				new_session: options?.newSession ?? false,
				config_options: options?.configOptions ?? {},
				attachments: options?.attachments ?? []
			}) }
		),
	cancelAttempt: (sessionId: string, attemptId: string) =>
		request<WorkAttempt>(`/api/sessions/${sessionId}/attempts/${attemptId}/cancel`, {
			method: 'POST',
			body: '{}'
		}),
	interruptAttempt: (sessionId: string, attemptId: string) =>
		request<WorkAttempt>(`/api/sessions/${sessionId}/attempts/${attemptId}/interrupt`, {
			method: 'POST',
			body: '{}'
		})
};

// -- Project workspaces --

export interface WorkspaceStatus {
	agent_id: string;
	root: string;
	container_exists: boolean;
	container_running: boolean;
	terminal_available: boolean;
}

export interface WorkspaceEntry {
	name: string;
	path: string;
	kind: 'directory' | 'file' | 'symlink' | 'other';
	symlink: boolean;
	size: number | null;
	modified_at: string | null;
}

export interface WorkspaceDirectory {
	path: string;
	entries: WorkspaceEntry[];
	truncated: boolean;
}

export interface WorkspaceFile {
	path: string;
	content: string;
	revision: string;
	size: number;
}

export interface GitChange {
	path: string;
	original_path: string | null;
	status: string;
	index_status: string;
	worktree_status: string;
}

export interface WorkspaceGitStatus {
	repository: boolean;
	branch: string | null;
	files: GitChange[];
}

export interface WorkspaceGitDiff {
	path: string;
	diff: string;
	truncated: boolean;
}

export const workspaces = {
	status: (agentId: string) =>
		request<WorkspaceStatus>(`/api/workspaces/${encodeURIComponent(agentId)}`),
	tree: (agentId: string, path = '') =>
		request<WorkspaceDirectory>(`/api/workspaces/${encodeURIComponent(agentId)}/tree?path=${encodeURIComponent(path)}`),
	readFile: (agentId: string, path: string) =>
		request<WorkspaceFile>(`/api/workspaces/${encodeURIComponent(agentId)}/file?path=${encodeURIComponent(path)}`),
	saveFile: (agentId: string, file: WorkspaceFile) =>
		request<{ path: string; revision: string; size: number }>(
			`/api/workspaces/${encodeURIComponent(agentId)}/file`,
			{
				method: 'PUT',
				body: JSON.stringify({
					path: file.path,
					content: file.content,
					expected_revision: file.revision,
				}),
			}
		),
	gitStatus: (agentId: string) =>
		request<WorkspaceGitStatus>(`/api/workspaces/${encodeURIComponent(agentId)}/git/status`),
	gitDiff: (agentId: string, path: string) =>
		request<WorkspaceGitDiff>(`/api/workspaces/${encodeURIComponent(agentId)}/git/diff?path=${encodeURIComponent(path)}`),
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
	conversation_id: string | null;
	project_id?: string | null;
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

export interface TaskActivity {
	attempts: WorkAttempt[];
	events: SessionEvent[];
	has_more_before: boolean;
	has_more_after: boolean;
}

export interface TaskListOptions {
	limit?: number;
	offset?: number;
	statuses?: string[];
	sort?: 'recent';
	excludeStatuses?: string[];
	search?: string;
}

export const tasks = {
	list: (status?: string, agentId?: string, options: TaskListOptions = {}) => {
		const params = new URLSearchParams();
		if (status) params.set('status', status);
		if (agentId) params.set('agent_id', agentId);
		if (options.limit !== undefined) params.set('limit', String(options.limit));
		if (options.offset !== undefined) params.set('offset', String(options.offset));
		if (options.statuses?.length) params.set('statuses', options.statuses.join(','));
		if (options.sort) params.set('sort', options.sort);
		if (options.excludeStatuses?.length) params.set('exclude_statuses', options.excludeStatuses.join(','));
		if (options.search) params.set('search', options.search);
		const qs = params.toString();
		return request<{ tasks: Task[]; counts: TaskCounts }>(`/api/tasks${qs ? `?${qs}` : ''}`);
	},
	recentByAgent: (limit = 5) =>
		request<{ tasks: Task[] }>(`/api/tasks/recent-by-agent?limit=${encodeURIComponent(limit)}`),
	get: (id: string) => request<Task>(`/api/tasks/${id}`),
	create: (data: { title: string; description?: string; agent_id?: string; priority?: number; context?: Record<string, unknown> }) =>
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
	activity: (id: string, options: { after?: number; before?: number; limit?: number } = {}) => {
		const params = new URLSearchParams();
		if (options.after !== undefined) params.set('after', String(options.after));
		if (options.before !== undefined) params.set('before', String(options.before));
		if (options.limit !== undefined) params.set('limit', String(options.limit));
		const query = params.size ? `?${params}` : '';
		return request<TaskActivity>(`/api/tasks/${id}/activity${query}`);
	},
	addMessage: (id: string, role: string, content: string, options?: { configOptions?: Record<string, string | boolean>; attachments?: ImageAttachmentUpload[]; delivery?: 'after_tool' | 'immediate' }) =>
		request<TaskMessageResponse>(`/api/tasks/${id}/messages`, {
			method: 'POST',
			body: JSON.stringify({
				role,
				content,
				config_options: options?.configOptions ?? {},
				attachments: options?.attachments ?? [],
				delivery: options?.delivery ?? 'after_tool'
			})
		}),
	respondToElicitation: (id: string, elicitationId: string, data: {
		action: 'accept' | 'decline' | 'cancel';
		content?: Record<string, unknown>;
		message?: string;
	}) => request<{ resolved: boolean; action: string }>(
		`/api/tasks/${id}/elicitations/${encodeURIComponent(elicitationId)}/response`,
		{ method: 'POST', body: JSON.stringify(data) }
	),
	subtasks: (id: string) => request<{ tasks: Task[]; counts: TaskCounts }>(`/api/tasks?parent_task_id=${id}`),
	createBatch: (data: { tasks: { ref: string; title: string; description?: string; agent_id?: string; priority?: number; new_session?: boolean; depends_on?: string[] }[]; parent_task_id?: string }) =>
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
	attachments: TaskMessageAttachment[];
}

export interface TaskMessageAttachment {
	id: string;
	name: string;
	mime_type: string;
	size: number;
}

export interface TaskMessageResponse {
	message: TaskMessage;
	continuation_queued: boolean;
	attempt_id: string | null;
	delivery: 'stored' | 'queued' | 'after_tool' | 'immediate';
}

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
	schedule_type: 'cron' | 'once';
	run_at: string | null;
	continuation_task_id: string | null;
	conversation_id: string | null;
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
	createOnce: (data: {
		name: string;
		agent_id: string;
		title: string;
		description?: string;
		run_at?: string;
		delay_seconds?: number;
		continuation_task_id?: string;
		conversation_id?: string;
	}) => request<Schedule>('/api/schedules/once', { method: 'POST', body: JSON.stringify(data) }),
	delete: (id: string) => request<void>(`/api/schedules/${id}`, { method: 'DELETE' }),
	enable: (id: string) => request<Schedule>(`/api/schedules/${id}/enable`, { method: 'POST' }),
	disable: (id: string) => request<Schedule>(`/api/schedules/${id}/disable`, { method: 'POST' }),
	trigger: (id: string) => request<Task | { conversation_id: string; agent_id: string; message: ConversationMessage }>(`/api/schedules/${id}/trigger`, { method: 'POST' })
};

// -- Health --

export const health = {
	check: () =>
		request<{ status: string; version: string; build: string; git_hash: string }>('/api/health')
};

// -- Setup --

export interface SetupStatus {
	setup_complete: boolean;
}

export interface DockerStatus {
	available: boolean;
	installed: boolean;
	can_start: boolean;
	runtime: 'docker' | 'podman' | null;
	version: string | null;
	socket: string | null;
	rootless: boolean | null;
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
	working_directory: string | null;
	ssh_agent_available: boolean;
	ssh_agent_socket: string | null;
}

export interface AcpAgentCatalogEntry {
	kind: string;
	name: string;
	mark: string;
	description: string;
	command: string[];
	login_command: string;
	install_url: string;
	image: string;
	host_image: string;
	installed: boolean;
	configured: boolean;
	status: 'ready' | 'sign_in' | 'not_installed';
	executable: string | null;
}

export interface DirectoryListing {
	path: string;
	parent: string | null;
	home: string | null;
	roots: string[];
	directories: { name: string; path: string }[];
}

export interface ProjectEnvironmentSuggestion {
	id: string;
	name: string;
	description: string;
	detected_file: string;
	command: string | null;
	requires_host_engine: boolean;
}

export interface ProjectEnvironment {
	workspace: string;
	detected_files: string[];
	suggestions: ProjectEnvironmentSuggestion[];
	git_repository: boolean;
	git_uses_ssh: boolean;
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

/// Per-agent provider summary (no api_key — it's masked).
export interface AgentProviderEntry {
	agent: string;
	provider: string | null;
	model: string | null;
	base_url: string | null;
	has_api_key: boolean;
}

/// Legacy per-session LLM config retained for old configurations.
export interface AgentLlmConfig {
	provider: string | null;
	model: string | null;
	api_key: string | null;
	base_url: string | null;
}

export interface LiveConfig {
	llm: {
		// Per-agent provider summary. There is no global LLM config — each
		// agent declares its own provider/model/key/base_url.
		providers: AgentProviderEntry[];
	};
	agents: {
		name: string;
		title: string;
		backend: string;
		model: string | null;
		llm?: AgentLlmConfig;
		runner: NativeRunnerConfig;
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
	mcp_servers: McpServerDefinition[];
}

export interface McpServerDefinition {
	name: string;
	type: 'stdio' | 'http' | 'sse' | string;
	command?: string | null;
	args: string[];
	url?: string | null;
	env: Record<string, string> | string[];
	headers?: Record<string, string>;
}

export interface McpVerificationResult {
	ok: boolean;
	status: 'ready'
		| 'authentication_required'
		| 'command_path_incorrect'
		| 'command_missing'
		| 'project_required'
		| string;
	message: string;
	suggestion: string | null;
}

export const mcpServers = {
	list: () => request<{ servers: McpServerDefinition[] }>('/api/setup/mcp-servers'),
	upsert: (server: McpServerDefinition) =>
		request<{ success: boolean; name: string }>('/api/setup/mcp-servers', {
			method: 'POST',
			body: JSON.stringify(server)
		}),
	delete: (name: string) =>
		request<{ success: boolean; deleted: string }>(`/api/setup/mcp-servers/${encodeURIComponent(name)}`, {
			method: 'DELETE'
		}),
	verify: (name: string, agentId?: string) =>
		request<McpVerificationResult>(`/api/setup/mcp-servers/${encodeURIComponent(name)}/verify`, {
			method: 'POST',
			body: JSON.stringify({ agent_id: agentId ?? null })
		})
};

export const setup = {
	status: () => request<SetupStatus>('/api/setup/status'),
	getConfig: () => request<LiveConfig>('/api/setup/config'),
	checkDocker: () => request<DockerStatus>('/api/setup/check-docker'),
	systemInfo: () => request<SystemInfo>('/api/setup/system-info'),
	agentCatalog: () => request<{ agents: AcpAgentCatalogEntry[] }>('/api/setup/agent-catalog'),
	directories: (path?: string) => {
		const query = path ? `?path=${encodeURIComponent(path)}` : '';
		return request<DirectoryListing>(`/api/setup/directories${query}`);
	},
	projectEnvironment: (path: string) =>
		request<ProjectEnvironment>(`/api/setup/project-environment?path=${encodeURIComponent(path)}`),
	checkOllama: () => request<OllamaInfo>('/api/setup/check-ollama'),
	recommendModel: () => request<ModelRecommendation>('/api/setup/recommend-model'),
	validateKey: (provider: string, apiKey: string, baseUrl?: string) =>
		request<{ valid: boolean; error?: string; models?: { id: string }[] }>('/api/setup/validate-key', {
			method: 'POST',
			body: JSON.stringify({ provider, api_key: apiKey, base_url: baseUrl })
		}),
	complete: (data: {
		agents: { backend?: string; runner_kind?: string; runner_image?: string; runner_workspace?: string; workspace_mode?: 'existing' | 'managed'; project_name?: string; runner_model?: string; runner_command?: string[]; startup_commands?: string[]; subscription_auth?: boolean; ssh_agent_forwarding?: boolean; runner_container_engine?: 'none' | 'host'; volumes?: string[] }[];
		mcp_servers?: Record<string, unknown>;
		isolation?: string;
	}) =>
		request<{ success: boolean; config_path: string }>('/api/setup/complete', {
			method: 'POST',
			body: JSON.stringify(data)
		}),
	addSession: (data: {
		project_id?: string; backend?: string; runner_kind?: string; runner_image?: string; runner_workspace?: string; workspace_mode?: 'existing' | 'managed'; project_name?: string; runner_model?: string; runner_command?: string[]; startup_commands?: string[]; subscription_auth?: boolean; ssh_agent_forwarding?: boolean; runner_container_engine?: 'none' | 'host'; volumes?: string[];
	}) => request<{ success: boolean; session: string; session_id: string; title: string; project_id: string | null }>('/api/setup/add-session', {
		method: 'POST',
		body: JSON.stringify(data)
	})
};

export interface UserProfile {
	name: string;
	avatar: string | null;
}

export interface ProjectSyncStatus {
	project_id: string;
	project_name: string;
	project_icon: string | null;
	status: 'ready' | 'unconfigured' | 'unavailable' | 'error';
	project_dir: string | null;
	remote: string | null;
	branch: string | null;
	store_path: string | null;
	share_project_memory: boolean | null;
	last_commit: string | null;
	last_synced_at: string | null;
	message: string | null;
}

export interface ProjectSyncCounts {
	agents: number;
	tasks: number;
	task_messages: number;
	conversations: number;
	conversation_messages: number;
	workflows: number;
	memory_notes: number;
}

export interface ProjectSyncAction {
	action: 'fetch' | 'publish';
	project_id: string;
	commit: string;
	counts: ProjectSyncCounts;
}

export const settings = {
	getProfile: () => request<UserProfile>('/api/settings/profile'),
	putProfile: (profile: UserProfile) =>
		request<UserProfile>('/api/settings/profile', {
			method: 'PUT',
			body: JSON.stringify(profile)
		}),
	listProjectSync: () => request<{ projects: ProjectSyncStatus[] }>('/api/settings/sync'),
	fetchProject: (projectId: string, force = false) =>
		request<ProjectSyncAction>(`/api/settings/sync/${encodeURIComponent(projectId)}/fetch`, {
			method: 'POST',
			body: JSON.stringify({ force })
		}),
	publishProject: (projectId: string) =>
		request<ProjectSyncAction>(`/api/settings/sync/${encodeURIComponent(projectId)}/publish`, {
			method: 'POST',
			body: '{}'
		})
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
	last_triggered_at: string | null;
	trigger_count: number;
	trigger_error: string | null;
}

export interface WorkflowInstance {
	id: string;
	workflow_id: string;
	project_id?: string | null;
	conversation_id?: string | null;
	current_task_id?: string | null;
	status: string;
	current_flow: string;
	current_step_index: number;
	trigger_data: string | null;
	variable_store: string;
	loop_state: string | null;
	started_at: string;
	completed_at: string | null;
	error_message: string | null;
	wait_event?: string | null;
	wait_resource?: string | null;
	wait_next_poll_at?: string | null;
	wait_error?: string | null;
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

export interface WorkflowInstanceDetails {
	instance: WorkflowInstance;
	step_executions: StepExecution[];
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
	run: (id: string, triggerData?: Record<string, unknown>, projectId?: string) => {
		const params = new URLSearchParams();
		if (projectId) params.set('project_id', projectId);
		const query = params.toString();
		return request<WorkflowInstance>(`/api/workflows/${id}/run${query ? `?${query}` : ''}`, { method: 'POST', body: JSON.stringify(triggerData || {}) });
	},
	instances: (id: string) => request<WorkflowInstance[]>(`/api/workflows/${id}/instances`),
	getInstance: (instanceId: string) => request<WorkflowInstanceDetails>(`/api/workflows/instances/${instanceId}`),
	cancelInstance: (instanceId: string) => request<void>(`/api/workflows/instances/${instanceId}/cancel`, { method: 'POST' }),
};
