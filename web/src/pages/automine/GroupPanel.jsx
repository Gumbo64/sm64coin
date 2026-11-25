import React, { useState, useSyncExternalStore } from 'react';
import { groupLink, groupAvailable } from '../../lib/webminer/group.js';
import { fmtRate } from '../../lib/webminer/controller.js';

// Group mining (under the Auto-mine card's ⚙): a trusted group mines ONE block template together over iroh-gossip.
// Create a group (this browser fetches the template from its node, address included: the reward is paid to it) and give
// the team code to the others; or join with a code. Members share the first castle step and the runs they find;
// whoever finds the winning run submits it. The code is a secret: anyone who has it joins.
export default function GroupPanel({ running }) {
  const g = useSyncExternalStore(groupLink.subscribe, groupLink.getSnapshot);
  const [code, setCode] = useState('');
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try { await navigator.clipboard.writeText(g.code); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch { /* clipboard blocked */ }
  };
  if (!groupAvailable) return <p className="muted small">Group mining is not part of this build.</p>;
  return (
    <div className="viz-settings group-panel">
      <span className="lab">Group mining</span>
      <div className="ctl">
        {g.status === 'on' ? <span className="muted">{g.leader ? 'leader: your node supplies the template' : 'member: mining the leader’s template'}</span>
          : <span className="muted">mine one template together with people you trust</span>}
      </div>
      {g.status !== 'on' && (<>
        <label htmlFor="grp-name">Your name</label>
        <div className="ctl"><input id="grp-name" value={g.name} maxLength={40} placeholder="browser" onChange={(e) => groupLink.setName(e.target.value)} /></div>
        <label htmlFor="grp-relay">Relay</label>
        <div className="ctl"><input id="grp-relay" className="mono" value={g.relay} placeholder="n0's public relays (default)" onChange={(e) => groupLink.setRelay(e.target.value)} title="A relay server URL; empty: n0's public relays. A browser reaches the other members through a relay." /></div>
        <span className="lab">Join</span>
        <div className="ctl">
          <input className="mono" value={code} placeholder="team code (sm64team1-…)" onChange={(e) => setCode(e.target.value)} />
          <button type="button" disabled={!code.trim() || g.status === 'starting' || running} onClick={() => groupLink.join(code)}>Join</button>
        </div>
        <span className="lab">Create</span>
        <div className="ctl">
          <button type="button" disabled={g.status === 'starting' || running} onClick={() => groupLink.create()}>{g.status === 'starting' ? 'connecting…' : 'Create a group'}</button>
          {running && <span className="muted"> stop mining to change</span>}
        </div>
      </>)}
      {g.status === 'on' && (<>
        <span className="lab">Team code</span>
        <div className="ctl">
          <input className="mono" readOnly value={g.code} onFocus={(e) => e.target.select()} />
          <button type="button" onClick={copy}>{copied ? 'copied' : 'Copy'}</button>
        </div>
        <p className="muted small" style={{ gridColumn: '1 / -1', margin: 0 }}>A secret: anyone with this code joins and mines for the leader’s address.</p>
        <span className="lab">Members</span>
        <div className="ctl" style={{ display: 'block' }}>
          <table className="small">
            <tbody>
              {g.members.map((m) => (
                <tr key={m.id}><td>{m.name || m.id}{m.me ? ' (you)' : ''}{m.leader ? ' ★' : ''}</td><td className="muted">{m.fps > 0 ? `${fmtRate(m.fps)} fr/s` : '—'}</td><td className="mono muted">{m.id}</td></tr>
              ))}
            </tbody>
          </table>
          <span className="muted small">{g.neighbors} direct neighbour{g.neighbors === 1 ? '' : 's'}</span>
        </div>
        <span className="lab" />
        <div className="ctl"><button type="button" onClick={() => groupLink.leave()}>Leave the group</button></div>
        {g.log.length > 0 && <pre className="log small" style={{ gridColumn: '1 / -1', maxHeight: 90, overflow: 'auto', margin: 0 }}>{g.log.slice(-8).join('\n')}</pre>}
      </>)}
      {g.status === 'error' && <p className="small" style={{ gridColumn: '1 / -1', color: 'var(--bad, #c33)', margin: 0 }}>{g.error}</p>}
    </div>
  );
}
