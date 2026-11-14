import { getContext, hasContext, setContext } from 'svelte';
import { sendToLogin, ui, type UiRevisions } from './api';

export type UiDomain =
	| 'projects'
	| 'agents'
	| 'conversations'
	| 'tasks'
	| 'sessions'
	| 'schedules'
	| 'workflows';

export type LiveUpdateSource = 'sse' | 'revisions' | 'fallback' | 'resume';

export interface LiveUpdateContext {
	source: LiveUpdateSource;
	changedDomains: ReadonlySet<UiDomain>;
}

export interface SubscribeLiveUpdatesOptions {
	/** Returns false when this subscriber's view or split pane is offscreen. */
	active?: () => boolean;
	/** Fallback refresh interval (ms) when live SSE and revisions are unavailable. */
	fallbackIntervalMs?: number;
	/**
	 * Optional predicate consulted only for synthetic fallback ticks when neither
	 * `/api/ui/stream` nor `/api/ui/revisions` is available (e.g. partial mocks).
	 */
	fallbackActive?: () => boolean;
}

const ALL_DOMAINS: readonly UiDomain[] = [
	'projects',
	'agents',
	'conversations',
	'tasks',
	'sessions',
	'schedules',
	'workflows'
];

const PANE_ACTIVE_CONTEXT = Symbol('xpressclaw.workspace.paneActive');

export function providePaneActive(getActive: () => boolean): void {
	setContext(PANE_ACTIVE_CONTEXT, getActive);
}

export function usePaneActive(): () => boolean {
	if (hasContext(PANE_ACTIVE_CONTEXT)) {
		return getContext<() => boolean>(PANE_ACTIVE_CONTEXT);
	}
	return () => true;
}

interface Subscriber {
	id: number;
	domains: UiDomain[];
	callback: (revisions: UiRevisions, context: LiveUpdateContext) => void | Promise<void>;
	isActive: () => boolean;
	fallbackIntervalMs: number;
	fallbackActive: () => boolean;
	lastRunAt: number;
	inFlight: boolean;
	pendingSource: LiveUpdateSource | null;
	pendingDomains: Set<UiDomain>;
	dirty: boolean;
	dirtyDomains: Set<UiDomain>;
}

let nextSubscriberId = 1;
const subscribers = new Map<number, Subscriber>();
const streamStatusListeners = new Set<(connected: boolean) => void>();

let currentRevisions: UiRevisions | null = null;
let streamAbort: AbortController | null = null;
let streamConnected = false;
let streamUnsupported = false;
let revisionsSupported: boolean | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;
let fallbackTimer: ReturnType<typeof setTimeout> | null = null;
let fallbackDueAt: number | null = null;
let fallbackTickInFlight = false;
let adaptiveFallbackMs = 3000;
let listenersInstalled = false;

const EMPTY_REVISIONS: UiRevisions = {
	seq: 0,
	projects: 0,
	agents: 0,
	conversations: 0,
	tasks: 0,
	sessions: 0,
	schedules: 0,
	workflows: 0
};

function isValidUiRevisions(value: unknown): value is UiRevisions {
	if (typeof value !== 'object' || value === null) return false;
	const candidate = value as Record<string, unknown>;
	return (
		typeof candidate.seq === 'number' &&
		ALL_DOMAINS.every((domain) => typeof candidate[domain] === 'number')
	);
}

function isDocumentHidden(): boolean {
	return typeof document !== 'undefined' && document.hidden;
}

function setStreamConnected(connected: boolean): void {
	if (streamConnected === connected) return;
	streamConnected = connected;
	for (const listener of streamStatusListeners) {
		try {
			listener(connected);
		} catch {
			// Ignore listener errors.
		}
	}
}

export function isLiveStreamConnected(): boolean {
	return streamConnected;
}

export function subscribeLiveStreamStatus(listener: (connected: boolean) => void): () => void {
	streamStatusListeners.add(listener);
	return () => {
		streamStatusListeners.delete(listener);
	};
}

