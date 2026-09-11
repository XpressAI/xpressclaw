// Minimal service worker for PWA installability.
//
// XpressClaw is a live control plane: API traffic (`/api/`, SSE, WebSockets)
// and page navigations always go straight to the network. Only immutable,
// content-hashed SvelteKit build assets are cached, so updates apply as soon
// as the server ships new hashed asset URLs.
//
// Caches are keyed by the SvelteKit build version from /_app/version.json
// (which is itself never cached). The current release and the immediately
// previous release are both retained: already-open clients of the previous
// release keep loading their lazy chunks across a deployment, while older
// caches are pruned so storage stays bounded.

const VERSION_URL = '/_app/version.json';
const CACHE_PREFIX = 'xpressclaw-static-';
const UNKNOWN_CACHE = `${CACHE_PREFIX}unknown`;

// Re-probe the build version periodically rather than memoizing for the
// worker's whole lifetime: deployments often leave /sw.js unchanged, so a
// long-lived worker would otherwise keep caching new chunks under a stale
// version (and never recover from an offline first probe).
const VERSION_PROBE_TTL_MS = 60_000;

let activeCachePromise = null;
let activeCacheProbedAt = 0;
let currentCache = null;
let currentCacheName = null;
let previousCache = null;
let previousCacheName = null;

function releaseVersionFromKey(key) {
	if (!key.startsWith(CACHE_PREFIX)) return null;
	const version = key.slice(CACHE_PREFIX.length);
	return /^\d+$/.test(version) ? Number(version) : null;
}

function resolveActiveCache() {
	return (async () => {
		let version = null;
		try {
			const response = await fetch(VERSION_URL, { cache: 'no-store' });
			if (response.ok) {
				const data = await response.json();
				if (typeof data.version === 'string' && data.version) version = data.version;
			}
		} catch {
			// Offline or unreachable.
		}

		if (!version) {
			// Probe failed: keep serving from the current cache, and prune
			// nothing so cached chunks remain usable during the outage.
			if (currentCache) return currentCache;
			// Fresh worker restarted during an outage: recover the newest
			// existing release cache instead of an empty fallback.
			const keys = await caches.keys();
			const versions = keys
				.map(releaseVersionFromKey)
				.filter((value) => value !== null)
				.sort((a, b) => b - a);
			if (versions.length > 0) {
				currentCacheName = `${CACHE_PREFIX}${versions[0]}`;
				currentCache = await caches.open(currentCacheName);
				return currentCache;
			}
			currentCacheName = UNKNOWN_CACHE;
			currentCache = await caches.open(UNKNOWN_CACHE);
			return currentCache;
		}

		const cacheName = `${CACHE_PREFIX}${version}`;
		if (currentCache && currentCacheName === cacheName) return currentCache;

		// Rotate: retain the previous release's cache so already-open clients
		// can keep loading their lazy chunks until they reload.
		previousCache = currentCache;
		previousCacheName = currentCacheName;
		currentCache = await caches.open(cacheName);
		currentCacheName = cacheName;

		// The static /sw.js bytes rarely change, so the browser may never
		// re-run install/activate for a new deployment. Reconcile here — but
		// only after a valid version response. Cache objects do not expose
		// their storage key, so compare against the constructed names.
		try {
			const keys = await caches.keys();
			const retained = new Set(
				[currentCacheName, previousCacheName].filter((name) => name !== null)
			);
			await Promise.all(keys.filter((key) => !retained.has(key)).map((key) => caches.delete(key)));
		} catch {
			// Best-effort cleanup.
		}
		return currentCache;
	})().catch(async () => {
		if (currentCache) return currentCache;
		currentCacheName = UNKNOWN_CACHE;
		currentCache = await caches.open(UNKNOWN_CACHE);
		return currentCache;
	});
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
			let cached = await cache.match(request);
			// Clients still running the previous release request their own
			// hashed chunks; serve them from the retained previous cache.
			if (!cached && previousCache) cached = await previousCache.match(request);
			if (cached) return cached;
			try {
				const response = await fetch(request);
				const contentType = response.headers.get('content-type') || '';
				if (response.ok && !contentType.toLowerCase().startsWith('text/html')) {
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
