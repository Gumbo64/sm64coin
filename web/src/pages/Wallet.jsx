import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { getNode } from '../lib/rpc.js';
import { getPlatform } from '../platform/index.js';
import WalletDesktop from './WalletDesktop.jsx';
import { loadStored, saveStored, forgetStored } from '../lib/wallet/store.js';
import * as eng from '../lib/wallet/engine.js';
import { BLOCK_TARGET_SECONDS } from '../lib/rules.js';
import * as pk from '../lib/wallet/passkey.js';
import { DEDICATED, NEW_SITE_URL } from '../lib/security.js';
import { reencryptForMove } from '../lib/migrate.js';
import { newPassword, NO_PASSWORD_WARNING } from '../lib/wallet/pwRule.js';
import Identicon from '../components/Identicon.jsx';
import { miner } from '../lib/webminer/controller.js';
import { useMiner } from '../lib/webminer/useMiner.js';

// A lite wallet in the browser: the fork's wallet2 (WebAssembly, in a web worker) scans the chain
// through the node's RPC. Keys are made and used inside the worker; what is stored (IndexedDB) is the
// wallet's own password-encrypted keys file and cache. Nothing but node RPC calls leaves the browser.
const SYNC_MS = 15000;
const short = (a) => (a ? `${a.slice(0, 10)}…${a.slice(-8)}` : '');
const isHex64 = (k) => /^[0-9a-fA-F]{64}$/.test((k || '').trim());
const BK = (a) => `sm64coin.backedup.${a}`;
const isBackedUp = (a) => { try { return !!localStorage.getItem(BK(a)); } catch { return false; } };
const markBackedUp = (a) => { try { localStorage.setItem(BK(a), String(Date.now())); } catch { /* storage unavailable */ } };
const readFile = (f) => new Promise((resolve, reject) => {
  const r = new FileReader();
  r.onload = () => resolve(new Uint8Array(r.result));
  r.onerror = () => reject(r.error || new Error('could not read the file'));
  r.readAsArrayBuffer(f);
});
/** Save bytes/text as a download: a Blob and a temporary link, nothing is uploaded. */
const download = (name, data, type) => {
  const url = URL.createObjectURL(new Blob([data], { type }));
  const a = document.createElement('a');
  a.href = url; a.download = name;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
};
const copy = (t) => navigator.clipboard && navigator.clipboard.writeText(t);
const wrongPw = (e) => new Error(/password/i.test(e.message || '') ? 'Wrong password (or this is not a wallet file).' : e.message);
const isAddress = (a) => /^SM6[a-zA-Z0-9]{90,110}$/.test((a || '').trim());

// The desktop app has its own wallet (the wallet-rpc sidecar); the browser build uses the WASM engine below.
export default function Wallet() {
  return getPlatform().wallet ? <WalletDesktop /> : <WebWallet />;
}

