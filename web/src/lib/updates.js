// "Update available" check against GitHub's public releases API (unauthenticated, from the
// visitor's browser). Cached for 6 h, fails silently, never blocks anything.
// A release marks an update as required with a line `Update: required` in its body
// (see packaging/release-notes.md).
export const LATEST_URL = 'https://api.github.com/repos/Gumbo64/sm64coin/releases/latest';
export const CACHE_MS = 6 * 3600 * 1000;
const CACHE_KEY = 'sm64coin.latestRelease';

// "v0.2.8" / "0.2.8-rc1" -> [0, 2, 8] (a pre-release suffix is ignored); null if it isn't a version.
export function parseVersion(s) {
  const m = /^v?(\d+)\.(\d+)\.(\d+)/.exec(String(s ?? '').trim());
  return m ? [+m[1], +m[2], +m[3]] : null;
}

// A node's get_info `version` counts only if it is a 3-part SM64Coin release version
// ("0.2.7", "0.2.7-abc"); Monero's own 4-part "0.18.5.4-tag" or "" (restricted node) is ignored.
export function nodeVersion(s) {
  return /^v?\d+\.\d+\.\d+(?:-[^.]*)?$/.test(String(s ?? '').trim()) ? s.trim().replace(/^v/, '') : null;
}

// true if `latest` is newer than `current`; false when either isn't a release version ("dev").
export function isNewer(latest, current) {
  const a = parseVersion(latest), b = parseVersion(current);
  if (!a || !b) return false;
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i];
  return false;
}

export function isRequired(body) {
  return /^\s*Update:\s*required\s*$/im.test(String(body ?? ''));
}

const RELEASES = 'https://github.com/Gumbo64/sm64coin/releases/';
// The GitHub release JSON -> { version, url, required } or null. The link is rendered as an href: only a page of
// this repository's releases is used (never e.g. a javascript: URL from a tampered or cached response).
export function summarize(rel) {
  const v = parseVersion(rel?.tag_name);
  if (!v) return null;
  const url = typeof rel.html_url === 'string' && rel.html_url.startsWith(RELEASES) ? rel.html_url : `${RELEASES}latest`;
  return { version: v.join('.'), url, required: isRequired(rel.body) };
}

// The site's own version: the bundle's (VITE_SM64COIN_VERSION, set by the release workflow);
// 'dev' (no notice) otherwise.
export const SITE_VERSION = (typeof import.meta !== 'undefined' && import.meta.env && import.meta.env.VITE_SM64COIN_VERSION) || 'dev';

// Latest release summary, or null. Never throws.
export async function latestRelease(fetchFn = globalThis.fetch, store = globalThis.localStorage, now = Date.now()) {
  try {
    const c = JSON.parse(store?.getItem(CACHE_KEY) || 'null');
    if (c && now - c.at < CACHE_MS) return c.release;
  } catch { /* no storage */ }
  try {
    const r = await fetchFn(LATEST_URL, { headers: { Accept: 'application/vnd.github+json' } });
    if (!r.ok) return null;
    const release = summarize(await r.json());
    try { store?.setItem(CACHE_KEY, JSON.stringify({ at: now, release })); } catch { /* ignore */ }
    return release;
  } catch { return null; }
}

// Is the node older than 0.2.8? A node answers get_sm64_state (0.2.8+) with a state or a normal
// error ("end state isn't cached", bad hash); an older one with JSON-RPC "Method not found".
// Anything else (unreachable, forbidden, timeout) says nothing -> null.
export function stateMethodMissing(err) {
  const m = String(err?.message ?? err ?? '');
  if (/forbidden|not allowed|restricted|denied|unauthori[sz]ed/i.test(m)) return false;
  return /method not found|-32601/i.test(m);
}

// true: the node lacks get_sm64_state; false: it has it; null: unknown. Asked once per visit.
let stateProbe = null;
export function nodeLacksState(call) {
  stateProbe ??= (async () => {
    try { await call('get_sm64_state', {}); return false; } catch (e) {
      if (stateMethodMissing(e)) return true;
      return /unreachable|fetch|timeout|HTTP|Failed|NetworkError/i.test(String(e?.message ?? e)) ? null : false;
    }
  })();
  return stateProbe;
}
