import { Link } from 'react-router';
import React, { useEffect, useState } from 'react';
import { chooseNode, daemonAt, getNode, nodeInUse, onNodeChange, getPublicNode, nodeNote, usingPublicFallback, NODE_HINT } from '../lib/rpc.js';
import useInterval from '../lib/useInterval.js';
import { verifyTitle } from '../lib/useVerify.js';
import { getPlatform } from '../platform/index.js';
import LocalNode from './LocalNode.jsx';
import NodePortsHelp from './NodePortsHelp.jsx';

const clean = (u) => (u || '').trim().replace(/\/+$/, '');
const LIST_KEY = 'sm64coin.nodes';
const loadList = () => { try { return JSON.parse(localStorage.getItem(LIST_KEY) || '[]'); } catch { return []; } };
const saveList = (l) => { try { localStorage.setItem(LIST_KEY, JSON.stringify(l)); } catch { /* storage unavailable */ } };

const label = (key) => String(key || '').replace(/^http:/, '');
// What the verifier found, in words (Rust lcverify.rs). A node that served an invalid chain is banned for a week.
function Verified({ v }) {
  if (!v) return null;
  const mark = { verified: '✓', conflict: '⚠', unverified: '?', checking: '…' }[v.state] || '';
  return (
    <p className={`verify-${v.state}`} title={verifyTitle(v)}>
      Chain check: <b>{mark} {v.label}</b>
      {v.height != null && <span className="muted"> | tip {v.height}, from the release's checkpoint at {v.checkpoint_height}{v.level === 'structural' ? ' (structure only)' : ''}{v.quorum ? '' : ', fewer than 3 nodes'}</span>}
      {v.replay && v.replay.state !== 'off' && <span className="muted"> | replay: {v.replay.state === 'running' && v.replay.total ? `${v.replay.done}/${v.replay.total}` : v.replay.detail || v.replay.state}</span>}
      {v.error && <span className="muted"> | {v.error}</span>}
      {v.banned && v.banned.length > 0 && <span className="muted"> | banned: {v.banned.join(', ')}</span>}
      {v.disagree && v.disagree.length > 0 && <span className="muted"> | {v.disagree.map(([k, why]) => `${k}: ${why}`).join('; ')}</span>}
    </p>
  );
}

const TRANSPORT = { https: 'https', http: 'http' };

// The app's light-client network (Rust lcnet.rs): which node and transport the last call used, and the candidate list with its
// scores. Shown where the app reads from remote nodes (no local node answering): Android always, desktop while its node is down.
function LightNet() {
  const net = getPlatform().net;
  const [st, setSt] = useState(null);
  const load = async () => { try { setSt(await net.status()); } catch { /* not up yet */ } };
  useInterval(load, 5000);
  useEffect(() => { load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  if (!st) return null;
  return (
    <div className="muted">
      <p>
        Connected through: <b>{st.used ? <><code>{label(st.used.node)}</code> over <b>{TRANSPORT[st.used.transport] || st.used.transport}</b></> : 'no call yet'}</b>
      </p>
      <Verified v={st.verify} />
      <table>
        <tbody>
          {st.nodes.map((n) => (
            <tr key={n.key}>
              <td><code>{label(n.key)}</code></td>
              <td>{TRANSPORT[n.transport] || n.transport}</td>
              <td>{n.source}</td>
              <td>{n.banned ? 'banned' : `${Math.round(n.ok_rate * 100)}% ok`}{n.height ? `, height ${n.height}` : ''}{n.latency_ms ? `, ${Math.round(n.latency_ms)} ms` : ''}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p>Calls move to the next node when one fails; submissions go to three nodes at once. The wallet and the miner use only nodes on the verified chain.</p>
    </div>
  );
}

// Which node the whole app talks to (your own by default). A node other than
// this site's own needs CORS for the site's origin (--rpc-access-control-origins).
export default function Network() {
  const [node, setNodeState] = useState(getNode());
  const [draft, setDraft] = useState(getNode());
  const [status, setStatus] = useState('');
  const [list, setList] = useState(loadList());

  const check = async (u) => {
    try {
      const i = await daemonAt(u, 'get_info');
      // The local node's port-forward warning is on its own card (it knows the configured port): pages/automine/localNode.js.
      return `height ${i.height}, ${i.synchronized ? 'synced' : 'syncing'}, ${i.outgoing_connections_count + i.incoming_connections_count} peers`;
    } catch (e) {
      return `unreachable: ${e.message}. ${NODE_HINT}`;
    }
  };
  useEffect(() => onNodeChange(() => { setNodeState(getNode()); setDraft((d) => (nodeInUse().kind === 'chosen' ? d : getNode())); }), []);
  const refresh = async () => setStatus(await check(node));
  useInterval(refresh, 10000);
  useEffect(() => { refresh(); }, [node]); // eslint-disable-line react-hooks/exhaustive-deps

  const use = async (u) => {
    const v = clean(u);
    try { await chooseNode(v); } catch (e) { setStatus(e.code === 'cancelled' ? 'not changed' : `not changed: ${e.message}`); return; }
    setNodeState(getNode()); setDraft(v); setStatus('');
  };
  const save = () => { const v = clean(draft); if (!v || list.includes(v)) return; const l = [...list, v]; setList(l); saveList(l); };
  const remove = (u) => { const l = list.filter((x) => x !== u); setList(l); saveList(l); };
  const test = async () => { const v = clean(draft) || node; setStatus('testing ' + (v || 'the proxy') + ': ' + (await check(v))); };

  return (
    <>
      <LocalNode />
      {!getPlatform().node && !getPlatform().mobile && <NodePortsHelp />}
      <div className="card">
      <h2>{getPlatform().node ? 'Read from another node' : 'Node'}</h2>
      <p className="muted">The node this site reads the chain from and submits blocks to. Leave it empty for the default ({getPlatform().node ? "the app's own node" : 'this site\'s node'}).</p>
      <input className="wide" value={draft} placeholder="http://127.0.0.1:16481" onChange={(e) => setDraft(e.target.value)} />
      <button onClick={() => use(draft)}>Use this node</button>{' '}
      <button onClick={test}>Test</button>{' '}
      <button onClick={save} disabled={!clean(draft)}>Save</button>{' '}
      {getPlatform().node && <button onClick={() => use('')}>Use the app's own node</button>}
      {nodeInUse().kind === 'local' && <p>Reading from: <b>this app's own node</b> (127.0.0.1)</p>}
      {nodeInUse().kind === 'public' && <p>Reading from: <b>the public node</b> <code>{nodeInUse().url}</code> (the local node is not running)</p>}
      {getPlatform().net && !['local', 'chosen'].includes(nodeInUse().kind) && <LightNet />}
      <p>Current: <b>{node || (nodeInUse().kind === 'local' ? '(the app\'s own node)' : '(this site\'s proxy)')}</b>{status && <span className="muted"> | {status}</span>}{/unreachable|No incoming peers/.test(status) && <> <Link to="/info#connection">Help</Link></>}</p>
      {usingPublicFallback() && node === getPublicNode() && (
        <p className="muted">{nodeNote() || `Your own node (${import.meta.env.VITE_DEFAULT_NODE}) didn't answer, so this visit uses the public node. Start yours and reload to use it.`}</p>
      )}
      {nodeNote() && !(usingPublicFallback() && node === getPublicNode()) && <p className="muted">{nodeNote()}</p>}
      {getPublicNode() && node !== getPublicNode() && (
        <p className="muted">Public node: <code>{getPublicNode()}</code> <button onClick={() => use(getPublicNode())}>Use it</button></p>
      )}
      {list.length > 0 && (
        <table>
          <tbody>
            {list.map((u) => (
              <tr key={u}>
                <td><code>{u}</code></td>
                <td><button onClick={() => use(u)}>Use</button>{' '}<button onClick={() => remove(u)}>Remove</button></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      </div>
    </>
  );
}
