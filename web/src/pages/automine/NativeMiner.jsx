import React, { useCallback, useEffect, useRef, useState } from 'react';
import { getPlatform } from '../../platform/index.js';
import { miner, isAddress, fmtRate } from '../../lib/webminer/controller.js';
import { useMiner } from '../../lib/webminer/useMiner.js';
import { SideCard, GearBtn, Toolbar, ToolBtn, Row, useStored } from '../../components/ui.jsx';

// Apps only (platform.nativeMiner): on desktop the native sm64coin-automine against the app's own node (also shows the local
// node and, until the ROM is found, asks for its path); on Android (platform.mobile) the miner library inside the app, against
// the public node, with the page's own ROM, and only while the app is in the foreground.
const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;
const KEEP = 200;
// What this webview supports (the WASM workers want SharedArrayBuffer, the game WebGL). Probed once, and the probe's context
// is released at once: a context per render piled up until the browser dropped the oldest live one (the 3D cloud went black).
let caps = null;
const webviewCaps = () => {
  if (caps) return caps;
  let gl = false;
  try {
    const c = document.createElement('canvas').getContext('webgl2');
    gl = !!c;
    c?.getExtension('WEBGL_lose_context')?.loseContext();
  } catch { /* none */ }
  caps = `crossOriginIsolated ${!!globalThis.crossOriginIsolated} · SharedArrayBuffer ${typeof SharedArrayBuffer !== 'undefined'} · WebGL2 ${gl}`;
  return caps;
};

function usePolled(fn, ms) {
  const [v, setV] = useState(null);
  const ref = useRef(fn);
  ref.current = fn;
  const refresh = useCallback(async () => { try { setV(await ref.current()); } catch (e) { setV({ error: e.message }); } }, []);
  useEffect(() => { refresh(); const t = setInterval(refresh, ms); return () => clearInterval(t); }, [refresh, ms]);
  return [v, refresh];
}