function WebWallet() {
  const [stage, setStage] = useState('loading'); // loading | none | locked | open
  const [rec, setRec] = useState(null);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState('');
  const [w, setW] = useState(null);
  const [viewOnly, setViewOnly] = useState(false);
  const [address, setAddress] = useState('');
  const [snap, setSnap] = useState(null);
  const [progress, setProgress] = useState(null);
  const [seed, setSeed] = useState('');
  const wRef = useRef(null);
  const passkeyRef = useRef(false);
  const nopwRef = useRef(false); // made without a password: opens by itself
  const [nopw, setNopw] = useState(false);
  const [passkey, setPasskey] = useState(false);
  const syncing = useRef(false);
  const [engineErr, setEngineErr] = useState('');

  useEffect(() => { eng.loadLib().catch((e) => setEngineErr(e.message || String(e))); }, []);

  useEffect(() => {
    loadStored().then((r) => { setRec(r || null); setStage(r ? 'locked' : 'none'); }).catch(() => setStage('none'));
  }, []);

  const persist = useCallback(async (wallet, addr, vo) => {
    const data = await eng.exportData(wallet);
    await saveStored({ ...data, chain: await eng.chainId(), address: addr, viewOnly: vo, passkey: passkeyRef.current, nopw: nopwRef.current });
  }, []);

  const refresh = useCallback(async (wallet) => {
    setSnap(await eng.snapshot(wallet));
  }, []);

  const sync = useCallback(async () => {
    const wallet = wRef.current;
    if (!wallet || syncing.current) return;
    syncing.current = true;
    try {
      await eng.syncOnce(wallet, (pct) => setProgress(pct));
      await refresh(wallet);
      await persist(wallet, await wallet.getPrimaryAddress(), await wallet.isViewOnly());
      setErr('');
    } catch (e) {
      setErr(`sync: ${e.message || e}`);
    } finally {
      syncing.current = false;
      setProgress(null);
    }
  }, [refresh, persist]);

  const adopt = useCallback(async (wallet, fresh, viaPasskey, noPassword) => {
    passkeyRef.current = !!viaPasskey;
    nopwRef.current = !!noPassword; setNopw(!!noPassword);
    setPasskey(!!viaPasskey);
    wRef.current = wallet;
    setW(wallet);
    const addr = await wallet.getPrimaryAddress();
    const vo = await wallet.isViewOnly();
    setAddress(addr);
    setViewOnly(vo);
    // the Mine and Auto-mine pages read this to know where the reward goes
    try { localStorage.setItem('sm64coin.address', addr); } catch { /* storage unavailable */ }
    miner.noteWallet(addr, !!fresh); // the default mining address (a wallet just made or restored is what the visitor wants to mine for)
    if (fresh) await persist(wallet, addr, vo);
    setRec(await loadStored());
    setStage('open');
    sync();
  }, [persist, sync]);

  useEffect(() => {
    if (stage !== 'open') return undefined;
    const t = setInterval(sync, SYNC_MS);
    return () => clearInterval(t);
  }, [stage, sync]);

  useEffect(() => () => { const x = wRef.current; wRef.current = null; if (x) x.close(false).catch(() => {}); }, []);

  const run = async (label, fn) => {
    setErr(''); setBusy(label);
    try { await fn(); } catch (e) { setErr(e.message || String(e)); } finally { setBusy(''); }
  };

  const passwordSet = async (wallet) => { nopwRef.current = false; setNopw(false); await persist(wallet, address, viewOnly); };

  const lock = async () => {
    const x = wRef.current;
    wRef.current = null; setW(null); setSnap(null); setSeed('');
    if (x) { try { await persist(x, address, viewOnly); await x.close(false); } catch { /* closing */ } }
    setStage('locked');
  };

  const forget = async () => {
    if (!window.confirm('Remove this wallet from this browser? Without its seed the coins are gone.')) return;
    const x = wRef.current;
    wRef.current = null; setW(null); setSnap(null); setSeed('');
    if (x) { try { await x.close(false); } catch { /* closing */ } }
    await forgetStored();
    miner.forgetWallet(rec?.address || address);
    setRec(null); setStage('none');
  };

  if (stage === 'loading') return <div className="card"><p className="muted">Loading…</p></div>;

  return (
    <div className="wallet">
      <div className="card">
        <h2>Wallet</h2>
        <p className="muted">
          A light wallet that runs in this browser: your keys are created and used here, encrypted with your
          password, and never sent anywhere. It scans the chain through the node ({getNode() || 'this site\'s proxy'}, change it on the{' '}
          <Link to="/network">Network</Link> page).
        </p>
        {engineErr && <p className="walleterr">{engineErr}</p>}
        {err && <p className="walleterr">{err}</p>}
        {busy && <p className="muted">{busy}…</p>}
      </div>
      {stage === 'none' && <Onboard run={run} adopt={adopt} busy={!!busy} />}
      {stage === 'locked' && <Unlock rec={rec} run={run} adopt={adopt} forget={forget} busy={!!busy} />}
      {stage === 'open' && (
        <Open w={w} passkey={passkey} nopw={nopw} passwordSet={() => passwordSet(w)} address={address} viewOnly={viewOnly} snap={snap} progress={progress} seed={seed} setSeed={setSeed}
              sync={sync} lock={lock} forget={forget} run={run} refresh={() => refresh(wRef.current)} />
      )}
    </div>
  );
}

