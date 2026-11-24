import React, { useEffect, useState } from 'react';
import { daemon } from './rpc.js';
import { latestRelease, isNewer, nodeLacksState, SITE_VERSION } from './updates.js';

// "Update available" notice (GitHub releases, checked from the browser, silent on any failure).
// A node older than 0.2.8 is detected by feature (get_info's version is Monero's, not ours): it
// answers get_sm64_state with "Method not found" (asked once per visit). The site's own version is baked into release builds
// (VITE_SM64COIN_VERSION; 'dev' and the GitHub Pages build never nag about themselves).
export default function UpdateBanner() {
  const [msg, setMsg] = useState(null);
  const [gone, setGone] = useState(false);

  useEffect(() => {
    let dead = false;
    const check = async () => {
      const rel = await latestRelease();
      const old = await nodeLacksState(daemon);
      if (dead) return;
      let text = null;
      if (old) {
        text = 'Your node is older than SM64Coin 0.2.8: some features fall back (update it).';
      } else if (rel && isNewer(rel.version, SITE_VERSION)) {
        text = `SM64Coin ${rel.version} is available.`;
      }
      setMsg(text && { text, url: rel?.url || 'https://github.com/Gumbo64/sm64coin/releases/latest', required: !old && rel?.required });
    };
    check();
    const t = setInterval(check, 6 * 3600 * 1000);
    return () => { dead = true; clearInterval(t); };
  }, []);

  if (!msg || gone) return null;
  return (
    <div className={`update-banner${msg.required ? ' required' : ''}`} role="status">
      {msg.required && <b>Update required. </b>}{msg.text}{' '}
      <a href={msg.url} target="_blank" rel="noreferrer">Release notes</a>
      <button className="close" onClick={() => setGone(true)} aria-label="Dismiss">×</button>
    </div>
  );
}
