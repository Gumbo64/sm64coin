import assert from 'node:assert/strict';
import { tipStreamUrls, openSse, SSE_SILENCE_MS, createTipStore, tipFromInfo, tipFromPush, POLL_MS, MIN_REPOLL_MS, PUSH_RETRY_MS, PUSH_RETRY_MAX_MS, FALLBACK_JITTER_MS } from './tip.js';

// ---- parsing
{
  const t = tipFromInfo({ height: 8, top_block_hash: 'ab', wide_difficulty: '0x3e8', wide_cumulative_difficulty: '0x10', adjusted_time: 99 }, 1234, 5);
  assert.deepEqual([t.height, t.hash, t.timestamp, t.difficulty, t.cumulativeDifficulty, t.serverTime, t.source], [7, 'ab', 1234, 1000n, 16n, 99, 'poll']);
  const old = tipFromInfo({ height: 2, top_block_hash: 'cd', difficulty: 5, cumulative_difficulty: 9 }, 1);
  assert.deepEqual([old.difficulty, old.cumulativeDifficulty], [5n, 9n], 'a node without the wide fields');
  const p = tipFromPush({ height: 7, hash: 'ab', timestamp: 1234, difficulty: '1000', cumulative_difficulty: '16', server_time: 99, node: 'n' }, 5);
  assert.deepEqual([p.height, p.difficulty, p.cumulativeDifficulty, p.source], [7, 1000n, 16n, 'push']);
  assert.equal(tipFromPush({ hash: 3 }), null);
  assert.equal(tipFromPush(null), null);
}

// ---- a fake world: clock, timers, node
function world({ sources = [], visible = true, random = () => 0.5 } = {}) {
  const w = { t: 0, timers: [], calls: { info: 0, header: 0 }, chain: { height: 5, hash: 'h5', ts: 1000, difficulty: 1000 }, visibleFns: [], failInfo: false };
  w.advance = async (ms) => {
    const end = w.t + ms;
    for (;;) {
      const due = w.timers.filter((x) => x.at <= end).sort((a, b) => a.at - b.at)[0];
      if (!due) break;
      w.timers.splice(w.timers.indexOf(due), 1);
      w.t = due.at;
      due.f();
      await settle();
    }
    w.t = end;
    await settle();
  };
  w.mine = (hash, ts) => { w.chain = { height: w.chain.height + 1, hash, ts, difficulty: w.chain.difficulty }; };
  w.store = createTipStore({
    now: () => w.t,
    random,
    setTimeout: (f, ms) => { const x = { at: w.t + ms, f }; w.timers.push(x); return x; },
    clearTimeout: (x) => { const i = w.timers.indexOf(x); if (i >= 0) w.timers.splice(i, 1); },
    info: async () => { w.calls.info++; if (w.failInfo) throw new Error('down'); return { height: w.chain.height + 1, top_block_hash: w.chain.hash, wide_difficulty: `0x${w.chain.difficulty.toString(16)}`, wide_cumulative_difficulty: '0x1' }; },
    header: async (hash) => { w.calls.header++; assert.equal(hash, w.chain.hash); return w.chain.ts; },
    onVisible: (f) => { w.visibleFns.push(f); return () => { w.visibleFns = w.visibleFns.filter((x) => x !== f); }; },
    sources: sources.map((s) => (h) => s(w, h)),
  });
  return w;
}
const settle = async () => { for (let i = 0; i < 6; i++) await new Promise((r) => setImmediate(r)); };
const total = (w) => w.calls.info + w.calls.header;

