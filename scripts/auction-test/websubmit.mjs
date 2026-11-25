// Submits a saved run (automine --save-block) the way the Mine page does: the valid time from
// rules.js maxFrames, the template restamped with proof.js withTimestamp, sealed (v0.3: proof AND the
// miner key's signature) with proof.js seal = block.js seal. The saved run carries `sig`, the signature
// of its one run by the template's key (the seed excludes the timestamp, so restamping keeps it); a file
// from an older automine has only key_secret, and then the signature cannot be made here (no signer in
// node): re-save with a current automine. One run per key: this signs nothing new.
//   node websubmit.bundle.mjs SAVED.json DIFFICULTY DAEMON_URL
import { readFileSync } from 'node:fs';
import { maxFrames } from '../../web/src/lib/rules.js';
import { withTimestamp, seal, hexToBytes } from '../../web/src/lib/proof.js';
const [file, d, url] = process.argv.slice(2);
const b = JSON.parse(readFileSync(file, 'utf8'));
let e = 0;
while (maxFrames(d, e) < b.frames) e += 1;
const now = Math.floor(Date.now() / 1000);
// As the Mine page's submitRun: never before now, never before the run is valid.
const validAt = Math.max(b.parent_ts + e, now);
if (!b.sig) throw new Error(`${file}: no "sig" in the saved run (v0.3 saved runs carry the signature of their run by the template's key; re-save with a current automine --save-block)`);
const blob = seal(withTimestamp(b.template_blob, validAt), hexToBytes(b.proof), b.sig);
const r = await fetch(`${url}/json_rpc`, { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 0, method: 'submit_block', params: [blob] }) });
const j = await r.json();
console.log(`${file.split('/').pop()}: ${b.frames} frames, limit now ${maxFrames(d, now - b.parent_ts)}, stamped ${validAt} = now +${validAt - now} s: ${j.error ? 'ERROR ' + j.error.message : j.result.status}`);