function installGlobalListeners(): void {
	if (listenersInstalled || typeof window === 'undefined') return;
	listenersInstalled = true;

	document.addEventListener('visibilitychange', () => {
		if (isDocumentHidden()) {
			stopStream();
			stopFallbackLoop();
			return;
		}
		flushDirtySubscribers();
		ensureTransport();
	});

	window.addEventListener('online', () => {
		streamUnsupported = false;
		reconnectAttempt = 0;
		if (!isDocumentHidden() && subscribers.size > 0) {
			ensureTransport();
		}
	});
}

export function notifyLiveUpdatesVisibilityChanged(): void {
	if (isDocumentHidden()) return;
	flushDirtySubscribers();
}

function flushDirtySubscribers(): void {
	const snapshot = currentRevisions ?? EMPTY_REVISIONS;
	for (const sub of subscribers.values()) {
		if (!sub.dirty || !sub.isActive()) continue;
		const changed = new Set(sub.dirtyDomains.size > 0 ? sub.dirtyDomains : sub.domains);
		sub.dirty = false;
		sub.dirtyDomains.clear();
		triggerSubscriber(sub, snapshot, 'resume', changed);
	}
}

function triggerSubscriber(
	sub: Subscriber,
	revisions: UiRevisions,
	source: LiveUpdateSource,
	changedDomains: Iterable<UiDomain>
): void {
	if (!subscribers.has(sub.id)) return;

	if (isDocumentHidden() || !sub.isActive()) {
		sub.dirty = true;
		for (const domain of changedDomains) sub.dirtyDomains.add(domain);
		return;
	}

	if (sub.inFlight) {
		sub.pendingSource = sub.pendingSource === 'sse' ? 'sse' : source;
		for (const domain of changedDomains) sub.pendingDomains.add(domain);
		return;
	}

	sub.inFlight = true;
	sub.lastRunAt = Date.now();
	const domainSet = new Set(changedDomains);

	Promise.resolve()
		.then(() => sub.callback(revisions, { source, changedDomains: domainSet }))
		.catch(() => {
			// Individual view callbacks handle their own error states.
		})
		.finally(() => {
			sub.inFlight = false;
			if (!subscribers.has(sub.id)) return;
			if (sub.pendingSource !== null) {
				const nextSource = sub.pendingSource;
				const nextDomains = new Set(sub.pendingDomains);
				sub.pendingSource = null;
				sub.pendingDomains.clear();
				triggerSubscriber(sub, currentRevisions ?? revisions, nextSource, nextDomains);
			}
		});
}

function applyIncomingRevisions(next: UiRevisions, source: 'sse' | 'revisions'): void {
	const previous = currentRevisions;
	currentRevisions = next;

	if (previous === null) {
		// Establish baseline on first observation; views perform their own initial load.
		return;
	}

	const serverRestarted = next.seq < previous.seq;
	const changed = new Set<UiDomain>();
	for (const domain of ALL_DOMAINS) {
		if (serverRestarted || next[domain] !== previous[domain]) {
			changed.add(domain);
		}
	}
	if (changed.size === 0) return;

	for (const sub of subscribers.values()) {
		const matching = sub.domains.filter((domain) => changed.has(domain));
		if (matching.length > 0) {
			triggerSubscriber(sub, next, source, matching);
		}
	}
}

function stopStream(): void {
	if (reconnectTimer !== null) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	if (streamAbort) {
		streamAbort.abort();
		streamAbort = null;
	}
	setStreamConnected(false);
}

function scheduleStreamReconnect(delayMs: number): void {
	if (reconnectTimer !== null || subscribers.size === 0 || isDocumentHidden()) return;
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		if (subscribers.size > 0 && !isDocumentHidden()) {
			void connectStream();
		}
	}, delayMs);
}

