import React, { useEffect, useState } from 'react';
import { daemon } from './rpc.js';
import { latestRelease, isNewer, nodeVersion, SITE_VERSION } from './updates.js';

// "Update available" notice (GitHub releases, checked from the browser, silent on any failure).
// The node's version comes from get_info; the site's own version is baked into release builds
// (VITE_SM64COIN_VERSION; 'dev' and the GitHub Pages build never nag about themselves).
export default function UpdateBanner() {
  const [msg, setMsg] = useState(null);
  const [gone, setGone] = useState(false);

  useEffect(() => {
    let dead = false;
    const check = async () => {
      const rel = await latestRelease();
      if (!rel || dead) return;
      let node = null;
      try { node = nodeVersion((await daemon('get_info')).version); } catch { /* node not reachable */ }
      if (dead) return;
      let text = null;
      if (node && isNewer(rel.version, node)) {
        text = `Your node (${node}) is older than SM64Coin ${rel.version}: some features fall back.`;
      } else if (isNewer(rel.version, SITE_VERSION)) {
        text = `SM64Coin ${rel.version} is available.`;
      }
      setMsg(text && { text, url: rel.url, required: rel.required });
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
