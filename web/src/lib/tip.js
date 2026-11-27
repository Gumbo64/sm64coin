// The chain tip, once per page. Everything that used to ask `get_info` on a timer (the Mine page, the auto-miner, the header
// light, the new-block banner, the explorer, the cycle timeline ...) reads this store instead; it learns the tip by PUSH where it
// can and by a slow poll where it cannot:
//   push  the light-client network's SubscribeTip stream over iroh (lib/lightnet.js, pow/lc-wasm), or the desktop app's `tip`
//         event (Rust: desktop/src-tauri/src/lctip.rs; the local node is polled there, once, for everyone). No request at all
//         while the tip stands.
//   poll  get_info every POLL_MS (30 s), at once again when the tab becomes visible or gets focus (a sleeping tab wakes up
//         with the right tip) and after a submit (`noteSubmit`).
// A tip is {height, hash, timestamp, difficulty, cumulativeDifficulty, serverTime?, at}: with the clock that is all the time limit
// needs (lib/limit.js: the Rust rules in WASM), so nothing polls for a growing limit any more.
//
// The store is plain JS with injected dependencies (tip.test.mjs drives it with fakes); `tip` is the page's instance, wired to
// rpc.js / lightnet.js / the platform lazily on first use.
import { useEffect, useState, useSyncExternalStore } from 'react';

export const POLL_MS = 30_000;
/** Polls closer together than this are one (focus + visibility fire together). */
export const MIN_REPOLL_MS = 1_500;
/** A push source that ended is tried again after this long. */
export const PUSH_RETRY_MS = 60_000;

const big = (v, d = 0n) => { try { return typeof v === 'bigint' ? v : BigInt(v); } catch { return d; } };

/** A tip from a `get_info` result and the tip block's timestamp. */
export function tipFromInfo(info, timestamp, at = Date.now()) {
  const wide = (k, plain) => (typeof info[k] === 'string' ? big(info[k]) : big(info[plain] ?? 0));
  return {
    height: Number(info.height) - 1, // get_info's height is the block count
    hash: String(info.top_block_hash || ''),
    timestamp: Number(timestamp) || 0,
    difficulty: wide('wide_difficulty', 'difficulty'),
    cumulativeDifficulty: wide('wide_cumulative_difficulty', 'cumulative_difficulty'),
    serverTime: Number(info.adjusted_time) || 0,
    source: 'poll',
    at,
  };
}

/** A tip from a pushed record (lc-wasm `watch_tip`, or the desktop's `tip` event: the same JSON shape). */
export function tipFromPush(j, at = Date.now()) {
  if (!j || typeof j.hash !== 'string' || !(Number(j.height) >= 0)) return null;
  return {
    height: Number(j.height),
    hash: j.hash,
    timestamp: Number(j.timestamp) || 0,
    difficulty: big(j.difficulty, 1n),
    cumulativeDifficulty: big(j.cumulative_difficulty ?? 0),
    serverTime: Number(j.server_time) || 0,
    source: 'push',
    at,
  };
}

/**
 * deps: { info() -> get_info result, header(hash) -> block timestamp, sources: [(handlers) => stop | null], onVisible(fn) -> off,
 *         now(), setTimeout, clearTimeout }
 * A source starts following the tip and calls `handlers.tip(json)` for each one and `handlers.end(why)` if it stops; it returns
 * its stop function, or null when it is not available (yet): `store.kick()` asks again.
 */
