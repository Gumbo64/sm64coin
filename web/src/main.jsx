import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App.jsx';
import { resolveNode } from './lib/rpc.js';
import { autoSync } from './lib/webminer/sync.js';
import { frameBuster } from './lib/security.js';
import './index.css';

// The game (emscripten) listens for keys on the whole window and cancels the
// ones it maps (WASD, I, J, ...), which would swallow typing in text fields.
// Keyboard events that aren't for the game are kept away from it: this
// capture-phase listener on window runs first, and stopping propagation there
// keeps the event from reaching the game's listener; typing itself (the
// default action) still happens.
const editable = (el) => el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable);
// The game's listeners also outlive the mining page (it is a single-page app),
// and it blocks shortcuts such as Ctrl+C; so keys only reach the game on the
// mining page, and never with Ctrl/Alt/Meta held (the game doesn't use them).
const keysForGame = (e) =>
  location.pathname.startsWith(`${import.meta.env.BASE_URL.replace(/\/+$/, '')}/mine`) && !editable(e.target) && !e.ctrlKey && !e.metaKey && !e.altKey;
for (const type of ['keydown', 'keyup', 'keypress']) {
  window.addEventListener(type, (e) => { if (!keysForGame(e)) e.stopPropagation(); }, true);
}

// A tab open across a deploy still runs the old build, whose lazily loaded pages (Wallet-<hash>.js, ...) the host no longer
// has: the import fails and the page goes blank. Vite reports that as 'vite:preloadError': load the new build instead,
// at most once a minute (so a real outage does not reload in a loop).
window.addEventListener('vite:preloadError', (e) => {
  const KEY = 'sm64coin.chunkReload';
  let last = 0;
  try { last = Number(sessionStorage.getItem(KEY) || 0); } catch { /* storage unavailable */ }
  if (Date.now() - last < 60000) return;
  try { sessionStorage.setItem(KEY, String(Date.now())); } catch { /* storage unavailable */ }
  e.preventDefault();
  location.reload();
});

// Your own node, else the public one (public-node.json) for this visit: decided before the
// first page asks the chain anything (at most a few seconds).
// Worker sync is on by default: the first visit of a session may reload once (before anything renders) so the page comes
// back cross-origin isolated (lib/webminer/sync.js).
// Framed by another page: stop before anything runs (the headers' frame-ancestors 'none' does the same where the host can send them).
if (!frameBuster()) autoSync().then((reloading) => { if (!reloading) resolveNode().finally(() => createRoot(document.getElementById('root')).render(<App />)); });
