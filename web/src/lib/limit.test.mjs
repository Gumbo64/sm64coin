// The time limit in the page is the Rust function: the vectors written from the Rust rules (pow/sm64pow/tests/limit_vectors.json,
// checked against sm64pow by its own test) must come out of the WASM identically, at every difficulty and timestamp in them.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { loadLimit } from './limit.testwasm.mjs';
import { maxFrames, dueElapsed, limitNow, validAtFor, windowFrames, maxRunFrames } from './limit.js';

if (!(await loadLimit())) { console.log('limit: skipped (perturb wasm not built: scripts/build-wasm.sh)'); process.exit(0); }

const rows = JSON.parse(readFileSync(new URL('../../../pow/sm64pow/tests/limit_vectors.json', import.meta.url), 'utf8'));
assert.ok(rows.length >= 100);
for (const [d, e, f] of rows) assert.equal(maxFrames(BigInt(d), e), f, `difficulty ${d}, ${e} s`);
assert.equal(maxRunFrames(), 36000);
// numbers, BigInts and numeric strings are the same difficulty; negative and fractional seconds clamp
assert.equal(maxFrames(12345678, 600), maxFrames(12345678n, 600));
assert.equal(maxFrames('12345678', 600), maxFrames(12345678n, 600));
assert.equal(maxFrames(1000, -5), maxFrames(1000, 0));
assert.equal(maxFrames(1000, 59.9), maxFrames(1000, 59));

// a tip: the limit grows second by second from its timestamp, the same as the node's get_info number at that second
const tip = { timestamp: 1_800_000_000, difficulty: 12345678n };
const at = [0, 1, 60, 61, 599, 600, 1200, 3600].map((s) => limitNow(tip, tip.timestamp + s));
for (let i = 1; i < at.length; i++) assert.ok(at[i] >= at[i - 1], 'the limit never shrinks while the tip stands');
assert.equal(at[0], maxFrames(12345678n, 0));
assert.equal(limitNow(null, 5), 0);
assert.equal(limitNow(tip, tip.timestamp - 100), at[0], 'a clock behind the block counts as the block second');

// when a run is valid: the first second the limit reaches it, never before the parent + 1 or the launch
for (const frames of [600, 1400, 3950, 12000, 36000]) {
  const v = validAtFor(tip, frames);
  assert.ok(v > tip.timestamp);
  assert.ok(limitNow(tip, v) >= frames && (v - 1 <= tip.timestamp || limitNow(tip, v - 1) < frames), `${frames} frames first valid at +${v - tip.timestamp}`);
}
assert.equal(validAtFor(tip, 36001), 0, 'longer than any block: never');
assert.equal(validAtFor(tip, 600, tip.timestamp + 99999), tip.timestamp + 99999, 'the chain launch is a floor');
assert.equal(dueElapsed(1, 36001), null);
assert.equal(windowFrames(1, 3950, 60), 4650, 'docs/V0.4-DESIGN.md: window_frames(1, 3950, 60)');
console.log('limit ok');
