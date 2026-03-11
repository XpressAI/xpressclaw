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
