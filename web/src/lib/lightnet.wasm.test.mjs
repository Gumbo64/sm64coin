// The real pow/lc-wasm against local fake nodes over HTTP (fetch, failover, fan-out, allowlist, chain check). Needs the built
// wasm (scripts/build-lc-wasm.sh); skipped when it is not there.
import assert from 'node:assert/strict';
import http from 'node:http';
import { existsSync, readFileSync } from 'node:fs';

const dir = new URL('../wasm/lc/', import.meta.url);
if (!existsSync(new URL('sm64coin_lc_wasm_bg.wasm', dir))) { console.log('lightnet.wasm: skipped (lc wasm not built)'); process.exit(0); }

const G = '6b7164fc12bd735bfe29caf87615ec62ca318f78539afae11a13a244eb080482';
const node = (name, { bad = false, genesis = G } = {}) => new Promise((res) => {
  const s = http.createServer((q, r) => {
    let b = ''; q.on('data', (d) => { b += d; });
    q.on('end', () => {
      r.setHeader('access-control-allow-origin', '*');
      if (bad) { r.statusCode = 500; return r.end('boom'); }
      const j = JSON.parse(b);
      const result = j.method === 'get_block_header_by_height' ? { block_header: { hash: genesis } }
        : j.method === 'get_info' ? { height: 5, top_block_hash: '11'.repeat(32), wide_cumulative_difficulty: '0x5', node: name } : { node: name };
      r.end(JSON.stringify({ jsonrpc: '2.0', id: 0, result }));
    });
  }).listen(0, '127.0.0.1', () => res(s));
});
const url = (s) => `http://127.0.0.1:${s.address().port}`;
const down = await node('down', { bad: true }), b = await node('b'), c = await node('c'), old = await node('old', { genesis: '00'.repeat(32) });
const servers = [down, b, c, old];

const m = await import(new URL('sm64coin_lc_wasm.js', dir));
await m.default({ module_or_path: readFileSync(new URL('sm64coin_lc_wasm_bg.wasm', dir)) });
const lc = await m.LightClient.create(JSON.stringify({ nodes: servers.map(url) }), G, '');

assert.equal(m.LightClient.allowed('get_info'), true);
assert.equal(m.LightClient.allowed('stop_daemon'), false);
await lc.refresh(); // tips, and the node on another chain is banned
const seen = new Set();
for (let i = 0; i < 12; i++) {
  const j = JSON.parse(await lc.rpc('get_info', '{}'));
  seen.add(j.result.node);
}
assert.ok(!seen.has('down') && !seen.has('old'), `served only by healthy nodes on our chain: ${[...seen]}`);
assert.ok(seen.size >= 1);
await assert.rejects(lc.rpc('stop_daemon', '{}'), /not allowed|not supported/);
const outs = JSON.parse(await lc.submit('submit_block', '["00"]'));
assert.ok(outs.length >= 2 && outs.filter((o) => o.ok).length >= 1);
const st = JSON.parse(lc.status());
const rec = (n) => st.list.records.find((r) => r.addr.Http.url === url(n));
assert.ok(rec(old).banned_until > st.now, 'the other-chain node is banned');
assert.ok(rec(down).strikes >= 1 || rec(down).ok_rate < 0.5);
const v = JSON.parse(await lc.verify()); // the fake nodes serve no blocks: verification reports failure, it does not throw
assert.ok(['failed', 'none'].includes(v.level), JSON.stringify(v));
assert.ok(typeof lc.list_json() === 'string' && JSON.parse(lc.list_json()).records.length === 4);
// no tip push in the WASM any more: the page follows the tip by EventSource (lib/tip.js)
assert.equal(typeof lc.watch_tip, 'undefined');
servers.forEach((s) => s.close());
console.log('lightnet.wasm ok');
process.exit(0);
