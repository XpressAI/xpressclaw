import {
	usage,
	type DeferredAutomationSummary,
	type UsageAccountReport,
	type UsageSnapshot,
	type UsageWindow,
} from '$lib/api';

const POLL_INTERVAL_MS = 5 * 60_000;
const PREFS_STORAGE_KEY = 'xpressclaw.usage.preferences.v1';

export type UsageDisplayMode = 'used' | 'remaining';

export type UsagePace =
	| { kind: 'unknown' }
	| { kind: 'reached' }
	| { kind: 'runs_out'; at: string }
	| { kind: 'tight' | 'on_track' };

interface UsagePreferences {
	displayMode: UsageDisplayMode;
	pinnedIds: string[];
	hiddenIds: string[];
}

function loadPreferences(): UsagePreferences {
	if (typeof localStorage === 'undefined') {
		return { displayMode: 'used', pinnedIds: [], hiddenIds: [] };
	}
	try {
		const raw = localStorage.getItem(PREFS_STORAGE_KEY);
		if (!raw) return { displayMode: 'used', pinnedIds: [], hiddenIds: [] };
		const parsed = JSON.parse(raw) as Partial<UsagePreferences>;
		return {
			displayMode: parsed.displayMode === 'remaining' ? 'remaining' : 'used',
			pinnedIds: Array.isArray(parsed.pinnedIds)
				? parsed.pinnedIds.filter((v): v is string => typeof v === 'string')
				: [],
			hiddenIds: Array.isArray(parsed.hiddenIds)
				? parsed.hiddenIds.filter((v): v is string => typeof v === 'string')
				: [],
		};
	} catch {
		return { displayMode: 'used', pinnedIds: [], hiddenIds: [] };
	}
}

function savePreferences(prefs: UsagePreferences) {
	if (typeof localStorage === 'undefined') return;
	try {
		localStorage.setItem(PREFS_STORAGE_KEY, JSON.stringify(prefs));
	} catch {
		// Ignore storage quota / privacy mode errors.
	}
}

/** Estimate burn-rate / run-out forecast from average window pace (inspired by Paseo). */
export function computeUsagePace(
	win: UsageWindow,
	observedAt: string | null | undefined,
	nowMs: number,
): UsagePace {
	if (!observedAt) return { kind: 'unknown' };
	const resetMs = win.resets_at ? Date.parse(win.resets_at) : Number.NaN;
	if (!Number.isFinite(resetMs) || resetMs <= nowMs) return { kind: 'unknown' };

	const used = win.used_pct;
	if (used != null && used >= 100) return { kind: 'reached' };

	const durationMs = (win.duration_seconds ?? 0) * 1_000;
	const sampledMs = Date.parse(observedAt);
	if (durationMs <= 0 || !Number.isFinite(sampledMs) || sampledMs >= resetMs) {
		return { kind: 'unknown' };
	}

	const elapsedMs = sampledMs - (resetMs - durationMs);
	const minElapsedMs = Math.max(60_000, Math.min(durationMs * 0.01, 3_600_000));
	if (used == null || !Number.isFinite(used) || used <= 0 || elapsedMs < minElapsedMs) {
		return { kind: 'unknown' };
	}

	const projectedAtReset = (used / elapsedMs) * durationMs;
	if (projectedAtReset > 100) {
		const runsOutMs = sampledMs + ((100 - used) / used) * elapsedMs;
		return { kind: 'runs_out', at: new Date(runsOutMs).toISOString() };
	}
	return { kind: projectedAtReset >= 90 ? 'tight' : 'on_track' };
}

export function formatResetCountdown(iso: string | null | undefined, nowMs: number): string | null {
	if (!iso) return null;
	const targetMs = Date.parse(iso);
	if (!Number.isFinite(targetMs)) return null;
	const diffSec = Math.round((targetMs - nowMs) / 1_000);
	if (diffSec <= 0) return 'now';
	const mins = Math.ceil(diffSec / 60);
	if (mins < 60) return `${mins}m`;
	const hours = Math.floor(mins / 60);
	const remMins = mins % 60;
	if (hours < 24) return remMins > 0 ? `${hours}h ${remMins}m` : `${hours}h`;
	const days = Math.floor(hours / 24);
	const remHours = hours % 24;
	return remHours > 0 ? `${days}d ${remHours}h` : `${days}d`;
}

export function effectiveWindows(report: UsageAccountReport): UsageWindow[] {
	if (report.status === 'available' && report.windows.length > 0) {
		return report.windows;
	}
	if (report.last_successful?.windows?.length) {
		return report.last_successful.windows;
	}
	return report.windows;
}

export function summaryWindows(report: UsageAccountReport): UsageWindow[] {
	const wins = effectiveWindows(report);
	const flagged = wins.filter((w) => w.summary);
	return flagged.length > 0 ? flagged.slice(0, 2) : wins.slice(0, 2);
}

export function displayPercent(win: UsageWindow, mode: UsageDisplayMode): number | null {
	if (mode === 'remaining') {
		if (win.remaining_pct != null) return win.remaining_pct;
		if (win.used_pct != null) return Math.max(0, 100 - win.used_pct);
		return null;
	}
	if (win.used_pct != null) return win.used_pct;
	if (win.remaining_pct != null) return Math.max(0, 100 - win.remaining_pct);
	return null;
}

