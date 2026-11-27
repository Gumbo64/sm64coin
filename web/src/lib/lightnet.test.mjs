import assert from 'node:assert/strict';
import { createLightnet, bootstrapEntries, bootstrapTip, firstHttpNode, Unavailable, SUBMIT_METHODS } from './lightnet.js';

const ID = 'ab'.repeat(32);
// --- bootstrap parsing (public-node.json: nodes[] and the old url key)
assert.deepEqual(bootstrapEntries({ url: 'https://eboy.au' }), ['https://eboy.au']);
assert.deepEqual(bootstrapEntries({ nodes: ['https://a.example', `iroh:${ID}@https://relay.example`, 'ftp://x', 'iroh:zz', 'https://u@evil'], url: 'https://b.example' }),
  ['https://a.example', 'https://b.example'], 'an old iroh entry is skipped');
assert.deepEqual(bootstrapEntries(null), []);
assert.deepEqual(bootstrapEntries({ nodes: 'nope' }), []);
assert.equal(firstHttpNode({ nodes: [`iroh:${ID}`, 'https://a.example/'] }), 'https://a.example');
assert.equal(firstHttpNode({ nodes: [`iroh:${ID}`] }), '');
// entries with a tip URL
const withTip = { url: 'https://eboy.au', nodes: [{ url: 'https://eboy.au/', tip: 'https://eboy.au:16490/lc/tip' }, 'https://b.example', { url: 'https://c.example', tip: 'javascript:x' }, { url: 'https://d.example', tip: 'https://u@evil/lc/tip' }, { tip: 'https://x.example/lc/tip' }] };
assert.deepEqual(bootstrapEntries(withTip), ['https://eboy.au/', 'https://b.example', 'https://c.example', 'https://d.example', 'https://eboy.au']);
assert.equal(firstHttpNode(withTip), 'https://eboy.au');
assert.equal(bootstrapTip(withTip, 'https://eboy.au'), 'https://eboy.au:16490/lc/tip');
assert.equal(bootstrapTip(withTip, 'https://eboy.au/'), 'https://eboy.au:16490/lc/tip');
for (const n of ['https://b.example', 'https://c.example', 'https://d.example', 'https://other.example', '']) assert.equal(bootstrapTip(withTip, n), '', n);
assert.equal(bootstrapTip(null, 'https://eboy.au'), '');

// --- a fake wasm module
function fakeModule({ rpc, submit, allow = (m) => m !== 'stop_daemon', createFails = false } = {}) {
  const log = { created: null, rpcs: [], refreshes: 0, verifies: 0, bans: [] };
  class LightClient {
    static async create(boot, genesis, saved) { if (createFails) throw new Error('no wasm'); log.created = { boot: JSON.parse(boot), genesis, saved }; return new LightClient(); }
    static allowed(m) { return allow(m); }
    async rpc(m, p) { log.rpcs.push([m, JSON.parse(p)]); return rpc ? rpc(m, p) : JSON.stringify({ jsonrpc: '2.0', id: 0, result: { m } }); }
    async submit(m, p) { return submit(m, p); }
    async refresh() { log.refreshes++; }
    async verify() { log.verifies++; return JSON.stringify({ level: 'full', height: 7, at: 1 }); }
    list_json() { return '{"records":[]}'; }
    status() { return JSON.stringify({ now: 100, allowed: ['http:https://a.example'], list: { records: [
      { addr: { Http: { url: 'https://a.example' } }, source: 'Bootstrap', ok_rate: 0.9, ewma_latency_ms: 80, height: 7, strikes: 0, banned_until: 0 },
      { addr: { Http: { url: 'http://203.0.113.9:16481' } }, source: 'PublicNodes', ok_rate: 0.5, ewma_latency_ms: 0, height: 0, strikes: 3, banned_until: 500 } ] } }); }
    ban_liar(k) { log.bans.push(k); }
  }
  return { mod: { LightClient, default: async () => {} }, log };
}
const storage = () => { const m = new Map(); return { getItem: (k) => m.has(k) ? m.get(k) : null, setItem: (k, v) => m.set(k, v), m }; };
const timers = () => { const t = { every: [], later: [] }; return { t, setInterval: (f, ms) => { t.every.push([f, ms]); }, setTimeout: (f, ms) => { t.later.push([f, ms]); } }; };
const settle = () => new Promise((r) => setImmediate(r));

