import assert from 'node:assert/strict';
import { createLightnet, bootstrapEntries, firstHttpNode, hasIrohNodes, Unavailable, SUBMIT_METHODS } from './lightnet.js';

const ID = 'ab'.repeat(32);
// --- bootstrap parsing (public-node.json: nodes[] and the old url key)
assert.deepEqual(bootstrapEntries({ url: 'https://eboy.au' }), ['https://eboy.au']);
assert.deepEqual(bootstrapEntries({ nodes: ['https://a.example', `iroh:${ID}@https://relay.example`, 'ftp://x', 'iroh:zz', 'https://u@evil'], url: 'https://b.example' }),
  ['https://a.example', `iroh:${ID}@https://relay.example`, 'https://b.example']);
assert.deepEqual(bootstrapEntries(null), []);
assert.deepEqual(bootstrapEntries({ nodes: 'nope' }), []);
assert.equal(firstHttpNode({ nodes: [`iroh:${ID}`, 'https://a.example/'] }), 'https://a.example');
assert.equal(firstHttpNode({ nodes: [`iroh:${ID}`] }), '');
assert.equal(hasIrohNodes({ nodes: [`iroh:${ID}`] }), true);
assert.equal(hasIrohNodes({ url: 'https://x' }), false);

// --- a fake wasm module
function fakeModule({ rpc, submit, allow = (m) => m !== 'stop_daemon', createFails = false } = {}) {
  const log = { watches: [], created: null, rpcs: [], refreshes: 0, verifies: 0, bans: [] };
  class LightClient {
    static async create(boot, genesis, saved) { if (createFails) throw new Error('no wasm'); log.created = { boot: JSON.parse(boot), genesis, saved }; return new LightClient(); }
    static allowed(m) { return allow(m); }
    async rpc(m, p) { log.rpcs.push([m, JSON.parse(p)]); return rpc ? rpc(m, p) : JSON.stringify({ jsonrpc: '2.0', id: 0, result: { m } }); }
    async submit(m, p) { return submit(m, p); }
    async refresh() { log.refreshes++; }
    watch_tip(onTip, onEnd) { const w = { onTip, onEnd, stopped: 0, freed: 0, stop() { w.stopped++; }, free() { w.freed++; } }; log.watches.push(w); return w; }
    async verify() { log.verifies++; return JSON.stringify({ level: 'full', height: 7, at: 1 }); }
    list_json() { return '{"records":[]}'; }
    status() { return JSON.stringify({ now: 100, iroh: true, allowed: ['http:https://a.example'], list: { records: [
      { addr: { Http: { url: 'https://a.example' } }, source: 'Bootstrap', ok_rate: 0.9, ewma_latency_ms: 80, height: 7, strikes: 0, banned_until: 0 },
      { addr: { Iroh: { endpoint_id: ID, relay_url: 'https://r.example', direct: [] } }, source: 'Bootstrap', ok_rate: 0.5, ewma_latency_ms: 0, height: 0, strikes: 3, banned_until: 500 } ] } }); }
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
  assert.equal(s.iroh, true);
  assert.deepEqual(s.nodes.map((n) => [n.kind, n.banned, n.verified]), [['https', false, true], ['iroh', true, false]]);
  assert.equal(s.nodes[1].relay, 'https://r.example');
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
// --- tip push: not available before the network is ready; then the records arrive parsed, the end is reported, stop frees the watch
{
  const { mod, log } = fakeModule();
  const ln = createLightnet({ loadWasm: async () => mod, genesis: 'g', storage: storage(), ...timers() });
  assert.equal(ln.watchTip(() => {}, () => {}), null, 'not ready: the caller polls');
  await ln.start({}); await settle();
  const got = [], ended = [];
  const stop = ln.watchTip((t) => got.push(t), (why) => ended.push(why));
  assert.equal(typeof stop, 'function');
  const w = log.watches[0];
  w.onTip(JSON.stringify({ height: 7, hash: 'ab', timestamp: 5, difficulty: '1000', cumulative_difficulty: '9', server_time: 6, node: 'n' }));
  w.onTip('not json');
  w.onEnd('nopush');
  assert.deepEqual(got.map((t) => t.height), [7]);
  assert.deepEqual(ended, ['nopush']);
  stop();
  assert.deepEqual([w.stopped, w.freed], [1, 1]);
  // a module from before tip push (no watch_tip): null, so the store polls
  const old = fakeModule();
  delete old.mod.LightClient.prototype.watch_tip;
  const ln2 = createLightnet({ loadWasm: async () => old.mod, genesis: 'g', storage: storage(), ...timers() });
  await ln2.start({}); await settle();
  assert.equal(ln2.watchTip(() => {}, () => {}), null);
}
console.log('lightnet ok');
