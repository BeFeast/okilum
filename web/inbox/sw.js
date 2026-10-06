const CACHE = 'tessera-inbox-shell-v8';
const SHELL = ['/', '/app.js', '/launches.js', '/questions.js', '/questions-view.js', '/outbox.js', '/publication-form.js', '/webauthn.js', '/style.css', '/manifest.webmanifest', '/icon.svg'];
self.addEventListener('install', event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});
self.addEventListener('activate', event => {
  event.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(key => key.startsWith('tessera-inbox-shell-') && key !== CACHE).map(key => caches.delete(key)))).then(() => self.clients.claim()));
});
self.addEventListener('fetch', event => {
  const url = new URL(event.request.url);
  // Never cache authenticated data, auth responses, or arbitrary URLs.
  if (event.request.method !== 'GET' || url.origin !== self.location.origin || !SHELL.includes(url.pathname) || url.search) return;
  event.respondWith(fetch(event.request).then(async response => {
    if (response.ok) { const cache = await caches.open(CACHE); await cache.put(url.pathname, response.clone()); }
    return response;
  }).catch(() => caches.match(url.pathname)));
});