async function connectStream(): Promise<void> {
	if (
		typeof window === 'undefined' ||
		streamAbort !== null ||
		streamUnsupported ||
		subscribers.size === 0 ||
		isDocumentHidden()
	) {
		return;
	}

	const controller = new AbortController();
	streamAbort = controller;

	try {
		const response = await fetch('/api/ui/stream', {
			headers: { Accept: 'text/event-stream' },
			credentials: 'same-origin',
			cache: 'no-store',
			signal: controller.signal
		});

		if (controller.signal.aborted) return;

		if (response.status === 401) {
			stopStream();
			sendToLogin();
			return;
		}

		const contentType = (response.headers.get('content-type') ?? '').toLowerCase();
		if (!response.ok || !contentType.includes('text/event-stream') || !response.body) {
			streamAbort = null;
			setStreamConnected(false);
			if (response.status === 404 || response.status === 204 || response.ok) {
				// Endpoint is mocked out or disabled; use direct fallback loop without noisy retries.
				streamUnsupported = true;
				revisionsSupported = false;
				ensureFallbackLoop();
				return;
			}
			throw new Error(`Unexpected stream status ${response.status}`);
		}

		reconnectAttempt = 0;
		revisionsSupported = true;
		setStreamConnected(true);
		stopFallbackLoop();

		const reader = response.body.getReader();
		const decoder = new TextDecoder();
		let buffer = '';
		let eventName = 'message';
		let dataLines: string[] = [];

		const dispatchFrame = () => {
			if (dataLines.length === 0) {
				eventName = 'message';
				return;
			}
			const raw = dataLines.join('\n');
			const currentEvent = eventName;
			dataLines = [];
			eventName = 'message';
			if (currentEvent === 'revisions') {
				try {
					const parsed = JSON.parse(raw);
					if (isValidUiRevisions(parsed)) {
						applyIncomingRevisions(parsed, 'sse');
					}
				} catch {
					// Ignore malformed frame.
				}
			}
		};

		while (true) {
			const { value, done } = await reader.read();
			if (done || controller.signal.aborted) break;
			buffer += decoder.decode(value, { stream: true });
			let newlineIdx = buffer.indexOf('\n');
			while (newlineIdx !== -1) {
				const rawLine = buffer.slice(0, newlineIdx);
				buffer = buffer.slice(newlineIdx + 1);
				const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
				if (line === '') {
					dispatchFrame();
				} else if (!line.startsWith(':')) {
					const colon = line.indexOf(':');
					const field = colon === -1 ? line : line.slice(0, colon);
					let val = colon === -1 ? '' : line.slice(colon + 1);
					if (val.startsWith(' ')) val = val.slice(1);
					if (field === 'event') eventName = val;
					else if (field === 'data') dataLines.push(val);
				}
				newlineIdx = buffer.indexOf('\n');
			}
		}
	} catch {
		// Network interruption or aborted stream.
	} finally {
		if (streamAbort === controller) {
			streamAbort = null;
		}
		setStreamConnected(false);
		if (subscribers.size > 0 && !isDocumentHidden() && !streamUnsupported) {
			ensureFallbackLoop();
			const base = Math.min(15_000, 1000 * 2 ** Math.min(reconnectAttempt, 4));
			reconnectAttempt += 1;
			scheduleStreamReconnect(base);
		}
	}
}

function stopFallbackLoop(): void {
	if (fallbackTimer !== null) {
		clearTimeout(fallbackTimer);
		fallbackTimer = null;
	}
	fallbackDueAt = null;
}

function nextFallbackDelayMs(): number {
	const now = Date.now();
	let minActiveIntervalMs = 15_000;
	for (const sub of subscribers.values()) {
		if (sub.isActive()) {
			minActiveIntervalMs = Math.min(minActiveIntervalMs, sub.fallbackIntervalMs);
		}
	}
	if (revisionsSupported === false) {
		let earliestDueMs = Math.max(1000, minActiveIntervalMs);
		for (const sub of subscribers.values()) {
			if (sub.isActive() && sub.fallbackActive()) {
				const remaining = sub.lastRunAt + sub.fallbackIntervalMs - now;
				earliestDueMs = Math.min(earliestDueMs, Math.max(50, remaining));
			}
		}
		return earliestDueMs;
	}
	return Math.max(minActiveIntervalMs, adaptiveFallbackMs);
}

