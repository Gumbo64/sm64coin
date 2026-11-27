import { useState } from 'react';
import useInterval from './useInterval.js';
import { getPlatform } from '../platform/index.js';

/** What the app's light-client verifier says about the chain it reads (Rust lcverify.rs, in `net.status().verify`):
 *  {state: 'checking'|'verified'|'conflict'|'unverified', label: 'verified (3 nodes agree)', agree, quorum, height, replay: {state, detail}, ...}.
 *  null where the platform has no light-client network (a plain browser visit until the site's own verifier lands). */
export function useVerify(ms = 10000) {
  const [v, setV] = useState(null);
  useInterval(async () => {
    const net = getPlatform().net;
    if (!net) return;
    try { const s = await net.status(); setV(s && s.verify ? s.verify : null); } catch { /* the app's network is not up yet */ }
  }, ms);
  return v;
}

/** One line for a tooltip: "Chain: verified (3 nodes agree)". */
export const verifyTitle = (v) => (v ? `Chain: ${v.label}${v.replay && v.replay.state !== 'off' ? `; replay: ${v.replay.detail || v.replay.state}` : ''}` : '');
