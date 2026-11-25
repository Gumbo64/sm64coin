import React, { useState } from 'react';
import { miner, isAddress, isolated, enableSync, fmtRate } from '../../lib/webminer/controller.js';
import { SideCard, GearBtn, useStored } from '../../components/ui.jsx';

const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;

// The Auto-mine card of the sidebar (like the Mine page's reset-cycle card): the big Start/Stop, the speed, the
// counts, the reward address, and under the ⚙ the worker count, the reset policy, worker sync and the master
// switch of the visualisations.
export default function ControlCard({ s }) {
  const { address, count, running, workers, stats, mode, vizOn, fps } = s;
  const [open, setOpen] = useStored('sm64coin.autoCardOpen', false);
  const [editing, setEditing] = useState(false);
  const n = workers.length || count;
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
            {isolated ? <span className="muted">on: workers share their best checkpoints</span>
              : 'serviceWorker' in navigator && !running ? <button type="button" onClick={enableSync} title="Workers share their best checkpoints (needs a page reload)">Enable (reloads the page)</button>
                : <span className="muted">{running ? 'stop to enable' : 'not available here'}</span>}
          </div>
          <span className="lab">Visualisations</span>
          <div className="ctl">
            <label className="vizswitch" title="The live game, 3D search cloud and charts. Off: nothing is drawn and the workers stop sampling their search trees (max speed)."><input type="checkbox" checked={vizOn} onChange={(e) => miner.setVizOn(e.target.checked)} /> on (off = maximum speed)</label>
          </div>
          {vizOn && running && !isolated && <p className="muted small" style={{ gridColumn: '1 / -1', margin: 0 }}>Workers started before it was switched on keep sampling off until their next search (enable worker sync for an instant switch).</p>}
        </form>
      )}
      {running ? <button className="ui-big" onClick={() => miner.stop()}>Stop</button>
        : <button className="ui-big" onClick={() => miner.start()} disabled={!isAddress(address)}>Start mining</button>}
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
