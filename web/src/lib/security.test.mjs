import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { framed, frameBuster } from './security.js';

// --- frame-buster
const same = { }; same.top = same; same.self = same;
assert.equal(framed(same), false);
assert.equal(framed({ top: {}, self: {} }), true);
assert.equal(framed({ get top() { throw new Error('cross-origin'); }, self: {} }), true, 'a throwing top is framed');
const el = () => ({ children: [], style: {}, append(...c) { this.children.push(...c); }, set textContent(v) { this.t = v; this.children = []; } });
const root = el();
const doc = { getElementById: () => root, createElement: () => el(), body: el() };
assert.equal(frameBuster({ top: {}, self: {}, location: { href: 'https://x/y' } }, doc), true);
assert.ok(root.children.length >= 1, 'the framed page shows its link');
const root2 = el(); root2.children.push('app');
assert.equal(frameBuster(same, { ...doc, getElementById: () => root2 }), false);
assert.deepEqual(root2.children, ['app'], 'an unframed page is untouched');

// --- the meta CSP in index.html and the header CSP of the dedicated origin agree (the header adds frame-ancestors only)
const html = readFileSync(new URL('../../index.html', import.meta.url), 'utf8');
const meta = /http-equiv="Content-Security-Policy" content="([^"]+)"/.exec(html)[1];
const headers = readFileSync(new URL('../../deploy/cloudflare/_headers', import.meta.url), 'utf8');
const blocks = headers.split(/\n(?=\/)/);
const rule = (name) => blocks.find((b) => b.startsWith(name + '\n'));
const csp = (b) => /^\s+Content-Security-Policy: (.+)$/m.exec(b)[1];
const parse = (s) => Object.fromEntries(s.split(';').map((d) => d.trim()).filter(Boolean).map((d) => { const [k, ...v] = d.split(/\s+/); return [k, v.join(' ')]; }));
const m = parse(meta), h = parse(csp(rule('/*')));
for (const k of Object.keys(m)) assert.equal(h[k], m[k], `directive ${k} differs between index.html and _headers`);
assert.equal(h['frame-ancestors'], "'none'");
// the light-client network: iroh reaches a node's home relay over WSS (relay hosts come from public-node.json and node announcements,
// so they are not listable; https: is already open to any host, wss: only adds the scheme) and its WASM needs wasm-unsafe-eval
assert.ok(/\bwss:/.test(m['connect-src']) && /\bwss:/.test(h['connect-src']), 'connect-src allows wss: for the iroh relays');
assert.ok(/'wasm-unsafe-eval'/.test(m['script-src']), 'the lc WASM compiles under wasm-unsafe-eval');
assert.ok(!/ws:/.test(m['connect-src'].replace(/wss:/g, '')), 'no plain ws:');
assert.ok(!/'unsafe-eval'/.test(csp(rule('/*'))), 'the document policy has no unsafe-eval');
assert.ok(/'unsafe-eval'/.test(csp(rule('/workers/*'))), 'workers may use new Function');
assert.ok(/^\s+! Content-Security-Policy$/m.test(rule('/workers/*')), 'the worker rule detaches the strict policy');
for (const need of ['Strict-Transport-Security', 'Cross-Origin-Opener-Policy: same-origin', 'Cross-Origin-Embedder-Policy', 'Permissions-Policy', 'X-Content-Type-Options: nosniff', 'Referrer-Policy']) assert.ok(rule('/*').includes(need), need);

// --- coi-sw.js: kill file and fetch fallback, run in a vm with a fake service worker scope
function runSw({ fetchImpl }) {
  const listeners = {};
  const unreg = [];
  const self = {
    location: { href: 'https://h/sm64coin/coi-sw.js?coep=credentialless' },
    registration: { scope: 'https://h/sm64coin/', unregister: async () => { unreg.push(1); } },
    addEventListener: (t, f) => { listeners[t] = f; },
    skipWaiting() {}, clients: { claim: async () => {} },
  };
  const ctx = vm.createContext({ self, URL, Headers, Response, Request: class { constructor(r) { Object.assign(this, r); } }, fetch: fetchImpl, Date, setTimeout, Promise });
  vm.runInContext(readFileSync(new URL('../../public/coi-sw.js', import.meta.url), 'utf8'), ctx);
  const fire = async (req) => { let p; listeners.fetch({ request: req, respondWith: (x) => { p = x; } }); return p; };
  return { fire, unreg, listeners };
}
{
  const calls = [];
  const sw = runSw({ fetchImpl: async (r) => { const u = r.url || String(r); calls.push(u); if (u.endsWith('coi-sw-off.txt')) return new Response('nope', { status: 404 }); return new Response('ok', { status: 200 }); } });
  const res = await sw.fire({ url: 'https://h/sm64coin/a', mode: 'navigate', cache: 'default' });
  assert.equal(res.headers.get('Cross-Origin-Opener-Policy'), 'same-origin', 'normal path re-signs');
  assert.equal(sw.unreg.length, 0);
}
{
  let n = 0;
  const sw = runSw({ fetchImpl: async (r) => { const u = r.url || String(r); if (u.endsWith('coi-sw-off.txt')) return new Response('nope', { status: 404 }); if (n++ === 0) throw new Error('wrap failed'); return new Response('plain', { status: 200 }); } });
  const res = await sw.fire({ url: 'https://h/sm64coin/a', mode: 'navigate', cache: 'default' });
  assert.equal(await res.text(), 'plain', 'a failed re-signed fetch falls back to the plain network');
}
{
  const sw = runSw({ fetchImpl: async (r) => { const u = r.url || String(r); if (u.endsWith('coi-sw-off.txt')) return new Response('off\n', { status: 200 }); return new Response('plain', { status: 200 }); } });
  const res = await sw.fire({ url: 'https://h/sm64coin/a', mode: 'navigate', cache: 'default' });
  assert.equal(res.headers.get('Cross-Origin-Opener-Policy'), null, 'killed worker passes requests through');
  assert.equal(sw.unreg.length, 1, 'and unregisters itself');
}
{ // an SPA host answering the kill path with index.html must not kill the worker
  const sw = runSw({ fetchImpl: async (r) => new Response('<!doctype html>', { status: 200 }) });
  const res = await sw.fire({ url: 'https://h/sm64coin/a', mode: 'navigate', cache: 'default' });
  assert.equal(res.headers.get('Cross-Origin-Opener-Policy'), 'same-origin');
  assert.equal(sw.unreg.length, 0);
}
console.log('security tests ok');
