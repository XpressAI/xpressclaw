/**
 * XpressClaw API timestamps are UTC. SQLite emits `YYYY-MM-DD HH:mm:ss`
 * without a zone, while some API producers emit qualified ISO/RFC 3339.
 * Normalize only the zone-less SQLite form so qualified values are never
 * double-suffixed or shifted through the browser's local timezone.
 */
export function serverTimestampMs(value: string | number | null | undefined): number | null {
	if (typeof value === 'number') return Number.isFinite(value) ? value : null;
	if (typeof value !== 'string' || !value.trim()) return null;

	const timestamp = value.trim();
	const qualified = /(?:z|[+-]\d{2}(?::?\d{2})?)$/i.test(timestamp);
	const sqliteUtc = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?$/;
	const normalized = !qualified && sqliteUtc.test(timestamp)
		? `${timestamp.replace(' ', 'T')}Z`
		: timestamp;
	const parsed = Date.parse(normalized);
	return Number.isNaN(parsed) ? null : parsed;
}

export function elapsedTimeLabel(milliseconds: number): string {
	const seconds = Math.max(0, milliseconds) / 1_000;
	if (seconds < 60) return `${seconds.toFixed(1)}s`;
	const minutes = Math.floor(seconds / 60);
	return `${minutes}m ${Math.floor(seconds % 60)}s`;
}