// ---- poll mode: no push source
{
  const w = world();
  const seen = [];
  const off = w.store.subscribe((t) => seen.push(t.height));
  await settle();
  assert.equal(w.store.status().mode, 'poll');
  assert.deepEqual([w.calls.info, w.calls.header], [1, 1], 'first tip: get_info and the header');
  assert.equal(w.store.get().timestamp, 1000);
  assert.deepEqual(seen, [5]);
  // a quiet half hour: one get_info per POLL_MS, and no header while the tip stands
  await w.advance(30 * 60_000);
  assert.equal(w.calls.info, 1 + 30 * 60_000 / POLL_MS);
  assert.equal(w.calls.header, 1);
  assert.deepEqual(seen, [5], 'unchanged tip: no notification');
  // a block lands: seen at the next poll, one header more
  w.mine('h6', 1100);
  await w.advance(POLL_MS);
  assert.deepEqual(seen, [5, 6]);
  assert.equal(w.calls.header, 2);
  assert.equal(w.store.get().hash, 'h6');
  // the tab wakes up: an immediate poll, not a second one within MIN_REPOLL_MS
  await w.advance(5000);
  w.mine('h7', 1200);
  const before = w.calls.info;
  w.visibleFns.forEach((f) => f());
  await settle();
  assert.equal(w.calls.info, before + 1);
  assert.deepEqual(seen, [5, 6, 7]);
  w.visibleFns.forEach((f) => f());
  await settle();
  assert.equal(w.calls.info, before + 1, 'focus and visibility fire together: one poll');
  await w.advance(MIN_REPOLL_MS + 10);
  // after a submit: looked at once
  w.mine('h8', 1300);
  w.store.noteSubmit();
  await settle();
  assert.equal(w.store.get().hash, 'h8');
  // the full info of the tip is the poll's own: no extra request
  const i0 = w.calls.info;
  assert.equal((await w.store.info()).top_block_hash, 'h8');
  assert.equal(w.calls.info, i0);
  // the last unsubscribe stops everything
  off();
  assert.equal(w.timers.length, 0);
  assert.equal(w.visibleFns.length, 0);
  await w.advance(10 * POLL_MS);
  assert.equal(w.calls.info, i0, 'nobody listening: no requests');
}

// ---- a failing node: the error is kept, the last tip stays, polling goes on
{
  const w = world();
  w.store.subscribe(() => {});
  await settle();
  w.failInfo = true;
  await w.advance(POLL_MS);
  assert.ok(w.store.status().error);
  assert.equal(w.store.get().hash, 'h5');
  w.failInfo = false;
  await w.advance(POLL_MS);
  assert.equal(w.store.status().error, null);
}

// ---- push mode: no requests while the tip stands, every block delivered, latency = the push
{
  let handlers = null, stopped = 0;
  const w = world({ sources: [async (_w, h) => { handlers = h; return () => { stopped++; }; }] });
  const seen = [];
  w.store.subscribe((t) => seen.push([t.height, t.source]));
  await settle();
  assert.equal(w.store.status().mode, 'push');
  const startup = total(w); // the page's first tip came from the one startup poll (a push source may still be loading)
  assert.ok(startup <= 2);
  handlers.tip({ height: 6, hash: 'h6', timestamp: 1100, difficulty: '1000', cumulative_difficulty: '2', server_time: 5 });
  assert.deepEqual(seen.at(-1), [6, 'push']);
  // an hour with three blocks: only pushes, not one request
  for (let i = 0; i < 3; i++) {
    await w.advance(20 * 60_000);
    handlers.tip({ height: 7 + i, hash: `p${i}`, timestamp: 1200 + i, difficulty: '1000', cumulative_difficulty: '3', server_time: 5 });
  }
  assert.equal(total(w), startup, 'an hour of mining followed by push: no polls');
  assert.equal(w.store.get().height, 9);
  // the same tip pushed again (a reconnect) is not a change
  const n = seen.length;
  handlers.tip({ height: 9, hash: 'p2', timestamp: 1202, difficulty: '1000', cumulative_difficulty: '3', server_time: 6 });
  assert.equal(seen.length, n);
  // noteSubmit in push mode asks nothing: the push brings the new tip
  w.store.noteSubmit();
  await settle();
  assert.equal(total(w), startup);
  // the stream ends (the server went away): back to polling, and the source is tried again later
  handlers.end('closed');
  await settle();
  assert.equal(w.store.status().mode, 'poll');
  assert.equal(stopped, 0);
  await w.advance(PUSH_RETRY_MS * 1.25 + 10);
  assert.equal(w.store.status().mode, 'push', 'the push source came back');
}

// ---- a source that is not ready: polls first, attaches on kick()
{
  let ready = false, h0 = null;
  const w = world({ sources: [async (_w, h) => (ready ? ((h0 = h), () => {}) : null)] });
  w.store.subscribe(() => {});
  await settle();
  assert.equal(w.store.status().mode, 'poll');
  ready = true;
  w.store.kick();
  await settle();
  assert.equal(w.store.status().mode, 'push');
  const c = total(w);
  await w.advance(5 * POLL_MS);
  assert.equal(total(w), c, 'polling stopped when push took over');
  assert.ok(h0);
}