export default function NativeMiner() {
  const p = getPlatform();
  const { address } = useMiner();
  const [open, setOpen] = useStored('sm64coin.nativeCardOpen', false);
  const mobile = !!p.mobile;
  // Desktop: 0 = all cores but one. Android: min(4, cores - 1) (a phone throttles and drains fast with more).
  const defThreads = mobile ? Math.max(1, Math.min(4, (navigator.hardwareConcurrency || 2) - 1)) : 0;
  const [threads, setThreads] = useStored('sm64coin.nativeThreads', defThreads);
  const [host, setHost] = useState(null);
  const [node, refreshNode] = usePolled(() => (p.node ? p.node.status() : Promise.resolve(null)), 2000);
  const [m, refreshMiner] = usePolled(() => p.nativeMiner.status(), 2000);
  const [lines, setLines] = useState([]);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => { p.host().then(setHost).catch(() => {}); }, [p]);
  useEffect(() => {
    if (m && m.lines && lines.length === 0 && m.lines.length) setLines(m.lines.slice(-KEEP));
  }, [m && m.running]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    const offLog = p.nativeMiner.subscribe((line) => setLines((l) => (l.length >= KEEP ? l.slice(1 - KEEP) : l).concat(line.startsWith('progress ') ? [] : [line])));
    const offState = p.nativeMiner.onState(() => refreshMiner());
    return () => { offLog(); offState(); };
  }, [p, refreshMiner]);

  const run = async (f) => { setBusy(true); setErr(''); try { await f(); } catch (e) { setErr(e.message); } finally { setBusy(false); refreshNode(); refreshMiner(); } };
  const running = !!(m && m.running);
  const romOk = host && host.rom ? host.rom.found : true; // Android: the page's ROM gate has already passed
  const nodeUp = mobile ? true : !!(node && node.answering); // Android: the public node
  const th = Number(threads) || undefined;
  // While the native miner runs, its search drives the page's visualisations (lib/webminer/nativeFeed.js).
  useEffect(() => { if (running) miner.attachNative(p.nativeMiner); }, [running, p]);
  // Android: mining only while the app is in the foreground; platform/tauri.js stops it on hide, on any page.

  return (
    <SideCard title={mobile ? 'Native miner (preview)' : 'Native miner'} tools={<GearBtn open={open} onClick={() => setOpen(!open)} />}>
      {open && (
        <form className="viz-settings" onSubmit={(e) => e.preventDefault()}>
          <label htmlFor="nm-threads">Threads</label>
          <div className="ctl"><input id="nm-threads" type="number" min="0" max="256" value={threads} onChange={(e) => setThreads(e.target.value)} disabled={running} /> <span className="muted">{running ? 'stop to change' : mobile ? 'default: cores - 1, at most 4' : '0 = all cores but one'}</span></div>
          <span className="lab">Webview</span><div className="ctl"><span className="mono small" data-testid="webview-caps">{webviewCaps()}</span></div>
          {host && (<><span className="lab">Data</span><div className="ctl"><span className="mono small">{host.data_dir}</span></div></>)}
        </form>
      )}
      {!romOk && (
        <div className="notice">
          <p>The app needs your US Super Mario 64 ROM (.z64) for its node and miner. It is checked (SHA-1) and copied into the app's data folder; it is not included.</p>
          <Toolbar label="ROM">
            <ToolBtn disabled={busy} onClick={() => run(async () => { const r = await p.rom.pick(); if (r) setHost(await p.host()); })}>Choose ROM file...</ToolBtn>
          </Toolbar>
        </div>
      )}
      {mobile && <p className="muted small" data-testid="native-battery">Uses a lot of battery and warms the phone: best while charging. It mines only while the app is open on screen, and stops when you leave it.</p>}
      <div className="ui-stat">
        <span><b>{running && m.fps > 0 ? `${fmtRate(m.fps)} frames/s` : '—'}</b></span>
        <span>{running ? `${m.threads} thread${m.threads === 1 ? '' : 's'} · ${m.accepted} block${m.accepted === 1 ? '' : 's'}` : 'native miner stopped'}</span>
      </div>
      <dl className="kv">
        <dt>{mobile ? 'Node' : 'Local node'}</dt>
        <dd data-testid="native-node">
          {mobile ? 'the public node' : nodeUp ? `block ${node.height}${node.synchronized ? '' : ` of ${node.target_height || '?'}`} · ${node.peers} peer${node.peers === 1 ? '' : 's'}${node.synchronized ? '' : ' · syncing'}`
            : node && node.starting ? 'starting…' : node && node.running ? 'loading the chain…' : node && node.error ? <span className="error">{node.error.split('\n')[0]}</span> : 'not running'}
        </dd>
        <dt>Paying</dt><dd>{isAddress(address || '') ? <span className="mono" title={address}>{short(address)}</span> : <em>no address</em>}</dd>
      </dl>
      {!running && (
        <input className="mono wide" style={{ width: '100%' }} value={address || ''} onChange={(e) => miner.setAddress(e.target.value.trim())} placeholder="SM64… (paste your address)" title="Reward address" />
      )}
      <Toolbar label="Native miner">
        {running
          ? <ToolBtn disabled={busy} onClick={() => run(() => p.nativeMiner.stop())}>Stop native miner</ToolBtn>
          : <ToolBtn disabled={busy || !isAddress(address || '') || !nodeUp} title={nodeUp ? '' : 'the local node must be running'} onClick={() => run(async () => { setLines([]); await p.nativeMiner.start({ address, threads: th, viz: miner.vizOn }); })}>Start native miner</ToolBtn>}
        {p.node && node && !node.running && !node.starting && romOk && <ToolBtn disabled={busy} onClick={() => run(() => p.node.start())}>Start node</ToolBtn>}
        {p.node && node && node.running && !running && <ToolBtn disabled={busy} onClick={() => run(() => p.node.stop())}>Stop node</ToolBtn>}
      </Toolbar>
      {err && <p className="error small">{err}</p>}
      {m && m.exit_code != null && !running && <p className="muted small">The miner exited (code {m.exit_code}).</p>}
      <Row title="Native log" storeKey="native-log"><pre className="log small" data-testid="native-log">{lines.join('\n') || 'Nothing yet.'}</pre></Row>
    </SideCard>
  );
}