function Onboard({ run, adopt, busy }) {
  const [pkOk, setPkOk] = useState(false);
  const [hand, setHand] = useState(false); // Android: the website-passkey hand-off panel
  // The Android app's webview has no usable WebAuthn (and its origin, tauri.localhost, would make a different wallet than the
  // website's passkey anyway): no passkey wallet there, the seed words restore any wallet.
  const mobile = !!getPlatform().mobile;
  useEffect(() => { if (!mobile) pk.supported().then(setPkOk); }, [mobile]);
  const pkCreate = () => run('Creating the passkey wallet', async () => {
    const k = await pk.createPasskey();
    await adopt(await eng.restoreFromSpendKey(k.spendKey, k.password, k.height), true, true);
  });
  const pkOpen = () => run('Opening with the passkey', async () => {
    const k = await pk.openPasskey();
    await adopt(await eng.restoreFromSpendKey(k.spendKey, k.password, k.height), true, true);
  });
  const [mode, setMode] = useState('home'); // home: one click to a wallet; the rest restores one
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [seed, setSeed] = useState('');
  const [offset, setOffset] = useState('');
  const [height, setHeight] = useState('0');
  const [addr, setAddr] = useState('');
  const [vk, setVk] = useState('');
  const [sk, setSk] = useState('');
  const [keysFile, setKeysFile] = useState(null);
  const [cacheFile, setCacheFile] = useState(null);
  const fresh = mode === 'seed' || mode === 'keys' || mode === 'view';
  const np = newPassword(pw, pw2);
  const okPw = fresh ? np.ok : true; // a password is optional; an opened file/backup may have none

  const createNow = () => run('Creating the wallet', async () => { await adopt(await eng.createNew(''), true, false, true); });
  const go = () => run({ seed: 'Restoring the wallet', keys: 'Restoring the wallet', view: 'Restoring the wallet', backup: 'Opening the backup', desktop: 'Opening the desktop wallet' }[mode], async () => {
    const h = Math.max(0, parseInt(height, 10) || 0);
    let wallet;
    if (mode === 'seed') wallet = await eng.restoreFromSeed(seed, pw, h, { offset });
    else if (mode === 'keys') wallet = await eng.restoreFromKeys(addr, vk, sk, pw, h);
    else if (mode === 'view') wallet = await eng.restoreViewOnly(addr, vk, pw, h);
    else {
      const { keysData } = mode === 'backup' ? eng.parseBackup(await readFile(keysFile)) : { keysData: await readFile(keysFile) };
      const cache = mode === 'desktop' && cacheFile ? await readFile(cacheFile) : undefined;
      try { wallet = await eng.openFiles(keysData, cache, pw); } catch (e) { throw wrongPw(e); }
    }
    await adopt(wallet, true, false, !pw); // left empty (with the warning shown): a wallet without a password
  });

  const ready = okPw && (
    (mode === 'seed' && seed.trim().split(/\s+/).length === 25) ||
    (mode === 'keys' && isAddress(addr) && isHex64(vk) && isHex64(sk)) ||
    (mode === 'view' && isAddress(addr) && isHex64(vk)) ||
    ((mode === 'backup' || mode === 'desktop') && keysFile));
  const fileInput = (label, onPick, accept) => (
    <p><label className="muted">{label} <input type="file" accept={accept} onChange={(e) => onPick(e.target.files[0] || null)} /></label></p>
  );
  const canPk = (pkOk && !mobile) || (mobile && getPlatform().link);
  const pkNew = () => (mobile ? setHand(true) : pkCreate());
  if (mode === 'home') {
    return (
      <div className="card" data-testid="onboard-home">
        <p>
          <button className="ui-big" style={{ maxWidth: 320 }} disabled={busy} onClick={createNow}>Create wallet</button>
        </p>
        {canPk && (
          <p data-testid="passkey-box">
            <button disabled={busy} onClick={pkNew}>Create with passkey</button>
            {pkOk && !mobile && <>{' '}<button disabled={busy} onClick={pkOpen}>Open with passkey</button></>}
            <span className="muted small"> Opens only with your passkey, on this site; save the seed words.</span>
          </p>
        )}
        {hand && <MobileHandoff run={run} adopt={adopt} busy={busy} close={() => setHand(false)} />}
        <p className="small"><button className="linkish" onClick={() => setMode('seed')}>Restore a wallet…</button></p>
      </div>
    );
  }
  return (
    <div className="card">
      <div className="controls">
        <button className="linkish" onClick={() => setMode('home')}>◂ back</button>
        <button className={mode === 'seed' ? 'on' : ''} onClick={() => setMode('seed')}>Restore from seed</button>
        <button className={mode === 'keys' ? 'on' : ''} onClick={() => setMode('keys')}>Restore from private keys</button>
        <button className={mode === 'backup' ? 'on' : ''} onClick={() => setMode('backup')}>Load backup file</button>
        <button className={mode === 'desktop' ? 'on' : ''} onClick={() => setMode('desktop')}>Import desktop wallet</button>
        <button className={mode === 'view' ? 'on' : ''} onClick={() => setMode('view')}>View-only</button>
      </div>
      {mode === 'seed' && (
        <>
          <p><textarea className="wide mono" rows={3} placeholder="25 word seed" value={seed} onChange={(e) => setSeed(e.target.value)} /></p>
          <p className="muted">
            The language (English, Spanish, French, German, Italian, Portuguese, Dutch, Russian, Japanese, Chinese, Esperanto, Lojban) is detected from the words.<br />
            Seed offset (optional passphrase) <input type="password" value={offset} onChange={(e) => setOffset(e.target.value)} />
          </p>
        </>
      )}
      {(mode === 'keys' || mode === 'view') && (
        <>
          {mode === 'view'
            ? <p className="muted">A view-only wallet sees incoming payments (mined rewards too) but cannot spend. It cannot see outgoing payments either, so its balance also counts coins the full wallet has already spent.</p>
            : <p className="muted">The address and both private keys (as the desktop wallet's <code>viewkey</code> and <code>spendkey</code> commands print them). They must belong together.</p>}
          <p><input className="wide mono" placeholder="SM64… primary address" value={addr} onChange={(e) => setAddr(e.target.value)} /></p>
          <p><input className="wide mono" placeholder="view key (64 hex)" value={vk} onChange={(e) => setVk(e.target.value)} /></p>
          {mode === 'keys' && <p><input className="wide mono" type="password" placeholder="private spend key (64 hex)" value={sk} onChange={(e) => setSk(e.target.value)} /></p>}
        </>
      )}
      {mode === 'backup' && (
        <>
          <p className="muted">A file saved by "Download backup" (sm64coin-wallet-….json), or a raw wallet <code>.keys</code> file. It is read in this browser only.</p>
          {fileInput('Backup file', setKeysFile, '.json,.keys,application/json')}
        </>
      )}
      {mode === 'desktop' && (
        <>
          <p className="muted">The SM64Coin desktop wallet (GUI or CLI) writes <code>&lt;name&gt;.keys</code> and a cache file <code>&lt;name&gt;</code>. Pick the .keys file; the cache is optional (it only saves rescanning). Enter that wallet's own password.</p>
          {fileInput('The .keys file', setKeysFile, '.keys')}
          {fileInput('The cache file (optional, no extension)', setCacheFile, '')}
        </>
      )}
      {(mode === 'seed' || mode === 'keys' || mode === 'view') && (
        <p><label className="muted">Start scanning at block <input value={height} size={8} onChange={(e) => setHeight(e.target.value)} /> (0 is always right, a later block is faster)</label></p>
      )}
      <p>
        <input type="password" placeholder={fresh ? 'password (8+ characters, optional)' : 'the file\'s password (if any)'} value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
        {fresh && <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} />}
      </p>
      {fresh && (np.error ? <p className="walleterr" data-testid="pw-error">{np.error}</p> : np.warn ? <p className="muted small" data-testid="pw-warn">{np.warn}</p> : null)}
      <button disabled={!ready || busy} onClick={go}>{mode === 'backup' || mode === 'desktop' ? 'Open and save in this browser' : 'Restore'}</button>
      <p className="muted">Your keys stay in this browser. The password encrypts the wallet here only; it can't be recovered, but the seed words or a backup file always can.</p>
    </div>
  );
}

// Android: the system browser (Chrome) derives the passkey wallet and sends its seed to a one-time listener in this app
// (127.0.0.1, link.rs); the user compares the address, then the seed is restored into the WASM wallet here.
function MobileHandoff({ run, adopt, busy, close }) {
  const p = getPlatform();
  const [link, setLink] = useState(null); // {state, url, error}
  const [got, setGot] = useState('');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const np = newPassword(pw, pw2);
  const waiting = link && link.state === 'waiting';
  useEffect(() => {
    if (!waiting) return undefined;
    const t = setInterval(async () => {
      const s = await p.link.status().catch(() => null);
      if (!s) return;
      if (s.state === 'received') { clearInterval(t); setLink(null); setGot(s.address || ''); }
      else if (s.state !== 'waiting') { clearInterval(t); setLink({ state: s.state, error: s.error }); }
    }, 1500);
    return () => clearInterval(t);
  }, [waiting]); // eslint-disable-line react-hooks/exhaustive-deps
  const start = () => run('Opening your browser', async () => setLink(await p.link.start(true)));
  const cancel = async () => { await p.link.cancel().catch(() => {}); setLink(null); setGot(''); };
  const finish = () => run('Restoring the passkey wallet', async () => {
    const r = await p.link.take(got);
    const wallet = await eng.restoreFromSeed(r.seed, pw, r.height || 0, {});
    if ((await wallet.getPrimaryAddress()) !== got) { try { await wallet.close(false); } catch { /* closing */ } throw new Error('The restored wallet has another address than the one shown: not kept.'); }
    const none = !pw;
    setPw(''); setPw2('');
    await adopt(wallet, true, false, none);
  });
  return (
    <div>
      {!waiting && !got && (
        <>
          <p className="muted small">Choose a password for this app (optional), then open the browser.</p>
          <p><input type="password" placeholder="password (8+ characters, optional)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
          {np.error ? <p className="walleterr">{np.error}</p> : np.warn ? <p className="muted small">{np.warn}</p> : null}
          <button disabled={busy || !np.ok} onClick={start}>Open the browser</button>{' '}
          <button className="linkish" onClick={close}>cancel</button>
        </>
      )}
      {waiting && (
        <>
          {link.error && <p className="walleterr">{link.error}</p>}
          <p>Waiting for the browser. Use your passkey there, then come back to this app.</p>
          <p className="muted small">No browser? Copy: <span className="mono break">{link.url}</span> <button onClick={() => copy(link.url)}>Copy</button></p>
          <button onClick={cancel}>Cancel</button>
        </>
      )}
      {got && (
        <>
          <p>Received this wallet. Import it only if the browser page shows the same address:</p>
          <p className="mono break"><b>{got}</b></p>
          <button disabled={busy || !np.ok} onClick={finish}>It is the same: import</button>{' '}
          <button className="linkish" onClick={cancel}>cancel</button>
        </>
      )}
      {link && !waiting && link.state !== 'idle' && <p className="walleterr">{link.error || `The link ${link.state}.`}</p>}
    </div>
  );
}

function Unlock({ rec, run, adopt, forget, busy }) {
  const pkUnlock = () => run('Opening with the passkey', async () => {
    const k = await pk.openPasskey();
    let wallet;
    try { wallet = await eng.openStored(rec, k.password); } catch { throw new Error('That passkey does not open this wallet (pick the one you made it with).'); }
    await adopt(wallet, false, true);
  });
  const [pw, setPw] = useState('');
  const go = () => run('Unlocking', async () => {
    try { await adopt(await eng.openStored(rec, pw), false, false, !pw && !!rec?.nopw); }
    catch (e) { throw new Error(/password/i.test(e.message || '') ? 'Wrong password.' : e.message); }
  });
  // a wallet made without a password opens by itself
  useEffect(() => { if (rec?.nopw && !rec?.passkey) go(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="card">
      <h3>Unlock</h3>
      <p className="mono break">{rec ? <><Identicon address={rec.address} size={20} /> {short(rec.address)}</> : ''}{rec?.viewOnly ? ' (view-only)' : ''}</p>
      {rec?.passkey && <p><button disabled={busy} onClick={pkUnlock}>Open with passkey</button> <span className="muted">this wallet has no password: it is opened by its passkey</span></p>}
      {!rec?.passkey && <input type="password" autoFocus placeholder="password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && go()} />}{' '}
      {!rec?.passkey && <button disabled={busy} onClick={go}>Unlock</button>}{' '}
      <button className="linkish" onClick={forget}>forget this wallet</button>
    </div>
  );
}

function Open({ w, passkey, nopw, passwordSet, address, viewOnly, snap, progress, seed, setSeed, sync, lock, forget, run, refresh }) {
  const [backed, setBacked] = useState(() => isBackedUp(address));
  const [later, setLater] = useState(false);
  const done = () => { markBackedUp(address); setBacked(true); };
  const downloadBackup = () => run('Preparing the backup', async () => {
    const b = await eng.makeBackup(w);
    download(b.name, b.doc, 'application/json');
    done();
  });
  const downloadKeys = () => run('Preparing the keys file', async () => {
    const b = await eng.makeBackup(w);
    download(b.keysName, b.keys, 'application/octet-stream');
    done();
  });
  const showSeed = async () => {
    if (seed) { setSeed(''); return; }
    setSeed(viewOnly ? 'view-only wallet: no seed' : (await eng.allKeys(w)).seed || 'this wallet has no seed words (use Show keys)');
    if (!viewOnly) done();
  };
  return (
    <>
      {!backed && !later && (
        <div className="card notice">
          <h3>Back up your seed words</h3>
          {passkey && <p className="walleterr">Passkey wallet: it opens with your passkey, which only works on this website. If the passkey is deleted the coins are lost unless you save the seed words.</p>}
          <p>This wallet exists only in this browser. <b>Without a backup, clearing the browser's data (or switching browser or device) loses the coins.</b> Your keys stay here; a backup is a copy you keep yourself.</p>
          <div className="controls">
            <button onClick={showSeed}>{seed ? 'Hide seed words' : 'Show seed words'}</button>
            <button onClick={downloadBackup}>Download backup file</button>
            <button className="linkish" onClick={() => setLater(true)}>remind me later</button>
          </div>
          {seed && <p className="mono seedbox">{seed}</p>}
        </div>
      )}
      <div className="card">
        <div className="label">Primary address</div>
        <p className="mono break addr"><Identicon address={address} size={32} /> {address}</p>
        <div className="controls">
          <button onClick={() => navigator.clipboard && navigator.clipboard.writeText(address)}>Copy</button>
          {/* an explicit click here sets the payout address; a bare ?address= link never does (it asks first) */}
          <Link to="/mine" onClick={() => miner.setAddress(address)}><button>Mine to it</button></Link>
        </div>
        <p className="muted">The Mine page now pays its rewards to this address.</p>
        <p className="muted">Balances and history come from the node you are connected to and are trusted as it reports them (a remote node could show wrong ones; it cannot spend your coins).</p>
      </div>
      <div className="stats">
        <div className="stat"><div className="label">Balance</div><div className="value" title={snap ? eng.fmt(snap.balance) : undefined}>{snap ? eng.fmtShort(snap.balance) : '…'}</div></div>
        <div className="stat"><div className="label">Unlocked</div><div className="value" title={snap ? eng.fmt(snap.unlocked) : undefined}>{snap ? eng.fmtShort(snap.unlocked) : '…'}</div></div>
        <div className="stat"><div className="label">Scanned to</div><div className="value">{snap ? `${snap.height} / ${snap.daemon}` : '…'}</div></div>
        <div className="stat"><div className="label">Sync</div><div className="value">{progress != null ? `${Math.round(progress * 100)}%` : 'idle'}</div></div>
      </div>
      <ShowKeys w={w} run={run} onShown={done} passkey={passkey} nopw={nopw}>
        <button onClick={sync}>Sync now</button>
        <button onClick={downloadBackup}>Download backup</button>
        <button onClick={downloadKeys} title="the raw wallet2 .keys file, as the desktop wallet reads it">Download .keys</button>
        <button onClick={lock}>Lock</button>
        <button onClick={forget}>Forget</button>
      </ShowKeys>
      {!passkey && !viewOnly && <SetPassword w={w} nopw={nopw} run={run} done={passwordSet} />}
      {!DEDICATED && <Migrate w={w} run={run} passkey={passkey} address={address} />}
      {viewOnly && <p className="muted">View-only: spends made by the full wallet are not visible here, so this balance can be higher than the real one (change from a payment shows up as received).</p>}
      {!viewOnly && <p className="muted">To show this wallet's blocks under a name, join on the <Link to="/leaderboard">Leaderboard</Link> page.</p>}
      <Txs snap={snap} />
      {!viewOnly && <Send w={w} snap={snap} run={run} refresh={refresh} />}
    </>
  );
}

/** Set (or change) the wallet's password, later: nothing asks for one while the visitor plays or explores. */
function SetPassword({ w, nopw, run, done }) {
  const [open, setOpen] = useState(false);
  const [cur, setCur] = useState('');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const np = newPassword(pw, pw2);
  const ok = np.ok && !!pw;
  const save = () => run('Setting the password', async () => {
    try { await eng.setPassword(w, nopw ? '' : cur, pw); } catch (e) { throw new Error(/password/i.test(e.message || '') ? 'Wrong current password.' : e.message); }
    await done();
    setOpen(false); setCur(''); setPw(''); setPw2('');
  });
  return (
    <div className="card">
      <button className="linkish" onClick={() => setOpen(!open)}>{open ? '▾' : '▸'} {nopw ? 'Set a password' : 'Change password'}</button>
      {open && (
        <>
          {nopw && <p className="muted small">{NO_PASSWORD_WARNING}</p>}
          {!nopw && <p><input type="password" placeholder="current password" value={cur} onChange={(e) => setCur(e.target.value)} /></p>}
          <p><input type="password" placeholder="new password (8+ characters)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
          {pw || pw2 ? (np.error ? <p className="walleterr" data-testid="pw-error">{np.error}</p> : null) : null}
          <button disabled={!ok} onClick={save}>Save password</button>
        </>
      )}
    </div>
  );
}

/** The wallet's actions (`children`: sync, backups, lock, forget) and the keys, shown after a confirmation. */
function ShowKeys({ w, run, onShown, passkey, nopw, children }) {
  const [step, setStep] = useState(0); // 0 closed, 1 confirm + password, 2 shown
  const [pw, setPw] = useState('');
  const [k, setK] = useState(null);
  const close = () => { setStep(0); setPw(''); setK(null); };
  const reveal = () => run('Checking the password', async () => {
    if (passkey) { const k = await pk.openPasskey(); await eng.checkPassword(w, k.password); } else await eng.checkPassword(w, pw);
    setK(await eng.allKeys(w)); setPw(''); setStep(2); onShown();
  });
  const row = (label, v, secret) => v ? (
    <p><span className="label">{label}</span><br /><span className={`mono break${secret ? ' seedbox' : ''}`}>{v}</span>{' '}
      <button onClick={() => copy(v)}>Copy</button></p>
  ) : null;
  return (
    <div className="card">
      <div className="controls">
        {step === 0 && <button onClick={() => setStep(1)}>Show keys…</button>}
        {children}
      </div>
      {step === 1 && (
        <>
          <p className="walleterr">Anyone who sees the spend key or the seed words can spend all of this wallet's coins. Show them only where nobody can look over your shoulder, and never paste them into a website or send them to anyone.</p>
          {!passkey && !nopw && <><input type="password" autoFocus placeholder="wallet password" value={pw} onChange={(e) => setPw(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && reveal()} />{' '}</>}
          <button onClick={reveal}>I understand, show the keys</button>{' '}
          <button className="linkish" onClick={close}>cancel</button>
        </>
      )}
      {step === 2 && k && (
        <>
          <p className="walleterr">Secret: whoever has the spend key or the seed can spend your coins.</p>
          {row('Seed words (25)' + (k.seedLanguage ? `, ${k.seedLanguage}` : ''), k.seed, true)}
          {!k.seed && <p className="muted">This wallet has no seed words (it was made from keys that are not derived from a seed, or is view-only).</p>}
          {row('Private spend key', k.spendKey, true)}
          {row('View key', k.viewKey, true)}
          {row('Public spend key', k.publicSpendKey)}
          {row('Public view key', k.publicViewKey)}
          {row('Address', k.address)}
          <button onClick={close}>Hide</button>
        </>
      )}
    </div>
  );
}

function Txs({ snap }) {
  const rows = snap ? snap.rows : [];
  return (
    <div className="card">
      <h3>Transactions</h3>
      {rows.length === 0 ? <p className="muted">Nothing yet. Rewards appear once their block is scanned.</p> : (
        <table>
          <thead><tr><th>Block</th><th>Kind</th><th>Amount</th><th>Status</th><th>Tx</th></tr></thead>
          <tbody>
            {rows.map((r) => {
              const net = (r.incoming ?? 0n) - (r.outgoing ?? 0n);
              return (
                <tr key={r.hash}>
                  <td>{r.height ?? 'pool'}</td>
                  <td>{r.miner ? '★ mined reward' : net >= 0n ? 'received' : 'sent'}</td>
                  <td className="mono">{net >= 0n ? '+' : '-'}{eng.fmt(net >= 0n ? net : -net)}{net < 0n && r.fee ? ` (+${eng.fmt(r.fee)} fee)` : ''}</td>
                  <td>{r.failed ? 'failed' : !r.confirmed ? 'pending' : r.locked ? (r.unlockIn > 0 ? `unlocks in ${r.unlockIn} block${r.unlockIn === 1 ? '' : 's'} (about ${r.unlockIn * BLOCK_TARGET_SECONDS >= 7200 ? `${Math.round(r.unlockIn * BLOCK_TARGET_SECONDS / 3600)} h` : `${Math.round(r.unlockIn * BLOCK_TARGET_SECONDS / 60)} min`})` : 'unlocking') : 'unlocked'}</td>
                  <td className="mono">{r.hash && <Link to={`/block/${r.height}`} title={r.hash}>{r.hash.slice(0, 10)}…</Link>}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

function Send({ w, snap, run, refresh }) {
  const [to, setTo] = useState('');
  const [amt, setAmt] = useState('');
  const [draft, setDraft] = useState(null);
  const [sent, setSent] = useState('');
  const atomic = eng.parseAmount(amt);
  const ok = isAddress(to) && atomic && atomic > 0n;

  const prepare = () => run('Building the transaction', async () => {
    setSent('');
    const tx = await w.createTx({ accountIndex: 0, address: to.trim(), amount: atomic, relay: false });
    setDraft({ tx, fee: tx.getFee(), amount: atomic, to: to.trim() });
  });
  const relay = () => run('Sending', async () => {
    const hash = await w.relayTx(draft.tx);
    setSent(hash); setDraft(null); setTo(''); setAmt('');
    await refresh();
  });

  return (
    <div className="card">
      <h3>Send</h3>
      <p><input className="wide mono" placeholder="SM64… recipient address" value={to} onChange={(e) => { setTo(e.target.value); setDraft(null); }} /></p>
      {isAddress(to) && <p className="muted small" data-testid="to-preview"><Identicon address={to.trim()} size={24} /> {to.trim().slice(0, 8)}…{to.trim().slice(-6)}: check it is the right one.</p>}

      <p>
        <input placeholder="amount" value={amt} size={16} onChange={(e) => { setAmt(e.target.value); setDraft(null); }} />{' '}
        <span className="muted" title={snap ? eng.fmt(snap.unlocked) : undefined}>spendable {snap ? eng.fmtShort(snap.unlocked) : '…'}</span>
      </p>
      {!draft && <button disabled={!ok} onClick={prepare}>Review</button>}
      {draft && (
        <div className="notice">
          Send <b>{eng.fmt(draft.amount)}</b> to <span className="mono break">{draft.to}</span>, network fee <b>{eng.fmt(draft.fee)}</b>.<br />
          <button onClick={relay}>Confirm and send</button>{' '}
          <button onClick={() => setDraft(null)}>Cancel</button>
        </div>
      )}
      {sent && <p>Sent: <span className="mono break">{sent}</span></p>}
    </div>
  );
}

const download2 = download;
/** Legacy origin only: move the wallet (and the miner's signed-seed memory) to the dedicated origin. */
function Migrate({ w, run, passkey, address }) {
  const s = useMiner();
  const [open, setOpen] = useState(false);
  const [cur, setCur] = useState('');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [res, setRes] = useState(null);
  const ok = pw.length >= 8 && pw === pw2 && (passkey || cur);
  const prepare = () => run('Preparing the move', async () => {
    if (passkey) { const k = await pk.openPasskey(); await eng.checkPassword(w, k.password); } else await eng.checkPassword(w, cur);
    const r = await reencryptForMove(eng, w, pw);
    setRes(r); setCur(''); setPw(''); setPw2('');
    download2(r.name, r.doc, 'application/json');
    markBackedUp(address);
  });
  return (
    <div className="card" data-testid="migrate-card">
      <h3>Move to the new site</h3>
      {!open ? (
        <>
          <p className="muted">The app now lives at <b>{NEW_SITE_URL.replace(/^https:\/\//, '')}</b>, on an address of its own (this one is shared with other pages of the same GitHub account). Move this wallet there with a backup file; nothing is sent anywhere.</p>
          <button onClick={() => setOpen(true)}>Start moving…</button>
        </>
      ) : (
        <>
          {s.running && <p className="walleterr">The miner is running on this site. <b>Stop it before you mine on the new site</b> with the same address: two sites signing runs for one template can break the one-run-per-template rule.{' '}
            <button onClick={() => miner.stop()}>Stop mining</button></p>}
          <ol className="muted">
            <li>Choose a NEW password below (it protects the file; for a passkey wallet this turns the passkey wallet into a normal password wallet on the new site).</li>
            <li>Save the downloaded file and write down the seed words shown (they are the real backup).</li>
            <li>Open <a href={NEW_SITE_URL + '/wallet'} rel="noopener">{NEW_SITE_URL.replace(/^https:\/\//, '')}/wallet</a>, choose "Load backup file", pick the file, enter the new password.</li>
            <li>Check the address on the new site is the same as here, then you may forget the wallet here.</li>
          </ol>
          {!passkey && <p><input type="password" placeholder="current password" value={cur} onChange={(e) => setCur(e.target.value)} /></p>}
          <p><input type="password" placeholder="new password (8+ characters)" value={pw} onChange={(e) => setPw(e.target.value)} />{' '}
            <input type="password" placeholder="repeat it" value={pw2} onChange={(e) => setPw2(e.target.value)} /></p>
          <button disabled={!ok} onClick={prepare}>{passkey ? 'Use the passkey, ' : ''}prepare and download the move file</button>{' '}
          <button className="linkish" onClick={() => { setOpen(false); setRes(null); }}>close</button>
          {res && (
            <div className="notice" data-testid="migrate-done">
              <p>Downloaded <b className="mono">{res.name}</b> for <span className="mono break">{res.address}</span>, locked with your new password.</p>
              {res.seed ? <><p className="walleterr">Seed words (secret; they spend your coins):</p><p className="mono seedbox" data-testid="migrate-seed">{res.seed}</p></> : <p className="muted">This wallet has no seed words.</p>}
              <button onClick={() => download2(res.name, res.doc, 'application/json')}>Download the file again</button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
