// Debug hooks for end-to-end test scripts (window.__sm64miner, window.__miner, window.__player): present only in dev and in
// test builds (VITE_E2E=1 at build time, which the e2e scripts set), and then only when the page's URL has ?e2e or
// localStorage has 'sm64coin.e2e' set (to anything but '' or '0'). A production build has none of this: the controller
// is not reachable from a crafted link or from another script.
const env = (typeof import.meta !== 'undefined' && import.meta.env) || {};
const BUILD = env.DEV === true || env.VITE_E2E === '1';

export function e2e() {
  if (!BUILD) return false;
  try {
    if (new URLSearchParams(window.location.search).has('e2e')) return true;
    const v = window.localStorage.getItem('sm64coin.e2e');
    return v != null && v !== '' && v !== '0';
  } catch {
    return false;
  }
}

/** Sets `window[name] = value` when e2e() holds; returns `value`. */
export function e2eHook(name, value) {
  if (e2e()) window[name] = value;
  return value;
}

import { allMarks } from './perf.js';
if (e2e()) window.__sm64marks = allMarks;
