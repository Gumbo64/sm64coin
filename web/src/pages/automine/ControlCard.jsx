import React, { useEffect, useState } from 'react';
import { miner, isAddress, isolated, fmtRate } from '../../lib/webminer/controller.js';
import { syncStatus, enableSync, disableSync, takeReloadNote } from '../../lib/webminer/sync.js';
import { SideCard, GearBtn, useStored } from '../../components/ui.jsx';

const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;

// The Auto-mine card of the sidebar (like the Mine page's reset-cycle card): the big Start/Stop, the speed, the
// counts, the reward address, and under the ⚙ the worker count, the reset policy, worker sync and the master
// switch of the visualisations.
// Worker sync's state and why, re-read when it may have changed; after a reload that came from the switch, the scroll position comes back.
function useSync() {
  const [st, setSt] = useState({ on: isolated, state: isolated ? 'on' : 'off', why: '' });
  const [tick, setTick] = useState(0);
  useEffect(() => { let dead = false; syncStatus().then((x) => { if (!dead) setSt(x); }); return () => { dead = true; }; }, [tick]);
  useEffect(() => {
    const note = takeReloadNote();
    if (note) {
      document.querySelector('.ui-sync')?.scrollIntoView({ block: 'center' });
      if (note.scrollY) setTimeout(() => window.scrollTo(0, note.scrollY), 0);
      setTick((x) => x + 1);
    }
  }, []);
  return [st, () => setTick((x) => x + 1)];
}

export default function ControlCard({ s }) {
  const { address, count, running, workers, stats, mode, vizOn, fps } = s;
  const [open, setOpen] = useStored('sm64coin.autoCardOpen', false);
  const [editing, setEditing] = useState(false);
  const n = workers.length || count;
  const [sync, recheck] = useSync();
  return (
    <SideCard className="first" title="Auto-mine" tools={<GearBtn open={open} onClick={() => setOpen(!open)} />}>
      {open && (
        <form className="viz-settings" onSubmit={(e) => e.preventDefault()}>
          <label htmlFor="am-workers">Workers</label>
          <div className="ctl"><input id="am-workers" type="number" min="1" max="32" value={count} onChange={(e) => miner.setCount(e.target.value)} disabled={running} /> {running && <span className="muted">stop to change</span>}</div>
          <label htmlFor="am-reset">Reset cycle</label>
          <div className="ctl">
            <select id="am-reset" value={mode} onChange={(e) => miner.setMode(e.target.value)} title="Reset cycle (power-on start) policy when the chain allows it">
              <option value="never">never</option>
              <option value="when-stuck">when stuck</option>
              <option value="always-when-allowed">whenever allowed</option>
            </select>
          </div>
          <span className="lab">Worker sync</span>
          <div className="ctl">
            {running ? <span className="muted">stop auto-mining to change it</span>
              : sync.on || sync.state === 'blocked' || sync.state === 'pending' ? <button type="button" onClick={disableSync} title="Removes the service worker and reloads; it stays off until you enable it again">Disable (reloads)</button>
                : <span className="muted">{sync.state === 'disabled' ? 'off by your choice (enable it below the title)' : 'see the line under the title'}</span>}
          </div>
          <span className="lab">Visualisations</span>
          <div className="ctl">
            <label className="vizswitch" title="The live game, 3D search cloud and charts. Off: nothing is drawn and the workers stop sampling their search trees (max speed)."><input type="checkbox" checked={vizOn} onChange={(e) => miner.setVizOn(e.target.checked)} /> on (off = maximum speed)</label>
          </div>
          {vizOn && running && !isolated && <p className="muted small" style={{ gridColumn: '1 / -1', margin: 0 }}>Workers started before it was switched on keep sampling off until their next search (enable worker sync for an instant switch).</p>}
        </form>
      )}
      <div className={`ui-sync ${sync.on ? 'on' : 'off'}`} data-testid="sync" data-state={sync.state}>
        <span className="chip" style={{ background: sync.on ? '#1f7a45' : '#3a3f5c', color: '#fff' }}>Worker sync: {sync.on ? 'on' : 'off'}</span>
        <span className="muted small"> {sync.why}</span>
        {!running && !sync.on && sync.state !== 'unsupported' && sync.state !== 'blocked' && <button type="button" className="ui-btn" style={{ width: '100%', marginTop: 4 }} onClick={() => enableSync()} title="Workers share their work and clicking a node plays it instantly; reloads the page once, it stays where it is">{sync.state === 'disabled' || sync.state === 'off' ? 'Enable worker sync (reloads once)' : 'Retry (reloads once)'}</button>}
        {running && !sync.on && <span className="muted small"> (stop auto-mining to change it)</span>}
      </div>
      {running ? <button className="ui-big" onClick={() => miner.stop()}>Stop auto-mining</button>
        : <button className="ui-big" onClick={() => miner.start()} disabled={!isAddress(address)}>Start auto-mining</button>}
      <div className="ui-stat">
        <span><b>{running && fps > 0 ? `${fmtRate(fps)} frames/s` : '—'}</b></span>
        <span>{n} worker{n === 1 ? '' : 's'} · {stats.found} run{stats.found === 1 ? '' : 's'} found · {stats.accepted} block{stats.accepted === 1 ? '' : 's'}</span>
      </div>
      <div className="ui-addr">
        Mining for {isAddress(address) ? <span className="mono paysto" title={address}>{short(address)}</span> : <em>no address</em>}{' '}
        <button className="linkish" title="Change the reward address" disabled={running} onClick={() => setEditing((e) => !e)}>✎ change</button>
      </div>
      {(editing || !isAddress(address)) && !running && (
        <div className="addrpop">
          <input className="mono wide" style={{ width: '100%' }} value={address} onChange={(e) => miner.setAddress(e.target.value.trim())} placeholder="SM64… (paste your address)" />
          {!isAddress(address) && <p className="muted small">Paste your address (in the SM64Coin wallet: Receive).</p>}
        </div>
      )}
    </SideCard>
  );
}
