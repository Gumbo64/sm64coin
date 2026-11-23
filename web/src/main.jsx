import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App.jsx';
import { resolveNode } from './lib/rpc.js';
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

// Your own node, else the public one (public-node.json) for this visit: decided before the
// first page asks the chain anything (at most a few seconds).
resolveNode().finally(() => createRoot(document.getElementById('root')).render(<App />));
