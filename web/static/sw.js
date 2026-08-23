/* TootTok service worker: cache-first for immutable build assets,
   network-first for navigations with cached-shell offline fallback. */

/* Bump on every deploy so activate() purges the previous cache (it deletes
   any cache whose name !== CACHE), clearing stale hashed /_app/ chunks. */
const CACHE = 'toottok-v3';
const SHELL = '/index.html';
const PRECACHE = [SHELL, '/manifest.webmanifest', '/icon.svg'];

self.addEventListener('install', (event) => {
	event.waitUntil(
		caches
			.open(CACHE)
			.then((cache) => Promise.all(PRECACHE.map((url) => cache.add(url).catch(() => {}))))
			.then(() => self.skipWaiting())
	);
});

self.addEventListener('activate', (event) => {
	event.waitUntil(
		caches
			.keys()
			.then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))))
			.then(() => self.clients.claim())
	);
});

self.addEventListener('fetch', (event) => {
	const request = event.request;
	if (request.method !== 'GET') return;

	const url = new URL(request.url);
	if (url.origin !== self.location.origin) return;

	if (url.pathname.startsWith('/_app/')) {
		event.respondWith(
			caches.match(request).then(
				(hit) =>
					hit ||
					fetch(request).then((response) => {
						if (response.ok) {
							const copy = response.clone();
							caches.open(CACHE).then((cache) => cache.put(request, copy));
						}
						return response;
					})
			)
		);
		return;
	}

	if (request.mode === 'navigate') {
		event.respondWith(
			fetch(request, { cache: 'no-cache' })
				.then((response) => {
					if (response.ok) {
						const copy = response.clone();
						caches.open(CACHE).then((cache) => cache.put(SHELL, copy));
						return response;
					}
					return caches.match(SHELL).then((cached) => cached || response);
				})
				.catch(async () => {
					const cached = await caches.match(SHELL);
					if (!cached) throw new Error('offline and no cached shell');
					const clients = await self.clients.matchAll({ type: 'window' });
					for (const client of clients) client.postMessage({ type: 'OFFLINE' });
					return cached;
				})
		);
	}
});