function ensureFallbackLoop(): void {
	if (streamConnected || subscribers.size === 0 || isDocumentHidden()) {
		return;
	}
	const delay = nextFallbackDelayMs();
	const desiredDueAt = Date.now() + delay;
	if (fallbackTimer !== null) {
		if (fallbackDueAt !== null && desiredDueAt < fallbackDueAt - 50) {
			clearTimeout(fallbackTimer);
			fallbackTimer = null;
		} else {
			return;
		}
	}
	fallbackDueAt = desiredDueAt;
	fallbackTimer = setTimeout(() => {
		fallbackTimer = null;
		fallbackDueAt = null;
		void runFallbackTick();
	}, delay);
}

async function runFallbackTick(): Promise<void> {
	if (streamConnected || subscribers.size === 0 || isDocumentHidden()) return;
	if (fallbackTickInFlight) {
		ensureFallbackLoop();
		return;
	}
	fallbackTickInFlight = true;
	const startedAt = Date.now();

	try {
		if (revisionsSupported !== false) {
			const timeoutController = new AbortController();
			const timeoutId = setTimeout(() => timeoutController.abort(), 8000);
			try {
				const snapshot = await ui.revisions(timeoutController.signal);
				if (isValidUiRevisions(snapshot)) {
					revisionsSupported = true;
					const elapsed = Date.now() - startedAt;
					adaptiveFallbackMs = elapsed > 1500 ? Math.min(15_000, elapsed * 3) : 3000;
					applyIncomingRevisions(snapshot, 'revisions');
					return;
				}
				revisionsSupported = false;
			} catch (err) {
				if (revisionsSupported === null) {
					// Check if unmocked/404 vs transient network failure.
					const status = (err as { status?: number } | null)?.status;
					if (status === 404) {
						revisionsSupported = false;
					} else {
						adaptiveFallbackMs = Math.min(15_000, adaptiveFallbackMs * 2);
						return;
					}
				} else {
					adaptiveFallbackMs = Math.min(15_000, adaptiveFallbackMs * 2);
					return;
				}
			} finally {
				clearTimeout(timeoutId);
			}
		}

		// Synthetic fallback tick when neither SSE nor /api/ui/revisions is available.
		const now = Date.now();
		const snapshot = currentRevisions ?? EMPTY_REVISIONS;
		for (const sub of subscribers.values()) {
			if (!sub.isActive() || !sub.fallbackActive()) continue;
			if (now - sub.lastRunAt >= sub.fallbackIntervalMs - 50) {
				triggerSubscriber(sub, snapshot, 'fallback', sub.domains);
			}
		}
	} finally {
		fallbackTickInFlight = false;
		if (!streamConnected && subscribers.size > 0 && !isDocumentHidden()) {
			ensureFallbackLoop();
		}
	}
}

function ensureTransport(): void {
	if (subscribers.size === 0 || isDocumentHidden()) return;
	if (!streamUnsupported) {
		void connectStream();
	} else {
		ensureFallbackLoop();
	}
}

export function subscribeLiveUpdates(
	domains: UiDomain[],
	callback: (revisions: UiRevisions, context: LiveUpdateContext) => void | Promise<void>,
	options: SubscribeLiveUpdatesOptions = {}
): () => void {
	installGlobalListeners();
	let paneActive = () => true;
	try {
		paneActive = usePaneActive();
	} catch {
		// Called outside synchronous component initialization; rely on options.active.
	}

	const id = nextSubscriberId++;
	const subscriber: Subscriber = {
		id,
		domains: [...domains],
		callback,
		isActive: () => paneActive() && (options.active ? options.active() : true),
		fallbackIntervalMs: options.fallbackIntervalMs ?? 3000,
		fallbackActive: options.fallbackActive ?? (() => true),
		lastRunAt: Date.now(),
		inFlight: false,
		pendingSource: null,
		pendingDomains: new Set(),
		dirty: false,
		dirtyDomains: new Set()
	};
	subscribers.set(id, subscriber);
	ensureTransport();

	return () => {
		subscribers.delete(id);
		if (subscribers.size === 0) {
			stopStream();
			stopFallbackLoop();
			currentRevisions = null;
			streamUnsupported = false;
			revisionsSupported = null;
			reconnectAttempt = 0;
		}
	};
}
