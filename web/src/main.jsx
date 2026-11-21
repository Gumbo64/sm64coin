import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App.jsx';
import './index.css';

// The game (emscripten) listens for keys on the whole window and cancels the
// ones it maps (WASD, I, J, ...), which would swallow typing in text fields.
// While a text field has focus, keep keyboard events away from the game: this
// capture-phase listener on window runs first, and stopping propagation there
// keeps the event from reaching the game's listener; typing itself (the
// default action) still happens.
const editable = (el) => el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable);
for (const type of ['keydown', 'keyup', 'keypress']) {
  window.addEventListener(type, (e) => { if (editable(e.target)) e.stopPropagation(); }, true);
}

createRoot(document.getElementById('root')).render(<App />);
