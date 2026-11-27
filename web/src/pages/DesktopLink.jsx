import React, { useEffect, useState } from 'react';
import { parseLink, sendToApp } from '../lib/desktopLink.js';
import * as pk from '../lib/wallet/passkey.js';
import * as eng from '../lib/wallet/engine.js';

// Opened by the SM64Coin desktop app (Wallet > Use my website passkey): derives the passkey wallet with the existing, frozen
// passkey.js and sends its seed words to the app's one-time listener on this device, encrypted to the app's key
// (lib/desktopLink.js). Nothing else is done here and nothing leaves this computer.
export default function DesktopLink() {
  const [link, setLink] = useState(undefined); // undefined: not read yet; null: not a valid link
  const [state, setState] = useState('ready'); // ready | working | done | error
  const [msg, setMsg] = useState('');
  const [address, setAddress] = useState('');

  useEffect(() => {
    setLink(parseLink(location.hash));
    // the link's secret is not needed in the address bar or history any more
    try { history.replaceState(null, '', location.pathname); } catch { /* ignore */ }
  }, []);

  const go = async () => {
    setState('working'); setMsg('');
    try {
      setMsg('Choose your SM64Coin passkey…');
      const k = await pk.openPasskey();
      setMsg('Deriving the wallet…');
      // the seed words come from the passkey's spend key the same way the passkey wallet makes them (wallet2)
      const w = await eng.restoreFromSpendKey(k.spendKey, k.password, k.height);
      let keys;
      try { keys = await eng.allKeys(w); } finally { try { await w.close(false); } catch { /* closing */ } }
      if (!keys.seed) throw new Error('This wallet has no seed words.');
      setMsg('Sending it to the app…');
      await sendToApp(link, { seed: keys.seed, height: k.height || 0, address: keys.address });
      setAddress(keys.address);
      setState('done');
    } catch (e) {
      setState('error'); setMsg(e.message || String(e));
    }
  };

  if (link === undefined) return null;
  return (
    <div className="card">
      <h2>Use my passkey in the SM64Coin app</h2>
      {link === null && <p className="walleterr">This page is opened from the SM64Coin app (Wallet, "Use my website passkey"), with a link that carries a one-time key. Open it from there.</p>}
      {link && state !== 'done' && (
        <>
          <p>This hands your passkey wallet to the SM64Coin app running on <b>this device</b>. The wallet's seed words are encrypted to a key the app just made and sent straight to it (127.0.0.1); they go nowhere else. Your passkey itself never leaves this device.</p>
          <p className="muted">Only continue if you just clicked this in your own SM64Coin app.</p>
          <button disabled={state === 'working'} onClick={go}>Use my passkey</button>
          {msg && <p className={state === 'error' ? 'walleterr' : 'muted'}>{msg}</p>}
          {state === 'error' && <p className="muted">If your browser blocks sending to a local program, close this tab and restore from your seed words in the app instead.</p>}
        </>
      )}
      {state === 'done' && (
        <>
          <p>Sent. Go back to the app: it shows the address it received. Import it there only if it is this one:</p>
          <p className="mono break"><b>{address}</b></p>
        </>
      )}
    </div>
  );
}
