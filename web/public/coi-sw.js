// Makes the page cross-origin isolated (COOP same-origin + COEP), which SharedArrayBuffer needs (the Auto-mine
// workers sync through one). GitHub Pages can't set response headers; this service worker adds them. Registered from the
// Auto-mine card ("Enable worker sync", lib/webminer/sync.js), which passes ?coep=credentialless (Chromium, Firefox 119+)
// or ?coep=require-corp (Safari and others: they don't know credentialless; cross-origin no-cors responses then also get
// Cross-Origin-Resource-Policy so they still load).
const COEP = new URL(self.location.href).searchParams.get('coep') === 'require-corp' ? 'require-corp' : 'credentialless';
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('message', (e) => { if (e.data === 'ping' && e.source) e.source.postMessage({ coi: COEP }); });
self.addEventListener('fetch', (e) => {
  let r = e.request;
  // COEP credentialless: no-cors cross-origin requests go out without credentials (as coi-serviceworker does).
  if (COEP === 'credentialless' && r.mode === 'no-cors') r = new Request(r, { credentials: 'omit' });
  if (r.cache === 'only-if-cached' && r.mode !== 'same-origin') return;
  e.respondWith(
    fetch(r).then((res) => {
      if (res.status === 0) return res; // opaque: can't be rewrapped
      const h = new Headers(res.headers);
      h.set('Cross-Origin-Embedder-Policy', COEP);
      h.set('Cross-Origin-Opener-Policy', 'same-origin');
      if (COEP === 'require-corp' && !h.has('Cross-Origin-Resource-Policy')) h.set('Cross-Origin-Resource-Policy', 'cross-origin');
      return new Response(res.body, { status: res.status, statusText: res.statusText, headers: h });
    })
  );
});
