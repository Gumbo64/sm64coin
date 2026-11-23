// Submits a saved run (automine --save-block) the way the Mine page does: the valid time from
// rules.js maxFrames, the template restamped with proof.js withTimestamp, sealed with proof.js seal.
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
const blob = seal(withTimestamp(b.template_blob, validAt), hexToBytes(b.proof));
const r = await fetch(`${url}/json_rpc`, { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 0, method: 'submit_block', params: [blob] }) });
const j = await r.json();
console.log(`${file.split('/').pop()}: ${b.frames} frames, limit now ${maxFrames(d, now - b.parent_ts)}, stamped ${validAt} = now +${validAt - now} s: ${j.error ? 'ERROR ' + j.error.message : j.result.status}`);
