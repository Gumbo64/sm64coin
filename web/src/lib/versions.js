// Version facts for the Info page: formatting and decoding, no network and no DOM.
import { nodeVersion, isNewer } from './updates.js';

export const REPO = 'https://github.com/Gumbo64/sm64coin';

// Monero's RPC version (get_version.version) packs major<<16 | minor: 196624 -> "3.16". null if it isn't one.
export function decodeRpcVersion(n) {
  if (!Number.isInteger(n) || n <= 0 || n > 0xffffffff) return null;
  return `${n >>> 16}.${n & 0xffff}`;
}

// A git sha as shown: the first 7 hex digits; null when it is not a sha ('' or unset in a local build).
export function shortSha(sha) {
  const s = String(sha ?? '').trim().toLowerCase();
  return /^[0-9a-f]{7,40}$/.test(s) ? s.slice(0, 7) : null;
}
export const commitUrl = (sha) => (shortSha(sha) ? `${REPO}/commit/${String(sha).trim().toLowerCase()}` : null);

// What the node says about itself. `info` = get_info result, `ver` = get_version result (either may be null).
// -> { version, known, label, rpc, gameBuild, rules }
//   known:   the node reports an SM64Coin release version ("dev" counts: a local build)
//   label:   the line to show
export function describeNode(info, ver) {
  const v = String(info?.sm64coin_version ?? ver?.sm64coin_version ?? '').trim();
  const rpc = decodeRpcVersion(ver?.version);
  const gameBuild = String(info?.sm64_game_build ?? ver?.sm64_game_build ?? '').trim() || null;
  const rules = String(info?.sm64_rules ?? ver?.sm64_rules ?? '').trim() || null;
  if (v) return { version: v, known: true, label: v, rpc, gameBuild, rules };
  const label = rpc ? `older than 0.3.3 (RPC ${rpc})` : 'older than 0.3.3';
  return { version: null, known: false, label, rpc, gameBuild, rules };
}

// Is a newer release out? For the node only when it reported a release version (nodeVersion also strips a "v").
export function updateAvailable(latest, current) {
  const c = nodeVersion(current) ?? current;
  return !!latest && isNewer(latest, c);
}

// "0.3.3" / "dev" -> display text ("dev" gets "(local build)")
export const versionText = (v) => (!v || v === 'dev' ? 'dev (local build)' : v.replace(/^v/, ''));