// ---- a source that throws, then a working second one
{
  const w = world({ sources: [async () => { throw new Error('no'); }, async (_w, h) => () => {}] });
  w.store.subscribe(() => {});
  await settle();
  assert.equal(w.store.status().mode, 'push');
}

// ---- next(): the first tip, then the next different one
{
  const w = world();
  const t = await w.store.next();
  assert.equal(t.hash, 'h5');
  w.mine('h6', 1);
  const p = w.store.next('h5');
  await w.advance(POLL_MS);
  assert.equal((await p).hash, 'h6');
  const pt = w.store.next('h6', 1000);
  await w.advance(1000);
  const timed = await pt;
  assert.equal(timed.hash, 'h6', 'timeout resolves with what there is');
}

// ---- info() in push mode: one get_info per tip, however many ask
{
  let handlers = null;
  const w = world({ sources: [async (_w, h) => { handlers = h; return () => {}; }] });
  w.store.subscribe(() => {});
  await settle();
  handlers.tip({ height: 6, hash: 'h6', timestamp: 1, difficulty: '1000', cumulative_difficulty: '1' });
  w.mine('h6', 1);
  const i0 = w.calls.info;
  const [a, b] = await Promise.all([w.store.info(), w.store.info()]);
  assert.equal(a, b);
  await w.store.info();
  assert.equal(w.calls.info, i0 + 1);
}

// ---- fresh(): a followed tip as it is; not followed: one poll (never a stale tip from before)
{
  const w = world();
  assert.equal((await w.store.fresh()).hash, 'h5');
  assert.equal(w.calls.info, 1);
  w.mine('h6', 5);
  assert.equal((await w.store.fresh()).hash, 'h6', 'nobody follows: asks again');
  const off = w.store.subscribe(() => {});
  await settle();
  const c = w.calls.info;
  assert.equal((await w.store.fresh()).hash, 'h6');
  assert.equal(w.calls.info, c, 'followed: no request');
  off();
}

// ---- reset (the node changed): forgets the tip and starts over
{
  const w = world();
  const seen = [];
  w.store.subscribe((t) => seen.push(t && t.hash));
  await settle();
  w.mine('other', 7);
  w.store.reset();
  await settle();
  assert.deepEqual(seen, ['h5', null, 'other']);
}
// ---- Server-Sent Events source 
{
  class FakeES {
    static last = null;
    constructor(url) { this.url = url; this.readyState = 0; this.listeners = {}; FakeES.last = this; }
    addEventListener(n, f) { this.listeners[n] = f; }
    close() { this.readyState = 2; this.closed = true; }
  }
  const w = world();
  const got = [], ends = [], unsupported = [];
  const h = { tip: (j) => got.push(j.hash), end: (why) => ends.push(why) };
  const deps = { EventSource: FakeES, setTimeout: (f, ms) => { const x = { at: w.t + ms, f }; w.timers.push(x); return x; }, clearTimeout: (x) => { const i = w.timers.indexOf(x); if (i >= 0) w.timers.splice(i, 1); }, onUnsupported: (u) => unsupported.push(u) };
  // a stream that works: records, keepalives re-arm the watchdog, silence ends it
  const stop = openSse('https://n.example/lc/tip', h, deps);
  const es = FakeES.last;
  assert.equal(es.url, 'https://n.example/lc/tip');
  es.readyState = 1; es.onopen();
  es.onmessage({ data: JSON.stringify({ height: 7, hash: 'aa', timestamp: 1, difficulty: '1', cumulative_difficulty: '1' }) });
  es.onmessage({ data: 'garbage' });
  assert.deepEqual(got, ['aa']);
  await w.advance(SSE_SILENCE_MS - 1000);
  es.listeners.ka(); // keepalive
  await w.advance(SSE_SILENCE_MS - 1000);
  assert.deepEqual(ends, [], 'keepalives hold it open');
  await w.advance(2000);
  assert.deepEqual(ends, ['silent']);
  assert.ok(es.closed);
  assert.deepEqual(unsupported, [], 'it had opened: not "unsupported"');
  stop();
  // the browser reconnecting (CONNECTING) is not an end; a refusal before open (404) is "unsupported" and ends
  const stop2 = openSse('https://m.example/lc/tip', h, deps);
  const es2 = FakeES.last;
  es2.readyState = 0; es2.onerror();
  assert.deepEqual(ends, ['silent']);
  es2.readyState = 2; es2.onerror();
  assert.deepEqual(ends, ['silent', 'unsupported']);
  assert.deepEqual(unsupported, ['https://m.example/lc/tip']);
  stop2();
}

