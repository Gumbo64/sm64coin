// The manual miner's flow after a finished run, as pure decisions (Mine.jsx drives them; mineflow.test.mjs checks them).
//
// v0.3 rules the flow follows: each attempt has its own block template with its own miner key, and a key signs ONE run
// (a second, different signed run for the same template is equivocation). A run longer than the current time limit is
// "pending" (kept, sent shortly before it is valid; signing happens at send time, so an unsent pending run may be replaced
// by a shorter one). The chain refuses blocks stamped before its launch (mainnet only).

/** Mainnet's launch (rules.rs launch_timestamp, 2026-11-26 00:00:00 UTC): no block may be stamped before it. */
export const LAUNCH_TIMESTAMP = 1795651200;
/** Seconds the player is put back after a run that is not final (adjustable on the page). */
export const REWIND_DEFAULT = 3;
export const REWIND_CHOICES = [1, 2, 3, 5, 10];
/** How long past its due time a sent block is looked for in the chain, seconds. */
export const CONFIRM_GRACE = 12;

/** The launch time that applies to a node: mainnet only (test networks run before it). `override` is the e2e test's. */
export function launchFor(info, override = 0) {
  if (override) return override;
  return info && info.mainnet ? LAUNCH_TIMESTAMP : 0;
}

/** When a run is valid: the node's answer (`sm64_valid_at`, 0 = now), never before the chain's launch. */
export const effectiveValidAt = (nodeValidAt, launch) => Math.max(nodeValidAt || 0, launch || 0);

/**
 * What to do with a finished run.
 *   used     this template's key already signed and sent a run (sent / accepted): practice only
 *   pending  the held run of the same parent, or null: {validAt, frames}
 * Returns {action: 'practice' | 'keep-pending' | 'hold' | 'send', replaces: bool}
 *   practice      nothing is sent (the template is spent); the player keeps playing
 *   keep-pending  the held run is valid sooner or ties and is not longer: this run is dropped
 *   hold          not valid yet (over the limit, or before launch): becomes the pending run (replacing the old one)
 *   send          valid now: sent at once (replacing a pending one)
 */
export function decideAfterWin({ now, validAt, frames, used = false, pending = null }) {
  if (used) return { action: 'practice', replaces: false };
  if (pending && (pending.validAt < validAt || (pending.validAt === validAt && pending.frames <= frames))) {
    return { action: 'keep-pending', replaces: false };
  }
  return { action: validAt > now ? 'hold' : 'send', replaces: !!pending };
}

/** The node's reason out of a thrown rpc.js error ("submit_block: <reason>" -> "<reason>"). */
export function refusalReason(e) {
  return String((e && e.message) || e || '').replace(/^(Error: )?submit_block: /, '') || 'refused';
}

// Refusals that the node (v0.3, core_rpc_server.cpp on_submitblock) gives BEFORE the block's claim is recorded
// (Blockchain::sm64_hold -> sm64coin::note_claim, after the signature check). Only after one of these may the same template sign
// another run: a recorded claim makes a second, different signed run for the template equivocation at that node, and
// the refusals after it (budget, queue full, too early, a proof that fails its replay, a generic "failed verification" that
// also covers an invalid proof, ...) leave the claim in place. Matched on the reason text; anything else, and every transport
// failure (timeout, HTTP error: the block may have arrived), keeps the key spent.
const PRE_CLAIM = [
  /^Wrong param$/, /^Wrong block blob$/, /^Block bloc size is too big/, /^Unknown parent block$/,
  /^Block is too old for restricted RPC$/, /^Busy: this address sent an invalid proof recently/,
  /^This command is unsupported for bootstrap daemon$/,
  /^Block not accepted: stamped \d+, before the chain's launch/, // checked in the RPC, before handle_block_found
];

/** Whether `e` is the node refusing a submission with a reason proven to come before it records the claim. */
export function refusedBeforeClaim(e) {
  const m = String((e && e.message) || e || '');
  if (!/^(Error: )?submit_block: /.test(m) || /^(Error: )?submit_block: HTTP \d+/.test(m)) return false;
  const why = refusalReason(e);
  return PRE_CLAIM.some((re) => re.test(why));
}

/**
 * Looks for a sent block in the chain until `dueAt + grace`.
 *   d.blockAt(height)  async: {prev_hash, blob} of the main chain's block at that height, or null (none yet; throw = hiccup)
 *   d.now()            seconds;  d.sleep(ms)
 *   t                  the template: {height, prev_hash}; keyHex: the miner key's public key (it is in the coinbase)
 * Resolves to {status: 'accepted'} | {status: 'lost', why} (another block holds the height) | {status: 'missing', why}.
 */
export async function confirmBlock(d, t, keyHex, dueAt, grace = CONFIRM_GRACE, every = 1000) {
  const give = dueAt + grace;
  for (;;) {
    let b = null;
    try { b = await d.blockAt(t.height); } catch { /* daemon hiccup: look again */ }
    if (b) {
      if (b.blob && b.blob.toLowerCase().includes(keyHex.toLowerCase())) return { status: 'accepted' };
      return { status: 'lost', why: `another block took height ${t.height} first` };
    }
    if (d.now() >= give) return { status: 'missing', why: `the node did not put it in the chain by ${new Date(give * 1000).toLocaleTimeString()}` };
    await d.sleep(every);
  }
}
