// Minimal service worker for PWA installability.
//
// XpressClaw is a live control plane: API traffic (`/api/`, SSE, WebSockets)
// and page navigations always go straight to the network. Only immutable,
// content-hashed SvelteKit build assets are cached, so updates apply as soon
// as the server ships new hashed asset URLs.
const CACHE_NAME = 'xpressclaw-static-v1';

self.addEventListener('install', () => {
	self.skipWaiting();
});

self.addEventListener('activate', (event) => {
	event.waitUntil(
		(async () => {
			const keys = await caches.keys();
			await Promise.all(keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key)));
			await self.clients.claim();
		})()
	);
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
			const cache = await caches.open(CACHE_NAME);
			const cached = await cache.match(request);
			if (cached) return cached;
			try {
				const response = await fetch(request);
				if (response.ok) {
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
