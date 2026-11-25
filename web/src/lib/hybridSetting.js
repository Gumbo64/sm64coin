// "Play from the start": when on, manual mining ignores hybrid mining entirely (no search tree to play from,
// no clicking into nodes): the classic manual miner, always from the block's beginning. Remembered in
// localStorage; every page reads it through here, so the Auto-mine and Mine pages agree.
const KEY = 'sm64coin.playFromStart';
const subs = new Set();
let value = null;

export function getPlayFromStart() {
  if (value === null) {
    try { value = localStorage.getItem(KEY) === '1'; } catch { value = false; }
  }
  return value;
}

export function setPlayFromStart(on) {
  value = !!on;
  try { localStorage.setItem(KEY, value ? '1' : '0'); } catch { /* private window */ }
  subs.forEach((f) => f(value));
}

export function subscribePlayFromStart(fn) {
  subs.add(fn);
  return () => subs.delete(fn);
}
