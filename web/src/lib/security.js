// Page-level security helpers (docs/WEB-SECURITY.md). Build targets:
//   legacy  = https://gumbo64.github.io/sm64coin/ (shared origin, no response headers): coi-sw.js, meta CSP, migration panel.
//   dedicated = the Cloudflare Pages build (VITE_DEDICATED=1): real headers from public-headers (_headers), no service worker.
const env = (typeof import.meta !== 'undefined' && import.meta.env) || {};

export const DEDICATED = env.VITE_DEDICATED === '1';
/** Where the app lives now (the legacy site links there). */
export const NEW_SITE_URL = (env.VITE_NEW_SITE_URL || 'https://app.eboy.au').replace(/\/+$/, '');

/** True when this page is inside a frame (clickjacking: Send and the passkey prompt must not sit under a decoy). */
export function framed(win = typeof window !== 'undefined' ? window : null) {
  if (!win) return false;
  try { return win.top !== win.self; } catch { return true; }
}

/**
 * Until every host sends `frame-ancestors 'none'` (GitHub Pages cannot), a framed page shows nothing but a link to itself.
 * Returns true when it blocked the page; the caller must not render the app.
 */
export function frameBuster(win = window, doc = document) {
  if (!framed(win)) return false;
  try {
    const root = doc.getElementById('root') || doc.body;
    root.textContent = '';
    const p = doc.createElement('p');
    p.style.cssText = 'font:16px system-ui;margin:2em;max-width:36em';
    p.append('SM64Coin does not run inside another page, to keep the wallet safe from look-alike overlays. ');
    const a = doc.createElement('a');
    a.href = win.location.href; a.target = '_blank'; a.rel = 'noopener noreferrer';
    a.textContent = 'Open it directly';
    p.append(a, '.');
    root.append(p);
  } catch { /* nothing more to do */ }
  return true;
}
