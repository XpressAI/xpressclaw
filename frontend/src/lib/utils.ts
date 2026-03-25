import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

export function timeAgo(dateStr: string): string {
	const date = new Date(dateStr + 'Z');
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

/** Open a URL in the system browser via the server, with browser fallback. */
export async function openExternal(url: string): Promise<void> {
	try {
		await fetch('/api/open-url', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ url })
		});
	} catch {
		window.open(url, '_blank');
	}
}

const AVATAR_COUNT = 32;

/** Get a deterministic avatar path for an agent based on its name. */
export function agentAvatar(agent: { name: string; id: string }): string {
	// Simple hash of the agent name to pick a consistent avatar
	let hash = 0;
	const key = agent.name || agent.id;
	for (let i = 0; i < key.length; i++) {
		hash = ((hash << 5) - hash + key.charCodeAt(i)) | 0;
	}
	const idx = ((hash % AVATAR_COUNT) + AVATAR_COUNT) % AVATAR_COUNT;
	return `/avatars/${idx.toString().padStart(2, '0')}.jpg`;
}

export function statusColor(status: string): string {
	switch (status) {
		case 'running':
		case 'completed':
			return 'text-emerald-400';
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
			return 'text-muted-foreground';
		default:
			return 'text-muted-foreground';
	}
}
