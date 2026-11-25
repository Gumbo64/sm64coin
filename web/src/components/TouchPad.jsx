import React, { useEffect, useRef, useState } from 'react';
import { BUTTONS, STICK_MAX, touchState, resetTouch, stickFromOffset } from '../lib/touchInput.js';
import './touchpad.css';

// On-screen N64 controller for phones and tablets: a floating analog stick (appears where the
// thumb lands, re-centres there: the "relative" mode of Mupen64Plus FZ / Delta) on the left, and
// A / B / Z / R / L / C / START on the right. Every touch is its own pointer (pointer capture),
// so stick + A + Z all work at once. It only writes lib/touchInput.js's state; the Recorder
// merges that into the pad it records, so touch runs are ordinary proofs.

const KEY = 'sm64coin.touch';
const load = () => {
  const d = { pad: null, auto: true, opacity: 0.6, size: 1, haptics: true };
  try { return { ...d, ...JSON.parse(localStorage.getItem(KEY) || '{}') }; } catch { return d; }
};
export const isTouchDevice = () => {
  try { return window.matchMedia('(pointer: coarse)').matches || 'ontouchstart' in window || navigator.maxTouchPoints > 0; } catch { return false; }
};

const STICK_R = 64; // px of thumb travel for full deflection (at size 1)

/** The saved touch settings: {auto (open the play view by itself on touch devices), opacity, size, haptics}. */
export function useTouchConfig() {
  const [cfg, setCfg] = useState(load);
  const set = (p) => setCfg((c) => { const n = { ...c, ...p }; try { localStorage.setItem(KEY, JSON.stringify(n)); } catch { /* private mode */ } return n; });
  return [cfg, set];
}

export function TouchSettings({ cfg, set }) {
  return (
    <div className="tp-settings">
      <label>Opacity <input type="range" min="0.2" max="1" step="0.05" value={cfg.opacity} onChange={(e) => set({ opacity: Number(e.target.value) })} /></label>
      <label>Size <input type="range" min="0.7" max="1.4" step="0.05" value={cfg.size} onChange={(e) => set({ size: Number(e.target.value) })} /></label>
      <label><input type="checkbox" checked={cfg.haptics} onChange={(e) => set({ haptics: e.target.checked })} /> Vibrate on press</label>
    </div>
  );
}

/** Releases every touch (buttons and stick): the controls call this whenever their touches may have vanished. */
export function useReleaseOnLayoutChange() {
  useEffect(() => {
    const rel = () => { resetTouch(); window.dispatchEvent(new Event('sm64touch-release')); };
    window.addEventListener('blur', rel);
    window.addEventListener('resize', rel);
    window.addEventListener('orientationchange', rel);
    document.addEventListener('visibilitychange', rel);
    return () => {
      window.removeEventListener('blur', rel); window.removeEventListener('resize', rel);
      window.removeEventListener('orientationchange', rel); document.removeEventListener('visibilitychange', rel);
    };
  }, []);
}

export function TouchOverlay({ cfg }) {
  useReleaseOnLayoutChange();
  const s = cfg.size;
  const buzz = () => { if (cfg.haptics && navigator.vibrate) { try { navigator.vibrate(8); } catch { /* unsupported */ } } };
  return (
    <div className="tp" data-testid="touchpad" style={{ '--tp-s': s, '--tp-op': cfg.opacity }} onContextMenu={(e) => e.preventDefault()}>
      <StickZone radius={STICK_R * s} buzz={buzz} />
      <div className="tp-right">
        <Btn id="START" cls="start" buzz={buzz}>START</Btn>
        <div className="tp-buttons">
          <Btn id="L" cls="l" buzz={buzz}>L</Btn>
          <Btn id="R" cls="r" buzz={buzz}>R</Btn>
          <Btn id="Z" cls="z" buzz={buzz}>Z</Btn>
          <Btn id="B" cls="b" buzz={buzz}>B</Btn>
          <Btn id="A" cls="a" buzz={buzz}>A</Btn>
          <Btn id="CU" cls="cu" buzz={buzz}>▲</Btn>
          <Btn id="CL" cls="cl" buzz={buzz}>◀</Btn>
          <Btn id="CR" cls="cr" buzz={buzz}>▶</Btn>
          <Btn id="CD" cls="cd" buzz={buzz}>▼</Btn>
        </div>
      </div>
    </div>
  );
}