// ---- the node's clock: pushed server times correct this device's clock (a phone 40 s fast sends a held run on time)
{
  let handlers = null;
  const w = world({ sources: [async (_w, h) => { handlers = h; return () => {}; }] });
  w.t = 1_800_000_000_000; // our clock (ms)
  w.store.subscribe(() => {});
  await settle();
  assert.equal(w.store.now(), w.t, 'nothing heard: our clock');
  // the server's clock is 40 s behind ours (this device is 40 s fast)
  handlers.tip({ height: 6, hash: 'h6', timestamp: 1, difficulty: '1000', cumulative_difficulty: '1', server_time: 1_800_000_000 - 40 });
  assert.ok(Math.abs(w.store.now() - (w.t - 40_000 + 500)) < 1, `node clock ${w.store.now() - w.t}`);
  await w.advance(10_000);
  assert.ok(Math.abs(w.store.now() - (w.t - 39_500)) < 1, 'the offset holds as time passes');
  // a keepalive's clock (SSE) updates it; within the tolerance it is taken as none
  handlers.clock(Math.floor(w.t / 1000));
  assert.equal(w.store.clockOffset(), 0);
  handlers.clock(0); // an old server's keepalive data ("1"): ignored
  assert.equal(w.store.clockOffset(), 0);
}

// ---- a push that ends: the fallback poll is jittered, retries back off (with jitter) and reset on a pushed tip
{
  let handlers = null, opens = 0;
  const w = world({ random: () => 0.9, sources: [async (_w, h) => { opens++; handlers = h; return () => {}; }] });
  w.store.subscribe(() => {});
  await settle();
  const i0 = w.calls.info;
  handlers.end('closed');
  await settle();
  assert.equal(w.calls.info, i0, 'no poll in the same instant');
  await w.advance(FALLBACK_JITTER_MS);
  assert.equal(w.calls.info, i0 + 1, 'polled after the jitter');
  // the retries: 60 s, 120 s, 240 s ... (x1.2 here), capped
  const gaps = [];
  for (let k = 0; k < 6; k++) {
    const t0 = w.t, o = opens;
    while (opens === o) await w.advance(1000);
    gaps.push(w.t - t0);
    handlers.end('closed');
    await settle();
  }
  assert.ok(gaps[1] > gaps[0] * 1.8 && gaps[2] > gaps[1] * 1.8, `backoff ${gaps}`);
  assert.ok(gaps.at(-1) <= PUSH_RETRY_MAX_MS * 1.25 + 1000, `capped ${gaps}`);
  // a pushed tip resets the backoff
  while (opens === 7) await w.advance(1000);
  handlers.tip({ height: 9, hash: 'z', timestamp: 1, difficulty: '1', cumulative_difficulty: '1' });
  const t0 = w.t, o = opens;
  handlers.end('closed');
  while (opens === o) await w.advance(1000);
  assert.ok(w.t - t0 <= PUSH_RETRY_MS * 1.25 + 1000, 'back to the first delay');
}

// ---- a source that is not there (SSE 404) moves on to the next one in seconds, not a minute
{
  let first = 0, second = 0;
  const w = world({ sources: [async (_w, h) => { if (first++ === 0) { setTimeout(() => h.end('unsupported')); return () => {}; } return null; }, async () => { second++; return null; }] });
  w.store.subscribe(() => {});
  await settle();
  await new Promise((r) => setTimeout(r, 5));
  await settle();
  await w.advance(3_100);
  assert.ok(second >= 1, 'the next source was tried within seconds');
}
console.log('tip ok');

// --- which URLs a node's stream is looked for at
assert.deepEqual(tipStreamUrls('https://eboy.au', 'https://eboy.au:16490/lc/tip'), ['https://eboy.au:16490/lc/tip', 'https://eboy.au/lc/tip']);
assert.deepEqual(tipStreamUrls('https://n.example', ''), ['https://n.example/lc/tip']);
