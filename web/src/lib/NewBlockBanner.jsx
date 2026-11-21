import React, { useEffect, useRef, useState } from 'react';
import { daemon } from './rpc.js';
import { coins } from './format.js';
import useInterval from './useInterval.js';

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

  useInterval(async () => {
    try {
      const i = await daemon('get_info');
      const h = i.top_block_hash;
      if (top.current && h !== top.current && !mine.current.has(i.height - 1)) {
        show({ kind: 'block', height: i.height - 1 }, 6000);
      }
      top.current = h;
    } catch { /* daemon hiccup */ }
  }, 2000);

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