// Buttons: several fingers may hold one (counted), the bit is set while any does.
function Btn({ id, cls, buzz, children }) {
  const held = useRef(new Set());
  const [down, setDown] = useState(false);
  const mask = BUTTONS[id];
  useEffect(() => {
    const f = () => { held.current.clear(); setDown(false); };
    window.addEventListener('sm64touch-release', f);
    return () => window.removeEventListener('sm64touch-release', f);
  }, []);
  const sync = () => {
    const d = held.current.size > 0;
    if (d) touchState.buttons |= mask; else touchState.buttons &= ~mask;
    setDown(d);
  };
  const press = (e) => {
    e.preventDefault(); e.stopPropagation();
    try { e.currentTarget.setPointerCapture(e.pointerId); } catch { /* synthetic pointer */ }
    if (!held.current.size) buzz();
    held.current.add(e.pointerId); sync();
  };
  const release = (e) => { e.preventDefault(); held.current.delete(e.pointerId); sync(); };
  useEffect(() => () => { touchState.buttons &= ~mask; }, [mask]);
  return (
    <div className={`tp-btn ${cls}${down ? ' down' : ''}`} role="button" aria-label={id} data-btn={id}
      onPointerDown={press} onPointerUp={release} onPointerCancel={release} onLostPointerCapture={release}>
      <span>{children}</span>
    </div>
  );
}

// The left half: the stick appears where the thumb lands and the thumb's offset from there is the
// stick (re-centres on every new touch; sliding past the radius drags the origin along so the
// thumb never "runs out" of travel).
function StickZone({ radius, buzz }) {
  const zone = useRef(null);
  const st = useRef(null); // {id, ox, oy}
  const [vis, setVis] = useState(null); // {ox, oy, kx, ky} in zone px
  const rel = (e) => { const r = zone.current.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };
  const move = (e) => {
    const g = st.current;
    if (!g || g.id !== e.pointerId) return;
    e.preventDefault();
    const [x, y] = rel(e);
    let dx = x - g.ox, dy = y - g.oy;
    const d = Math.hypot(dx, dy);
    if (d > radius * 1.15) { // drag the origin along with the thumb
      const k = (d - radius * 1.15) / d;
      g.ox += dx * k; g.oy += dy * k; dx = x - g.ox; dy = y - g.oy;
    }
    const [sx, sy] = stickFromOffset(dx, dy, radius);
    touchState.x = sx; touchState.y = sy;
    const dd = Math.hypot(dx, dy) || 1, c = Math.min(dd, radius) / dd;
    setVis({ ox: g.ox, oy: g.oy, kx: dx * c, ky: dy * c });
  };
  const down = (e) => {
    if (st.current) return; // one finger runs the stick
    e.preventDefault();
    try { e.currentTarget.setPointerCapture(e.pointerId); } catch { /* synthetic pointer */ }
    const [x, y] = rel(e);
    st.current = { id: e.pointerId, ox: x, oy: y };
    buzz();
    setVis({ ox: x, oy: y, kx: 0, ky: 0 });
    touchState.x = 0; touchState.y = 0;
  };
  const up = (e) => {
    if (!st.current || st.current.id !== e.pointerId) return;
    st.current = null; touchState.x = 0; touchState.y = 0; setVis(null);
  };
  useEffect(() => {
    const f = () => { st.current = null; setVis(null); };
    window.addEventListener('sm64touch-release', f);
    return () => { window.removeEventListener('sm64touch-release', f); touchState.x = 0; touchState.y = 0; };
  }, []);
  return (
    <div className="tp-left" ref={zone} data-testid="stickzone" data-max={STICK_MAX}
      onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onLostPointerCapture={up}>
      {vis
        ? <div className="tp-stick" style={{ left: vis.ox, top: vis.oy }}><div className="tp-knob" style={{ transform: `translate(${vis.kx}px, ${vis.ky}px)` }} /></div>
        : <div className="tp-ghost" />}
    </div>
  );
}
