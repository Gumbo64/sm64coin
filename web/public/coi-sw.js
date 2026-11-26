// Makes the page cross-origin isolated (COOP same-origin + COEP), which SharedArrayBuffer needs (the Auto-mine
// workers sync through one). GitHub Pages can't set response headers; this service worker adds them. Registered at startup
// (lib/webminer/sync.js), which passes ?coep=credentialless (Chromium, Firefox 119+)
// or ?coep=require-corp (Safari and others: they don't know credentialless; cross-origin no-cors responses then also get
// Cross-Origin-Resource-Policy so they still load). Not used on the dedicated origin, whose headers do this natively.
const COEP = new URL(self.location.href).searchParams.get('coep') === 'require-corp' ? 'require-corp' : 'credentialless';

// Remote kill switch: if <scope>coi-sw-off.txt exists and starts with "off", the worker unregisters itself and every request
// goes straight to the network. Checked at activation and, while running, at most every 10 minutes (on a fetch). The file must
// say "off": a host that answers unknown paths with index.html must not switch the worker off by accident.
const OFF_URL = new URL('coi-sw-off.txt', self.registration.scope).href;
let offChecked = 0, off = false;
async function killed() {
  if (off) return true;
  if (Date.now() - offChecked < 600000) return false;
  offChecked = Date.now();
  try {
    const r = await fetch(OFF_URL, { cache: 'no-store', credentials: 'omit' });
    if (r.ok && /^\s*off\b/i.test(await r.text())) { off = true; self.registration.unregister().catch(() => {}); }
  } catch { /* offline: stay as is */ }
  return off;
}

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(Promise.all([self.clients.claim(), killed()])));
self.addEventListener('message', (e) => { if (e.data === 'ping' && e.source) e.source.postMessage({ coi: COEP }); });
self.addEventListener('fetch', (e) => {
  let r = e.request;
  // COEP credentialless: no-cors cross-origin requests go out without credentials (as coi-serviceworker does).
  if (COEP === 'credentialless' && r.mode === 'no-cors') r = new Request(r, { credentials: 'omit' });
  if (r.cache === 'only-if-cached' && r.mode !== 'same-origin') return;
  e.respondWith((async () => {
    if (await killed()) return fetch(e.request);
    try {
      const res = await fetch(r);
      if (res.status === 0) return res; // opaque: can't be rewrapped
      const h = new Headers(res.headers);
      h.set('Cross-Origin-Embedder-Policy', COEP);
      h.set('Cross-Origin-Opener-Policy', 'same-origin');
      if (COEP === 'require-corp' && !h.has('Cross-Origin-Resource-Policy')) h.set('Cross-Origin-Resource-Policy', 'cross-origin');
      return new Response(res.body, { status: res.status, statusText: res.statusText, headers: h });
    } catch {
      // The re-signed fetch failed (a request the worker cannot rebuild, a quirk of the wrapping): the plain network answer instead.
      return fetch(e.request);
    }
  })());
});