// --- before start / not ready: Unavailable, so rpc.js uses the single node
{
  const ln = createLightnet({ loadWasm: async () => fakeModule().mod, genesis: 'g', storage: storage(), ...timers() });
  await assert.rejects(ln.call('get_info'), (e) => e instanceof Unavailable && e.unavailable);
  assert.equal(ln.available('get_info'), false);
  assert.equal(ln.state(), 'idle');
}
// --- wasm fails to load / create: state failed, calls Unavailable, start never rejects
{
  const ln = createLightnet({ loadWasm: async () => { throw new Error('404 wasm'); }, storage: storage(), ...timers() });
  await ln.start({ url: 'https://a.example' });
  assert.equal(ln.state(), 'failed');
  assert.equal(ln.status().failure, '404 wasm');
  await assert.rejects(ln.call('get_info'), Unavailable);
  const ln2 = createLightnet({ loadWasm: async () => fakeModule({ createFails: true }).mod, storage: storage(), ...timers() });
  await ln2.start({});
  assert.equal(ln2.state(), 'failed');
}
// --- ready: calls go through, results unwrapped, node errors thrown (no fallback), saved list passed back in
{
  const st = storage(); st.setItem('sm64coin.lcnodes', 'SAVED');
  const { mod, log } = fakeModule({ rpc: (m) => m === 'get_block' ? JSON.stringify({ error: { code: -1, message: 'Block not found' } }) : JSON.stringify({ result: { ok: m } }) });
  const tm = timers();
  const ln = createLightnet({ loadWasm: async () => mod, genesis: 'GEN', storage: st, ...tm });
  const boot = { nodes: ['https://a.example'] };
  const p = ln.start(boot);
  assert.equal(ln.start(boot), p, 'start is idempotent');
  await p; await settle();
  assert.equal(ln.state(), 'ready');
  assert.deepEqual(log.created, { boot, genesis: 'GEN', saved: 'SAVED' });
  assert.deepEqual(await ln.call('get_info'), { ok: 'get_info' });
  assert.deepEqual(log.rpcs.at(-1), ['get_info', {}]);
  await ln.call('get_block_header_by_height', { height: 0 });
  assert.deepEqual(log.rpcs.at(-1), ['get_block_header_by_height', { height: 0 }]);
  await assert.rejects(ln.call('get_block', { height: 9 }), (e) => !e.unavailable && /get_block: Block not found/.test(e.message));
  await assert.rejects(ln.call('stop_daemon'), Unavailable, 'a method off the allowlist is not served here');
  // refresh + verify run at start and on timers; the list is persisted
  assert.ok(log.refreshes >= 1);
  assert.equal(tm.t.every.length, 2);
  assert.equal(st.m.get('sm64coin.lcnodes'), '{"records":[]}');
  tm.t.later.forEach(([f]) => f()); await settle();
  assert.equal(log.verifies, 1);
  const s = ln.status();
  assert.equal(s.verify.level, 'full');
  assert.deepEqual(s.nodes.map((n) => [n.kind, n.banned, n.verified]), [['https', false, true], ['http', true, false]]);
  ln.banLiar('http:https://a.example');
  assert.deepEqual(log.bans, ['http:https://a.example']);
}
// --- a transport failure of the network is Unavailable (the single node tries), not a node error
{
  const { mod } = fakeModule({ rpc: () => { throw 'every node failed: x'; } });
  const ln = createLightnet({ loadWasm: async () => mod, storage: storage(), ...timers() });
  await ln.start({}); await settle();
  await assert.rejects(ln.call('get_info'), (e) => e.unavailable && /every node failed/.test(e.message));
}
// --- submissions: fan-out answers, the first accepted one wins; all-rejected is a node error; none reached is Unavailable
{
  assert.ok(SUBMIT_METHODS.has('submit_block') && SUBMIT_METHODS.has('submit_sm64_proof'));
  const answer = (r) => ({ ok: true, node: 'n', answer: JSON.stringify(r) });
  const run = async (outs) => {
    const { mod } = fakeModule({ submit: () => JSON.stringify(outs) });
    const ln = createLightnet({ loadWasm: async () => mod, storage: storage(), ...timers() });
    await ln.start({}); await settle();
    return ln.call('submit_block', ['00']);
  };
  assert.deepEqual(await run([{ ok: false, node: 'a', error: 'down' }, answer({ result: { status: 'OK' } })]), { status: 'OK' });
  assert.deepEqual(await run([answer({ error: { message: 'stale' } }), answer({ result: { status: 'OK' } })]), { status: 'OK' });
  await assert.rejects(run([answer({ error: { message: 'block rejected' } }), { ok: false, node: 'b', error: 'down' }]), (e) => !e.unavailable && /submit_block: block rejected/.test(e.message));
  await assert.rejects(run([{ ok: false, node: 'a', error: 'down' }]), (e) => e.unavailable && /down/.test(e.message));
}
// --- storage that throws does not break anything
{
  const bad = { getItem() { throw new Error('denied'); }, setItem() { throw new Error('denied'); } };
  const { mod } = fakeModule();
  const ln = createLightnet({ loadWasm: async () => mod, storage: bad, ...timers() });
  await ln.start({}); await settle();
  assert.equal(ln.state(), 'ready');
}
console.log('lightnet ok');
