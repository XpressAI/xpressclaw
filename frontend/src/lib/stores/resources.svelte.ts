import { dashboard, type DashboardResources } from '$lib/api';

const POLL_INTERVAL_MS = 5_000;
const REQUEST_TIMEOUT_MS = 15_000;
const STALE_AFTER_MS = 20_000;

class ResourceStore {
	resources = $state<DashboardResources | null>(null);
	loading = $state(true);
	error = $state(false);
	now = $state(Date.now());
	receivedAt = $state<number | null>(null);

	private subscribers = 0;
	private inFlight = false;
	private timer: ReturnType<typeof setTimeout> | null = null;
	private clockTimer: ReturnType<typeof setInterval> | null = null;
	private requestController: AbortController | null = null;
	private listening = false;

	get stale(): boolean {
		return this.error || (this.receivedAt !== null && this.now - this.receivedAt > STALE_AFTER_MS);
	}

	subscribe(): () => void {
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

	retry = () => {
		void this.refresh();
	};

	private handleVisibilityChange = () => {
		if (this.timer) clearTimeout(this.timer);
		if (this.clockTimer) clearInterval(this.clockTimer);
		this.timer = null;
		this.clockTimer = null;
		if (this.subscribers > 0 && !document.hidden) {
			this.now = Date.now();
			this.clockTimer = setInterval(() => {
				this.now = Date.now();
			}, POLL_INTERVAL_MS);
			void this.refresh();
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
		if (this.timer) clearTimeout(this.timer);
		if (this.clockTimer) clearInterval(this.clockTimer);
		this.timer = null;
		this.clockTimer = null;
		this.requestController?.abort();
		this.requestController = null;
		this.inFlight = false;
		if (this.listening && typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', this.handleVisibilityChange);
			this.listening = false;
		}
	}

	private async refresh() {
		if (this.subscribers === 0 || this.inFlight || (typeof document !== 'undefined' && document.hidden)) {
			return;
		}
		if (this.timer) {
			clearTimeout(this.timer);
			this.timer = null;
		}
		this.inFlight = true;
		const controller = new AbortController();
		this.requestController = controller;
		const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
		try {
			const sample = await dashboard.resources(controller.signal);
			if (this.subscribers === 0) return;
			if (sample.sampled_at !== this.resources?.sampled_at) {
				this.receivedAt = Date.now();
			}
			this.resources = sample;
			this.error = false;
		} catch {
			if (this.subscribers > 0) this.error = true;
		} finally {
			clearTimeout(timeout);
			if (this.requestController === controller) this.requestController = null;
			this.inFlight = false;
			if (this.subscribers > 0) {
				this.loading = false;
				this.now = Date.now();
				if (typeof document === 'undefined' || !document.hidden) {
					this.timer = setTimeout(() => void this.refresh(), POLL_INTERVAL_MS);
				}
			}
		}
	}
}

export const resourceStore = new ResourceStore();
