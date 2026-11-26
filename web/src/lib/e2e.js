// Debug hooks for end-to-end test scripts (window.__sm64miner, window.__miner, window.__player): installed only when
// the page's URL has ?e2e or localStorage has 'sm64coin.e2e' set (to anything but '' or '0'), never otherwise.
export function e2e() {
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
