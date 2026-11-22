// The leaderboard computed from the chain alone: scan every coinbase, parse the
// opt-in tags (lib/lbTag.js, pow/sm64lb) and keep those whose coinbase really pays the
// tagged address (checked with the tag's view key: Monero's standard derivation, in
// WASM). Nothing is trusted from a server: a tag that does not verify is ignored.
// Results are cached per node in localStorage (verified entries only, plus the last
// block hashes for reorg detection), so a reload only scans new blocks.
import { daemonAt, getNode } from './rpc.js';
import init, { verify_tagged_coinbase } from '../wasm/lb/sm64lb_wasm.js';

const ready = init();
const CACHE = 'sm64coin.chainboard.v1';
const TAIL = 64; // block hashes kept to notice reorgs
const BATCH = 6;

const hexBytes = (h) => Uint8Array.from(h.match(/../g) || [], (x) => parseInt(x, 16));

/** Verify one `get_block` result: the entry {height, hash, name, address, reward, frames, ts} or null. */
export async function checkBlock(res) {
  await ready;
  const j = JSON.parse(res.json);
  const mt = j.miner_tx;
  const extra = Uint8Array.from(mt.extra || []);
  if (extra.length < 40) return null;
  const outs = mt.vout || [];
  const keys = new Uint8Array(32 * outs.length);
  const amounts = new Float64Array(outs.length);
  const tags = [];
  outs.forEach((o, i) => {
    const t = o.target.tagged_key || o.target.key_obj || o.target;
    keys.set(hexBytes(t.key || ''), 32 * i);
    amounts[i] = o.amount;
    tags.push(t.view_tag ? parseInt(t.view_tag, 16) : -1);
  });
  const vt = tags.every((x) => x >= 0) ? Uint8Array.from(tags) : new Uint8Array(0);
  const r = verify_tagged_coinbase(extra, keys, vt, amounts);
  if (!r) return null;
  const v = JSON.parse(r);
  return { height: res.block_header.height, hash: res.block_header.hash, name: v.name, address: v.address, reward: v.reward, frames: res.sm64_frames ?? null, ts: res.block_header.timestamp };
}

function load(node) {
  try {
    const c = JSON.parse(localStorage.getItem(`${CACHE}:${node}`) || 'null');
    if (c && c.v === 1) return c;
  } catch { /* corrupt */ }
  return { v: 1, genesis: '', tail: [], entries: [], next: 0 };
}
function save(node, c) {
  try { localStorage.setItem(`${CACHE}:${node}`, JSON.stringify(c)); } catch { /* full or blocked: just rescan next time */ }
}
export const clearBoardCache = () => { try { localStorage.removeItem(`${CACHE}:${getNode()}`); } catch { /* */ } };

/**
 * Bring the cache up to the chain tip. `onProgress(done, total, entries)` after each batch.
 * Returns {entries, scanned}.
 */
export async function syncBoard(onProgress = () => {}, shouldStop = () => false) {
  const node = getNode();
  const rpc = (m, p) => daemonAt(node, m, p);
  const info = await rpc('get_info');
  const tip = info.height; // block count
  let c = load(node);
  const hdr = async (h) => (await rpc('get_block_header_by_height', { height: h })).block_header.hash;
  const genesis = tip > 0 ? await hdr(0) : '';
  if (c.genesis !== genesis) c = { v: 1, genesis, tail: [], entries: [], next: 0 };
  // reorg: drop what sits above the last cached hash that is still on the chain
  let keep = Math.min(c.next, tip);
  while (c.tail.length && c.tail[c.tail.length - 1].h >= keep) c.tail.pop();
  while (c.tail.length) {
    const t = c.tail[c.tail.length - 1];
    if ((await hdr(t.h)) === t.hash) { keep = t.h + 1; break; }
    c.tail.pop();
  }
  if (!c.tail.length) keep = 0; // nothing verifiable left (or a fresh cache): rescan
  if (keep < c.next) c.entries = c.entries.filter((e) => e.height < keep);
  c.next = keep;
  const from = c.next;
  for (let h = from; h < tip && !shouldStop(); h += BATCH) {
    const hs = Array.from({ length: Math.min(BATCH, tip - h) }, (_, i) => h + i);
    const blocks = await Promise.all(hs.map((x) => rpc('get_block', { height: x })));
    for (const b of blocks) {
      const e = await checkBlock(b);
      if (e) c.entries.push(e);
      c.tail.push({ h: b.block_header.height, hash: b.block_header.hash });
    }
    c.tail = c.tail.slice(-TAIL);
    c.next = hs[hs.length - 1] + 1;
    if (((h - from) / BATCH) % 20 === 19) save(node, c);
    onProgress(c.next, tip, c.entries);
  }
  save(node, c);
  return { entries: c.entries, scanned: c.next, tip };
}

/** Group verified blocks by address into ranked rows (blocks, then rewards, then best frames). */
export function aggregate(entries) {
  const by = new Map();
  for (const e of [...entries].sort((a, b) => a.height - b.height)) {
    let r = by.get(e.address);
    if (!r) by.set(e.address, (r = { address: e.address, name: e.name, blocks: [], reward: 0n, bestFrames: null, last: 0 }));
    r.name = e.name; // the latest tag's name
    r.blocks.push(e.height);
    r.reward += BigInt(e.reward);
    if (e.frames != null && (r.bestFrames == null || e.frames < r.bestFrames)) r.bestFrames = e.frames;
    r.last = e.height;
  }
  return [...by.values()].sort((a, b) =>
    b.blocks.length - a.blocks.length || (b.reward > a.reward ? 1 : b.reward < a.reward ? -1 : 0) || (a.bestFrames ?? 1e12) - (b.bestFrames ?? 1e12));
}