export function createTipStore(deps) {
  const now = deps.now || (() => Date.now());
  const later = deps.setTimeout || ((f, ms) => setTimeout(f, ms));
  const cancel = deps.clearTimeout || ((t) => clearTimeout(t));
  const listeners = new Set();
  let refs = 0;
  let tip = null;
  let info = null; // the last full get_info, for the tip it belongs to
  let infoFetch = null;
  let mode = 'idle'; // idle | push | poll
  let error = null;
  let pushStop = null;
  let pollTimer = null;
  let retryTimer = null;
  let offVisible = null;
  let inflight = null;
  let lastPoll = 0;
  let seq = 0;
  const stats = { polls: 0, pushes: 0, headers: 0, infos: 0 };

  const emit = () => listeners.forEach((f) => { try { f(tip); } catch { /* a listener must not break the store */ } });

  function apply(t) {
    if (!t) return false;
    if (tip && tip.hash === t.hash && tip.difficulty === t.difficulty) {
      tip = { ...tip, at: t.at, source: t.source }; // seen again: not a change (identity kept for useSyncExternalStore: only `at` moves)
      return false;
    }
    tip = t;
    seq++;
    emit();
    return true;
  }

  async function refresh(why = 'poll', force = false) {
    if (inflight) return inflight;
    if (!force && now() - lastPoll < MIN_REPOLL_MS) return undefined;
    lastPoll = now();
    stats.polls++;
    inflight = (async () => {
      try {
        const i = await deps.info();
        let ts = tip && tip.hash === i.top_block_hash ? tip.timestamp : null;
        if (ts === null) { stats.headers++; ts = await deps.header(i.top_block_hash); }
        info = i;
        error = null;
        apply(tipFromInfo(i, ts, now()));
      } catch (e) {
        error = e;
      } finally {
        inflight = null;
      }
    })();
    return inflight;
  }

  function schedulePoll() {
    cancel(pollTimer);
    pollTimer = later(async () => {
      pollTimer = null;
      if (refs > 0 && mode === 'poll') { await refresh('timer', true); schedulePoll(); }
    }, POLL_MS);
  }

  function startPoll() {
    mode = 'poll';
    refresh('start', true).then(() => { if (refs > 0 && mode === 'poll') schedulePoll(); });
    if (!offVisible && deps.onVisible) offVisible = deps.onVisible(() => { if (refs > 0 && mode === 'poll') refresh('visible'); });
  }

  const handlers = {
    tip: (j) => {
      const t = tipFromPush(j, now());
      if (t) { stats.pushes++; error = null; apply(t); }
    },
    end: () => {
      pushStop = null;
      if (refs > 0) {
        startPoll();
        cancel(retryTimer);
        retryTimer = later(() => { retryTimer = null; kick(); }, PUSH_RETRY_MS);
      }
    },
  };

  async function attachPush() {
    if (pushStop) return;
    for (const src of deps.sources || []) {
      let stop = null;
      try { stop = await src(handlers); } catch { stop = null; }
      if (stop && refs > 0 && !pushStop) {
        pushStop = stop;
        mode = 'push';
        cancel(pollTimer); pollTimer = null;
        cancel(retryTimer); retryTimer = null;
        return;
      }
      if (stop) { try { stop(); } catch { /* already stopped */ } }
    }
  }

  /** Looks again for a push source (one became available, e.g. the light-client network finished loading). */
  function kick() {
    if (refs > 0 && mode !== 'push') attachPush();
  }

  async function begin() {
    // Poll first when nothing pushes (and at once: the page needs a tip now); push replaces it as soon as a source answers.
    startPoll();
    await attachPush();
  }

  function end() {
    cancel(pollTimer); pollTimer = null;
    cancel(retryTimer); retryTimer = null;
    if (pushStop) { try { pushStop(); } catch { /* stopped */ } pushStop = null; }
    if (offVisible) { offVisible(); offVisible = null; }
    mode = 'idle';
  }

  return {
    /** The current tip (the same object until it changes), or null before the first one. */
    get: () => tip,
    /** Calls `fn(tip)` on each change; the first subscriber starts the following, the last stop ends it. `immediate`: also now. */
    subscribe(fn, { immediate = false } = {}) {
      listeners.add(fn);
      if (++refs === 1) begin();
      if (immediate && tip) { try { fn(tip); } catch { /* listener */ } }
      let done = false;
      return () => {
        if (done) return;
        done = true;
        listeners.delete(fn);
        if (--refs === 0) end();
      };
    },
    /** Resolves with the tip once there is one (the next, if `after` is a hash: a tip other than that one). */
    next(after = null, timeoutMs = 0) {
      return new Promise((resolve) => {
        if (tip && tip.hash !== after) return resolve(tip);
        let timer = null;
        const off = this.subscribe((t) => { if (t.hash !== after) { off(); cancel(timer); resolve(t); } });
        if (timeoutMs) timer = later(() => { off(); resolve(tip); }, timeoutMs);
      });
    },
    /** The tip, at most `maxAgeMs` old: while it is pushed, the current one (no request); polled, a poll if the last look is older. */
    async latest(maxAgeMs = 5000) {
      if (tip && (mode === 'push' || now() - tip.at <= maxAgeMs)) return tip;
      await refresh('latest', true);
      if (!tip && error) throw error;
      return tip;
    },
    /** The tip as of now: the followed one while something listens (it is pushed or at most POLL_MS old), else one fresh poll. */
    async fresh() {
      if (refs > 0 && tip) return tip;
      await refresh('fresh', true);
      if (!tip && error) throw error;
      return tip;
    },
    /** The full get_info for the current tip (kept per tip: one request per block at most, none in poll mode). */
    async info() {
      const t = tip;
      if (t && info && info.top_block_hash === t.hash) return info;
      if (!infoFetch) {
        stats.infos++;
        infoFetch = Promise.resolve(deps.info()).then((i) => { info = i; return i; }).finally(() => { infoFetch = null; });
      }
      return infoFetch;
    },
    /** Something was submitted (a block, a run): in poll mode look at once; push delivers the new tip by itself. */
    noteSubmit() { if (refs > 0 && mode === 'poll') refresh('submit', true); },
    /** Polls right now (a user's "refresh", a failed call that suggests the tip moved). */
    refresh: (force = true) => refresh('manual', force),
    kick,
    /** {mode, error, seq, ...counters}: how the tip is being followed. */
    status: () => ({ mode, error, seq, refs, ...stats }),
    /** Forgets everything (the node changed). */
    reset() {
      tip = null; info = null; error = null; inflight = null; lastPoll = 0;
      if (refs > 0) { end(); begin(); }
      listeners.forEach((f) => { try { f(null); } catch { /* listener */ } });
    },
  };
}

