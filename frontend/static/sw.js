// Minimal service worker for PWA installability.
//
// XpressClaw is a live control plane: API traffic (`/api/`, SSE, WebSockets)
// and page navigations always go straight to the network. Only immutable,
// content-hashed SvelteKit build assets are cached, so updates apply as soon
// as the server ships new hashed asset URLs.
//
// The cache is keyed by the SvelteKit build version from /_app/version.json
// (which is itself never cached), so each deploy rotates to a fresh cache and
// activation deletes the previous release's entries instead of accumulating
// every historical asset until the storage quota is exhausted.

// Re-probe the build version periodically rather than memoizing for the
// worker's whole lifetime: deployments often leave /sw.js unchanged, so a
// long-lived worker would otherwise keep caching new chunks under a stale
// version (and never recover from an offline "unknown" first probe).
const VERSION_PROBE_TTL_MS = 60_000;

let activeCachePromise = null;
let activeCacheProbedAt = 0;
let lastResolvedCache = null;

function resolveActiveCache() {
	return (async () => {
		let version = null;
		try {
			const response = await fetch('/_app/version.json', { cache: 'no-store' });
			if (response.ok) {
				const data = await response.json();
				if (typeof data.version === 'string' && data.version) version = data.version;
			}
		} catch {
			// Offline or unreachable.
		}
		if (!version) {
			// Probe failed: keep the last resolved cache (or the fallback) and
			// prune nothing, so chunks cached under the last known release
			// remain servable while offline.
			if (lastResolvedCache) return lastResolvedCache;
			return caches.open('xpressclaw-static-unknown');
		}
		const cacheName = `xpressclaw-static-${version}`;
		const cache = await caches.open(cacheName);
		// The static /sw.js bytes rarely change, so the browser may never
		// re-run install/activate for a new deployment. Reconcile here — but
		// only after a valid version response — so stale release caches are
		// pruned whenever a new version is first observed, regardless of
		// worker lifecycle. Cache objects do not expose their storage key, so
		// compare against the constructed name.
		try {
			const keys = await caches.keys();
			await Promise.all(keys.filter((key) => key !== cacheName).map((key) => caches.delete(key)));
		} catch {
			// Best-effort cleanup.
		}
		lastResolvedCache = cache;
		return cache;
	})().catch(async () => lastResolvedCache ?? caches.open('xpressclaw-static-unknown'));
}

function activeCache() {
	const now = Date.now();
	if (!activeCachePromise || now - activeCacheProbedAt > VERSION_PROBE_TTL_MS) {
		activeCacheProbedAt = now;
		activeCachePromise = resolveActiveCache();
	}
	return activeCachePromise;
}

self.addEventListener('install', () => {
	self.skipWaiting();
});

self.addEventListener('activate', (event) => {
	// Reconciliation (including cache pruning) happens inside
	// resolveActiveCache() and only after a successful version probe; doing
	// it unconditionally here could delete a populated release cache when
	// the worker activates while /_app/version.json is unreachable.
	event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
	const request = event.request;
	if (request.method !== 'GET') return;

	const url = new URL(request.url);
	if (url.origin !== self.location.origin) return;
	// Only content-hashed build assets are safe to cache; everything else
	// (API, streams, navigations, static icons that may change) stays online.
	if (!url.pathname.startsWith('/_app/immutable/')) return;

	event.respondWith(
		(async () => {
			const cache = await activeCache();
			const cached = await cache.match(request);
			if (cached) return cached;
			try {
				const response = await fetch(request);
				const contentType = response.headers.get('content-type') || '';
				if (
					response.ok &&
					!contentType.toLowerCase().startsWith('text/html')
				) {
					try {
						// Await the write so the fetch event stays alive until the
						// cache entry is complete; otherwise the worker may be
						// terminated mid-write and leave a truncated entry.
						await cache.put(request, response.clone());
					} catch {
						// Caching is best-effort (quota, private mode): a failed
						// write must not fail an otherwise successful response.
					}
				}
				return response;
			} catch (error) {
				if (cached) return cached;
				throw error;
			}
		})()
	);
});