class UsageStore {
	snapshot = $state<UsageSnapshot | null>(null);
	loading = $state(false);
	refreshing = $state(false);
	error = $state<string | null>(null);
	now = $state(Date.now());
	displayMode = $state<UsageDisplayMode>('used');
	pinnedIds = $state<string[]>([]);
	hiddenIds = $state<string[]>([]);

	private subscribers = 0;
	private prefsLoaded = false;
	private timer: ReturnType<typeof setInterval> | null = null;
	private clockTimer: ReturnType<typeof setInterval> | null = null;
	private listening = false;
	private inFlight: Promise<void> | null = null;

	get accounts(): UsageAccountReport[] {
		return this.snapshot?.accounts ?? [];
	}

	get visibleAccounts(): UsageAccountReport[] {
		const hidden = new Set(this.hiddenIds);
		return this.accounts.filter((a) => !hidden.has(a.id));
	}

	get pinnedAccounts(): UsageAccountReport[] {
		const visible = this.visibleAccounts;
		if (this.pinnedIds.length > 0) {
			const pinnedSet = new Set(this.pinnedIds);
			const matched = visible.filter((a) => pinnedSet.has(a.id));
			if (matched.length > 0) return matched;
		}
		// Default: show up to 3 accounts that have quota windows (available or cached).
		return visible.filter((a) => effectiveWindows(a).length > 0).slice(0, 3);
	}

	get deferredAutomations(): DeferredAutomationSummary[] {
		return this.snapshot?.deferred_automations ?? [];
	}

	subscribe(): () => void {
		if (!this.prefsLoaded) {
			const prefs = loadPreferences();
			this.displayMode = prefs.displayMode;
			this.pinnedIds = prefs.pinnedIds;
			this.hiddenIds = prefs.hiddenIds;
			this.prefsLoaded = true;
		}
		this.subscribers += 1;
		if (this.subscribers === 1) {
			this.start();
		}
		return () => {
			this.subscribers = Math.max(0, this.subscribers - 1);
			if (this.subscribers === 0) {
				this.stop();
			}
		};
	}

	setDisplayMode(mode: UsageDisplayMode) {
		this.displayMode = mode;
		savePreferences({
			displayMode: this.displayMode,
			pinnedIds: this.pinnedIds,
			hiddenIds: this.hiddenIds,
		});
	}

	togglePin(id: string) {
		if (this.pinnedIds.includes(id)) {
			this.pinnedIds = this.pinnedIds.filter((item) => item !== id);
		} else {
			this.pinnedIds = [...this.pinnedIds, id];
		}
		savePreferences({
			displayMode: this.displayMode,
			pinnedIds: this.pinnedIds,
			hiddenIds: this.hiddenIds,
		});
	}

	toggleHidden(id: string) {
		if (this.hiddenIds.includes(id)) {
			this.hiddenIds = this.hiddenIds.filter((item) => item !== id);
		} else {
			this.hiddenIds = [...this.hiddenIds, id];
			this.pinnedIds = this.pinnedIds.filter((item) => item !== id);
		}
		savePreferences({
			displayMode: this.displayMode,
			pinnedIds: this.pinnedIds,
			hiddenIds: this.hiddenIds,
		});
	}

	async refresh(force = false): Promise<void> {
		if (this.inFlight) return this.inFlight;
		const run = (async () => {
			if (!this.snapshot) this.loading = true;
			this.refreshing = true;
			try {
				const loaded = await usage.get({ refresh: force });
				if (this.subscribers > 0 || force) {
					this.snapshot = loaded;
					this.error = null;
					this.now = Date.now();
				}
			} catch (caught) {
				if (this.subscribers > 0 || force) {
					this.error = caught instanceof Error ? caught.message : 'Unable to load harness usage';
				}
			} finally {
				this.loading = false;
				this.refreshing = false;
				this.inFlight = null;
			}
		})();
		this.inFlight = run;
		return run;
	}

	private handleVisibilityChange = () => {
		if (this.timer) clearInterval(this.timer);
		if (this.clockTimer) clearInterval(this.clockTimer);
		this.timer = null;
		this.clockTimer = null;
		if (this.subscribers > 0 && !document.hidden) {
			this.now = Date.now();
			void this.refresh(false);
			this.timer = setInterval(() => void this.refresh(false), POLL_INTERVAL_MS);
			this.clockTimer = setInterval(() => {
				this.now = Date.now();
			}, 30_000);
		}
	};

	private start() {
		if (typeof document === 'undefined') return;
		if (!this.listening) {
			document.addEventListener('visibilitychange', this.handleVisibilityChange);
			this.listening = true;
		}
		this.handleVisibilityChange();
	}

	private stop() {
		if (this.timer) clearInterval(this.timer);
		if (this.clockTimer) clearInterval(this.clockTimer);
		this.timer = null;
		this.clockTimer = null;
		if (this.listening && typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', this.handleVisibilityChange);
			this.listening = false;
		}
	}
}

export const usageStore = new UsageStore();
