// Worker sync = a cross-origin-isolated page (SharedArrayBuffer for the workers' board). GitHub Pages can't send the headers,
// so public/coi-sw.js (a service worker) adds them and the page reloads once. This module enables / disables it and says
// what the state is and why, for the Auto-mine card's indicator.

import { DEDICATED } from '../security.js';

const KEY = 'sm64coin.coiReload'; // sessionStorage: {scrollY, enabled, at}: the reload came from the sync switch
const swUrl = () => {
  // credentialless is Chromium's and Firefox 119+'s; others (Safari) only know require-corp.
  const ua = navigator.userAgent;
  const ff = /Firefox\/(\d+)/.exec(ua);
  const chromium = /Chrome\/|Chromium\/|Edg\/|SamsungBrowser\//.test(ua) && !/Firefox\//.test(ua);
  const cred = chromium || (ff && +ff[1] >= 119);
  return `${import.meta.env.BASE_URL}coi-sw.js?coep=${cred ? 'credentialless' : 'require-corp'}`;
};
const scope = () => new URL(import.meta.env.BASE_URL, location.href).href;
const DIS = 'sm64coin.syncDisabled'; // localStorage: the user switched worker sync off
const AUTO = 'sm64coin.coiAuto'; // sessionStorage: this session already tried the automatic reload
const disabledByUser = () => { try { return localStorage.getItem(DIS) === '1'; } catch { return false; } };
const ss = (f) => { try { return f(sessionStorage); } catch { return null; } };

const OFF = `${import.meta.env.BASE_URL}coi-sw-off.txt`;
/** The remote kill file (see public/coi-sw.js): when it says "off", the worker is not registered and an existing one is removed. */
async function remotelyOff() {
  try {
    const r = await fetch(OFF, { cache: 'no-store' });
    if (!(r.ok && /^\s*off\b/i.test(await r.text()))) return false;
    for (const g of await navigator.serviceWorker.getRegistrations()) if (g.scope === scope()) await g.unregister();
    return true;
  } catch { return false; }
}

export const isolatedNow = () => typeof SharedArrayBuffer !== 'undefined' && globalThis.crossOriginIsolated === true;

/** {on, state: 'on'|'off'|'unsupported'|'pending'|'blocked', why}: derived from crossOriginIsolated, the service worker and feature checks. */
export async function syncStatus() {
  if (isolatedNow()) return { on: true, state: 'on', why: 'workers share their best checkpoints' };
  if (disabledByUser()) return { on: false, state: 'disabled', why: 'switched off by you: workers search separately' };
  if (!window.isSecureContext) return { on: false, state: 'unsupported', why: 'needs a secure (https) page' };
  if (!('serviceWorker' in navigator)) return { on: false, state: 'unsupported', why: 'not supported by this browser (no service workers; a private window can cause this)' };
  let reg = null;
  try { reg = await navigator.serviceWorker.getRegistration(scope()); } catch { /* blocked */ }
  const tried = ss((s) => s.getItem(KEY + '.tried'));
  if (reg && reg.active) {
    if (!navigator.serviceWorker.controller) return { on: false, state: 'pending', why: 'service worker not controlling this page yet: tap to retry' };
    return { on: false, state: 'blocked', why: 'the service worker is active but the browser did not isolate the page (it may not support COEP here): tap to retry, or disable' };
  }
  if (reg && (reg.installing || reg.waiting)) return { on: false, state: 'pending', why: 'service worker not active yet: tap to retry' };
  if (tried) return { on: false, state: 'blocked', why: 'your browser blocks service workers (a private window or strict tracking protection can do that): ' + tried };
  return { on: false, state: 'off', why: 'off: workers search separately, and clicking a node plays it after a short wait' };
}

async function until(cond, ms) {
  const t0 = Date.now();
  while (!cond() && Date.now() - t0 < ms) await new Promise((r) => setTimeout(r, 50));
  return cond();
}

/** Registers the service worker, waits until it controls the page, then reloads once (remembering where the user was). */
export async function enableSync() {
  try { localStorage.removeItem(DIS); } catch { /* private window */ }
  ss((s) => s.removeItem(KEY + '.tried'));
  try {
    const reg = await navigator.serviceWorker.register(swUrl(), { scope: scope() });
    try { await reg.update(); } catch { /* offline */ }
    await navigator.serviceWorker.ready;
    // The reload must happen once the worker controls the page (clients.claim), or the reload would not be isolated.
    await until(() => !!navigator.serviceWorker.controller, 4000);
  } catch (e) {
    ss((s) => s.setItem(KEY + '.tried', String(e && e.message || e)));
    return;
  }
  ss((s) => s.setItem(KEY, JSON.stringify({ scrollY: window.scrollY, enabled: true, at: Date.now() })));
  window.location.reload();
}

/** Unregisters the service worker and reloads (the page is not isolated afterwards). */
export async function disableSync() {
  try { localStorage.setItem(DIS, '1'); } catch { /* private window */ }
  try { for (const r of await navigator.serviceWorker.getRegistrations()) if (r.scope === scope()) await r.unregister(); } catch { /* ignore */ }
  ss((s) => { s.removeItem(KEY + '.tried'); s.setItem(KEY, JSON.stringify({ scrollY: window.scrollY, enabled: false, at: Date.now() })); });
  window.location.reload();
}

/** After a reload that came from the switch: the scroll position back, and what the user did ({enabled} or null). Once per reload. */
export function takeReloadNote() {
  const raw = ss((s) => s.getItem(KEY));
  if (!raw) return null;
  ss((s) => s.removeItem(KEY));
  try { return JSON.parse(raw); } catch { return null; }
}

/**
 * Called once at startup, before anything renders: worker sync is the DEFAULT. On a page that isn't isolated yet (and a browser
 * that can do it), registers the service worker and reloads ONCE, before the ROM prompt, the game or the miner load. At most one
 * automatic reload per session (no loops), none when the user disabled sync or the worker can't take control. Resolves true when it
 * is reloading (the caller renders nothing).
 */
export async function autoSync() {
  try {
    if (isolatedNow() || DEDICATED || disabledByUser() || !window.isSecureContext || !('serviceWorker' in navigator)) return false;
    if (ss((s) => s.getItem(AUTO))) return false;
    ss((s) => s.setItem(AUTO, '1'));
    if (await remotelyOff()) return false;
    const reg = await navigator.serviceWorker.register(swUrl(), { scope: scope() });
    await navigator.serviceWorker.ready;
    if (!(await until(() => !!navigator.serviceWorker.controller, 3000))) return false; // can't take control: fall back silently
    ss((s) => s.setItem(KEY, JSON.stringify({ scrollY: 0, enabled: true, auto: true, at: Date.now() })));
    window.location.reload();
    void reg;
    return true;
  } catch { return false; }
}
