import React, { useEffect } from 'react';
import { Link } from 'react-router';
import RomGate from '../lib/RomGate.jsx';
import { miner, isAddress, isolated, enableSync, fmtRate } from '../lib/webminer/controller.js';
import { useMiner } from '../lib/webminer/useMiner.js';

// The automated miner in the browser: the native miner's RRT campaign (objective-RRT search,
// pow/webminer) compiled to WebAssembly, one campaign per Web Worker. Each worker searches the
// same block template with its own salt (as `automine --salt`); the first run found is sealed,
// stamped when it is valid and submitted. A new tip restarts them on the new template.
// This page is only a view: the miner itself (lib/webminer/controller.js) lives as long as the site's
// page, so it goes on while you visit other pages (the header's light shows what it is doing).
// The visualisations (live game, 3D search cloud, envelope, campaign strips...) are loaded only while
// switched on (the toggle is remembered); off, workers are told to stop sampling their trees.
const VizPanel = React.lazy(() => import('./automine/VizPanel.jsx'));

export default function AutoMine() {
  return (
    <RomGate fallback={<div className="card"><h2>Auto-mine</h2><p>The automated miner plays the game in this browser: it needs your US Super Mario 64 ROM (it stays in this browser).</p></div>}>
      <AutoMiner />
    </RomGate>
  );
}

function AutoMiner() {
  const s = useMiner();
  const { address, count, running, log, workers, stats, mode, vizOn, fps, playFromStart, hybridOK } = s;
  // The views redraw only while this page is on screen.
  useEffect(() => {
    const q = new URLSearchParams(window.location.search);
    const a = q.get('address');
    if (a && isAddress(a.trim())) miner.setAddress(a.trim());
    if (q.get('vizmock') === '1' && !miner.running) miner.startMock({ count: 3, runAfter: Number(q.get('run') || 0) * 1000 });
    miner.setViewing(true);
    return () => miner.setViewing(false);
  }, []);

  return (
    <div className="card">
      <h2>Auto-mine</h2>
      <p className="muted">
        The automated miner in your browser: the same campaign search as the native <code>sm64coin-automine</code>
        (objective-RRT, compiled to WebAssembly), one per worker, each with its own seed. It uses a lot of CPU while it runs,
        and keeps running while you visit other pages of the site (stop it here). Rewards go to your address; nothing leaves this browser but the block.
      </p>
      <p>
        <input className="mono" style={{ width: '100%' }} placeholder="your SM64… address" value={address} onChange={(e) => miner.setAddress(e.target.value.trim())} disabled={running} />
      </p>
      <p>
        Workers <input type="number" min="1" max="32" value={count} onChange={(e) => miner.setCount(e.target.value)} disabled={running} style={{ width: '4em' }} />{' '}
        <select value={mode} onChange={(e) => miner.setMode(e.target.value)} title="Reset cycle (power-on start) policy when the chain allows it">
          <option value="never">reset: never</option>
          <option value="when-stuck">reset: when stuck</option>
          <option value="always-when-allowed">reset: whenever allowed</option>
        </select>{' '}
        {running ? <button onClick={() => miner.stop()}>Stop</button> : <button onClick={() => miner.start()} disabled={!isAddress(address)}>Start mining</button>}{' '}
        {!running && !isolated && 'serviceWorker' in navigator && <button onClick={enableSync} title="Workers share their best checkpoints (needs a page reload)">Enable worker sync</button>}{' '}
        <span className="muted">{isolated ? 'sync on · ' : ''}rounds {stats.rounds} · runs found {stats.found} · blocks submitted {stats.accepted}</span>
      </p>
      <p className="small">
        <label className="vizswitch" title="The live game, 3D search cloud, envelope and campaign views. Off: nothing is drawn and the workers stop sampling their search trees (max performance).">
          <input type="checkbox" checked={vizOn} onChange={(e) => miner.setVizOn(e.target.checked)} /> Visualisations
        </label>
        {vizOn && running && !isolated && <span className="muted"> · workers started before it was switched on keep sampling off until their next search (enable worker sync for an instant switch)</span>}
      </p>
      <p className="small">
        <label className="switch-row" title="On: manual mining ignores hybrid mining entirely: always from the block's beginning (power-on / the parent's end), no search tree to play from, no clicking into nodes. Off: click a node of the 3D cloud and press Play from here.">
          <input type="checkbox" checked={playFromStart} onChange={(e) => miner.setPlayFromStart(e.target.checked)} /> Play from the start
        </label>
        <span className="muted"> · {playFromStart
          ? <>hybrid mining is off: <Link to="/mine">play the classic manual miner</Link> from the block's beginning</>
          : <>hybrid mining: click a node in the cloud, <b>Play from here</b>, play on by hand, Enter returns to the visualiser{running && !hybridOK ? ' (needs worker sync: reload with "Enable worker sync", then restart)' : ''}</>}</span>
      </p>
      {running && (
        <p style={{ fontSize: '1.15em', margin: '0.4em 0' }} title="Simulated game frames per second, all workers (moving average over the last minute): this miner's hashrate">
          <strong>Speed</strong> <span className="mono">{fps > 0 ? `${fmtRate(fps)} frames/s` : '…'}</span>
          <span className="muted"> · {workers.length || count} worker{(workers.length || count) === 1 ? '' : 's'}</span>
        </p>
      )}
      {workers.length > 0 && (
        <table className="small">
          <tbody>
            {workers.map((w, i) => <tr key={i}><td>worker {i + 1}</td><td className="muted">salt {w.salt}{w.runs ? ` · ${w.runs} search${w.runs === 1 ? '' : 'es'} done` : ''}</td><td className="mono">{w.line}</td></tr>)}
          </tbody>
        </table>
      )}
      {vizOn && <React.Suspense fallback={<p className="muted">Loading visualisations…</p>}><VizPanel hub={miner.hub} mode={mode} /></React.Suspense>}
      <pre className="log small">{log.join('\n')}</pre>
    </div>
  );
}
