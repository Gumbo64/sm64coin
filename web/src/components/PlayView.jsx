import React, { useEffect, useRef, useState } from 'react';
import { TouchOverlay, TouchSettings, useTouchConfig, isTouchDevice } from './TouchPad.jsx';
import { frames } from '../lib/format.js';
import './touchpad.css';

// The play view: on touch devices (or from the "Full screen" button) the game takes the whole screen,
// the way mobile emulators do. Landscape is the real layout (game centred at full height, the stick on
// the left, A/B/Z/R/L/C/START on the right, over the side bars); portrait puts the game on top and the
// controller below it, with a hint to rotate. The canvas is NOT moved: this wrapper is the game column,
// and the `imm` class turns it into a fixed full-viewport view, so the game keeps running through
// entering and leaving it.
//
// Reusable: it needs the recorder ref (`rec`, for pause), the recorder's status (`clock`:
// {frame, limit, paused}) and the canvas as `children` (first child), plus the normal page controls as
// `normal` (shown when not in the play view).
export default function PlayView({ rec, clock, limit, children, normal }) {
  const root = useRef(null);
  const [cfg, setCfg] = useTouchConfig();
  const [imm, setImm] = useState(false);
  const [hintGone, setHintGone] = useState(() => { try { return sessionStorage.getItem('sm64coin.rotatehint') === '1'; } catch { return false; } });
  const [settings, setSettings] = useState(false);
  const autoDone = useRef(false);
  const touch = isTouchDevice();
  const showPad = cfg.pad == null ? touch : cfg.pad; // the in-page controller under the game

  const enter = (gesture) => {
    setImm(true);
    const r = rec.current;
    if (r && r.paused) r.togglePause();
    const el = root.current;
    if (gesture && el && el.requestFullscreen) {
      Promise.resolve(el.requestFullscreen({ navigationUI: 'hide' })).then(() => {
        try { return screen.orientation.lock('landscape'); } catch { return null; }
      }).catch(() => { /* no element fullscreen (iOS Safari): the fixed view is the full screen */ });
    }
  };
  const leave = () => {
    setImm(false);
    try { if (document.fullscreenElement) document.exitFullscreen(); } catch { /* ignore */ }
    try { screen.orientation.unlock(); } catch { /* ignore */ }
  };
  const pauseMenu = () => { const r = rec.current; if (r && !r.paused) r.togglePause(); leave(); };

  // On a touch device open the view by itself, once, when the game starts.
  useEffect(() => {
    if (clock && touch && cfg.auto && !autoDone.current) { autoDone.current = true; enter(false); }
  }, [clock]); // eslint-disable-line react-hooks/exhaustive-deps
  // Leaving fullscreen by Esc / the back gesture keeps the (fixed) view; the Exit button leaves it.
  useEffect(() => {
    if (!imm) return undefined;
    const prev = document.documentElement.style.overflow;
    document.documentElement.style.overflow = 'hidden';
    return () => { document.documentElement.style.overflow = prev; };
  }, [imm]);

  const used = clock ? clock.frame : 0;
  const lim = clock ? clock.limit : limit;
  const over = lim && used > lim;
  const hideHint = () => { setHintGone(true); try { sessionStorage.setItem('sm64coin.rotatehint', '1'); } catch { /* private mode */ } };

  return (
    <div ref={root} className={`gamecol pv${imm ? ' imm' : ''}`} style={{ '--tp-s': cfg.size }}>
      {children}
      {!imm && showPad && <TouchOverlay cfg={cfg} />}
      {!imm && (
        <>
          {normal}
          <div className="tp-bar">
            <button className="on" onClick={() => enter(true)} title="Full-screen game with on-screen controls">⛶ Full screen{touch ? ' / touch controls' : ''}</button>
            <button className={showPad ? 'on' : ''} onClick={() => setCfg({ pad: !showPad })} title="Show the on-screen controller under the game">🎮 Controller: {showPad ? 'on' : 'off'}</button>
            <button onClick={() => setSettings((o) => !o)} aria-expanded={settings}>⚙ Touch settings</button>
          </div>
          {settings && <TouchSettings cfg={cfg} set={setCfg} />}
        </>
      )}
      {imm && (
        <>
          <div className="pv-strip" data-testid="strip">
            <button className="pv-btn" onClick={pauseMenu} aria-label="Pause and open the page controls">II</button>
            <div className={`pv-bar${over ? ' over' : ''}`}><div className="fill" style={{ width: `${lim ? Math.min(100, (used / lim) * 100) : 0}%` }} /></div>
            <span className="pv-text">{lim ? `${used}/${lim}` : used} · {lim ? (over ? `+${used - lim}` : `${frames(Math.max(0, lim - used))} left`) : ''}{clock && clock.paused ? ' · PAUSED' : ''}</span>
            <button className="pv-btn" onClick={leave} aria-label="Exit full screen">✕</button>
          </div>
          <TouchOverlay cfg={cfg} />
          {!hintGone && (
            <div className="pv-hint" role="note">
              <span>Rotate your phone for the full-screen controller</span>
              <button className="pv-x" onClick={hideHint} aria-label="Dismiss">✕</button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
