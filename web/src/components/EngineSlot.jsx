import React, { useEffect, useRef } from 'react';

// ONE visual game serves the whole Mine page (manual play, the highlight reel, playing from a search node): a game engine's
// GL objects belong to its canvas, so the engine is created once (the first time something needs it) and its canvas moves
// between slots; it is never recreated. Whoever drives the game takes the engine's lock first (acquireEngine) and gives
// it back when done, so two drivers never step the same game at once.

let shared = null; // Promise of {engine, canvas}

export function getEngine() {
  shared ??= (async () => {
    const { createEngine } = await import('../lib/cycle.js');
    // The game build looks its canvas up in the document (by id) while it is created, so it starts in a hidden holder
    // and moves into the first slot that asks for it.
    const holder = document.createElement('div');
    holder.style.cssText = 'position:absolute;left:-10000px;top:0;width:640px;height:480px;visibility:hidden;pointer-events:none';
    const canvas = document.createElement('canvas');
    canvas.className = 'sm64canvas';
    holder.appendChild(canvas);
    document.body.appendChild(holder);
    try {
      const engine = await createEngine(canvas);
      return { engine, canvas };
    } finally { holder.remove(); }
  })();
  shared.catch(() => { shared = null; });
  return shared;
}

let tail = Promise.resolve();
/** Resolves with a `release()` once the engine's previous driver has released it. */
export function acquireEngine() {
  let release;
  const mine = new Promise((r) => { release = r; });
  const prev = tail;
  tail = prev.then(() => mine);
  return prev.then(() => release);
}

/** The game's canvas: mounted here while this component is on screen. `onEngine(engine)` when it is ready; `onError(msg)`. */
export default function EngineSlot({ onEngine, onError }) {
  const slot = useRef(null);
  useEffect(() => {
    let dead = false;
    getEngine().then(({ engine, canvas }) => {
      if (dead) return;
      slot.current.replaceChildren(canvas);
      onEngine(engine);
    }).catch((e) => { if (!dead && onError) onError(String(e.message || e)); });
    return () => {
      dead = true;
      if (shared) shared.then(({ engine }) => { try { engine.set_audio_enabled(0); } catch { /* gone */ } }, () => {});
      onEngine(null);
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return <div ref={slot} className="engine-slot" />;
}