// ---- the page's instance -------------------------------------------------------------------------------------------------

let page = null;

/** The page's tip store, wired to rpc.js, lightnet.js and the platform. */
export function tipStore() {
  if (page) return page;
  const rpc = () => import('./rpc.js');
  page = createTipStore({
    info: async () => (await rpc()).daemon('get_info'),
    header: async (hash) => {
      const h = await (await rpc()).daemon('get_block_header_by_hash', { hash });
      return h.block_header.timestamp;
    },
    onVisible: (fn) => {
      if (typeof document === 'undefined') return null;
      const f = () => { if (!document.hidden) fn(); };
      document.addEventListener('visibilitychange', f);
      window.addEventListener('focus', f);
      return () => { document.removeEventListener('visibilitychange', f); window.removeEventListener('focus', f); };
    },
    sources: [
      // the desktop / Android app: Rust follows the tip (the local node, or the light-client network) and emits `tip` events
      async (h) => {
        const { getPlatform } = await import('../platform/index.js');
        const p = getPlatform();
        if (!p.tip) return null;
        return p.tip.subscribe((j) => h.tip(j));
      },
      // a browser tab on the light-client network: SubscribeTip over iroh
      async (h) => {
        const { lightnet } = await import('./lightnet.js');
        const ln = lightnet();
        if (!ln.onChangeOnce) ln.onChangeOnce = ln.onChange(() => page && page.kick());
        return ln.watchTip(h.tip, h.end);
      },
    ],
  });
  rpc().then((m) => m.onNodeChange && m.onNodeChange(() => page.reset())).catch(() => {});
  return page;
}

/** The current tip (null until known), re-rendering on each new block. Also keeps the store following while mounted. */
const subscribeStable = (fn) => tipStore().subscribe(fn);
const getStable = () => tipStore().get();
export function useTip() {
  return useSyncExternalStore(subscribeStable, getStable);
}

/** The full get_info of the current tip (one request per block at most; none while polling, which already has it), null until known. */
export function useTipInfo() {
  const t = useTip();
  const [info, setInfo] = useState(null);
  useEffect(() => {
    let live = true;
    if (!t) return undefined;
    tipStore().info().then((i) => { if (live) setInfo(i); }).catch(() => {});
    return () => { live = false; };
  }, [t && t.hash]);
  return info;
}
