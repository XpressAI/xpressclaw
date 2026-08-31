import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { serverTimestampMs } from './serverTime';

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

export function timeAgo(dateStr: string): string {
	const parsed = serverTimestampMs(dateStr);
	if (parsed === null) return '';
	const date = new Date(parsed);
	const now = new Date();
	const seconds = Math.floor((now.getTime() - date.getTime()) / 1000);

	if (seconds < 60) return 'just now';
	if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
	if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
	if (seconds < 604800) return `${Math.floor(seconds / 86400)}d ago`;
	return date.toLocaleDateString();
}

export function formatCost(usd: number): string {
	if (usd < 0.01) return `$${usd.toFixed(4)}`;
	return `$${usd.toFixed(2)}`;
}

/** Open a URL in the user's browser, never on the machine hosting the API. */
export async function openExternal(url: string): Promise<void> {
	const target = new URL(url);
	if (target.protocol !== 'http:' && target.protocol !== 'https:') {
		throw new Error('Only HTTP and HTTPS links can be opened');
	}

	if (isTauri()) {
		await invoke('open_external_url', { url: target.href });
		return;
	}

	window.open(target.href, '_blank', 'noopener,noreferrer');
}

/** Compact product mark for an ACP-backed agent. */
export function harnessMark(backend: string): string {
	const normalized = canonicalHarnessKind(backend);
	if (normalized.includes('claude')) return 'A';
	if (normalized.includes('opencode')) return 'O';
	if (normalized.includes('codex')) return 'C';
	if (normalized === 'deepseek-harness') return 'DS';
	if (normalized.includes('copilot')) return 'GH';
	if (normalized.includes('cursor')) return 'CU';
	if (normalized.includes('cline')) return 'CL';
	if (normalized.includes('glm')) return 'G';
	if (normalized.includes('grok')) return 'X';
	if (normalized.includes('junie')) return 'J';
	if (normalized.includes('kilo')) return 'KI';
	if (normalized.includes('kimi')) return 'K';
	if (normalized.includes('mistral')) return 'M';
	if (normalized.includes('qwen')) return 'Q';
	if (normalized === 'pi' || normalized.includes('pi-acp')) return 'π';
	if (normalized.includes('custom')) return '+';
	return 'R';
}

/** Normalize exact supported aliases to a stable built-in harness kind. */
export function canonicalHarnessKind(backend: string): string {
	const normalized = backend.trim().toLowerCase();
	if (['deepseek', 'dsh', 'dsh-acp', 'deepseek-harness-acp', '@openma/deepseek-harness-acp'].includes(normalized)) return 'deepseek-harness';
	if (normalized === 'copilot') return 'github-copilot';
	if (normalized === 'pi-acp') return 'pi';
	return normalized;
}

/** Infer a built-in kind from a legacy backend label, where fuzzy matching is intentional. */
export function inferHarnessKindFromBackend(backend: string): string {
	const normalized = canonicalHarnessKind(backend);
	if (normalized.includes('deepseek-harness')) return 'deepseek-harness';
	if (normalized.includes('claude')) return 'claude';
	if (normalized.includes('opencode')) return 'opencode';
	if (normalized.includes('codex')) return 'codex';
	if (normalized.includes('copilot')) return 'github-copilot';
	return normalized;
}

/** User-facing product name for an ACP harness identifier. */
export function harnessName(backend: string): string {
	const normalized = canonicalHarnessKind(backend);
	const names: Record<string, string> = {
		claude: 'Claude Agent',
		'claude-code': 'Claude Agent',
		'claude-sdk': 'Claude Agent',
		codex: 'Codex',
		'deepseek-harness': 'DeepSeek Harness',
		'github-copilot': 'GitHub Copilot',
		junie: 'Junie',
		kimi: 'Kimi CLI',
		opencode: 'OpenCode',
		pi: 'pi ACP',
		qwen: 'Qwen Code',
		cursor: 'Cursor',
		cline: 'Cline',
		glm: 'GLM Agent',
		grok: 'Grok Build',
		kilo: 'Kilo Code',
		'mistral-vibe': 'Mistral Vibe',
		custom: 'Custom ACP',
	};
	if (names[normalized]) return names[normalized];
	return normalized
		.split(/[-_\s]+/)
		.filter(Boolean)
		.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
		.join(' ') || 'ACP harness';
}

/** Compact folder label for an agent workspace. */
export function workspaceFolder(workspace: string | null | undefined): string {
	const normalized = workspace?.trim().replace(/[\\/]+$/, '') ?? '';
	if (!normalized) return 'No folder yet';
	return normalized.split(/[\\/]/).filter(Boolean).pop() ?? normalized;
}

interface AgentRuntimeSummaryInput {
	backend: string;
	config?: {
		runner?: {
			kind?: string | null;
			workspace?: string | null;
		};
	};
}

/** Harness and folder metadata shown beneath an Agent name. */
export function agentRuntimeSummary(agent: AgentRuntimeSummaryInput): string {
	const runner = agent.config?.runner;
	return `${harnessName(runner?.kind || agent.backend)} · ${workspaceFolder(runner?.workspace)}`;
}

/** Expanded runtime metadata for hover text. */
export function agentRuntimeTitle(agent: AgentRuntimeSummaryInput): string {
	const runner = agent.config?.runner;
	return `${harnessName(runner?.kind || agent.backend)} · ${runner?.workspace?.trim() || 'No workspace folder yet'}`;
}

/** Get cached user profile (loaded from server, cached in memory). */
let _cachedProfile: { name: string; avatar: string | null } = { name: 'You', avatar: null };
let _profileLoaded = false;

export function getCachedProfile(): { name: string; avatar: string | null } {
	return _cachedProfile;
}

export function setCachedProfile(profile: { name: string; avatar: string | null }) {
	_cachedProfile = profile;
	_profileLoaded = true;
}

export function isProfileLoaded(): boolean {
	return _profileLoaded;
}

export function statusColor(status: string): string {
	switch (status) {
		case 'running':
		case 'completed':
			return 'text-emerald-400';
		case 'starting':
		case 'stopping':
		case 'pending':
		case 'queued':
			return 'text-yellow-400';
		case 'in_progress':
			return 'text-blue-400';
		case 'error':
		case 'cancelled':
			return 'text-red-400';
		case 'stopped':
		case 'blocked':
		case 'docker_unavailable':
		case 'not_found':
		case 'exited':
			return 'text-muted-foreground';
		default:
			return 'text-muted-foreground';
	}
}
