import React, { useEffect, useRef, useState } from 'react';
import { useTip } from './tip.js';
import { coins } from './format.js';

// Top popups: green "+X SM64 mined" for your own block (see payout.js), and a
// short notice when anyone else's block lands (mining restarts on it; the
// mining page's help explains why).
export default function NewBlockBanner() {
  const [msg, setMsg] = useState(null);
  const top = useRef(null);
  const mine = useRef(new Set());
  const timer = useRef(null);

  const show = (m, ms) => {
    setMsg(m);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setMsg(null), ms);
  };

  useEffect(() => {
    const onMined = (e) => {
      mine.current.add(e.detail.height);
      show({ kind: 'mined', ...e.detail }, 12000);
    };
    window.addEventListener('sm64coin-mined', onMined);
    return () => window.removeEventListener('sm64coin-mined', onMined);
  }, []);

  // A new tip, by push (or the store's slow poll): not the first one seen, and not your own block.
  const tip = useTip();
  useEffect(() => {
    if (!tip) return;
    if (top.current && tip.hash !== top.current && !mine.current.has(tip.height)) show({ kind: 'block', height: tip.height }, 6000);
    top.current = tip.hash;
  }, [tip && tip.hash]);

  if (!msg) return null;
  if (msg.kind === 'mined') {
    return (
      <div className="banner mined" role="status">
        <span className="big">+{coins(msg.reward)} SM64</span> mined: block {msg.height} is yours!
        <button className="close" onClick={() => setMsg(null)}>×</button>
      </div>
    );
  }
  return (
    <div className="banner" role="status">
      New block {msg.height} mined. Mining has been restarted.
      <button className="close" onClick={() => setMsg(null)}>×</button>
    </div>
  );
}
