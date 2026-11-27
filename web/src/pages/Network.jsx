import { Link } from 'react-router';
import React, { useEffect, useState } from 'react';
import { chooseNode, daemonAt, getNode, nodeInUse, onNodeChange, getPublicNode, nodeNote, usingPublicFallback, NODE_HINT } from '../lib/rpc.js';
import useInterval from '../lib/useInterval.js';

const clean = (u) => (u || '').trim().replace(/\/+$/, '');
const LIST_KEY = 'sm64coin.nodes';
const loadList = () => { try { return JSON.parse(localStorage.getItem(LIST_KEY) || '[]'); } catch { return []; } };
const saveList = (l) => { try { localStorage.setItem(LIST_KEY, JSON.stringify(l)); } catch { /* storage unavailable */ } };

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
      const base = `height ${i.height}, ${i.synchronized ? 'synced' : 'syncing'}, ${i.outgoing_connections_count + i.incoming_connections_count} peers`;
      // Your own node with no incoming peers: other nodes can't reach it (it still syncs and mines through its outgoing ones).
      return nodeInUse().kind === 'local' && i.incoming_connections_count === 0 && i.outgoing_connections_count > 0
        ? `${base}. No incoming peers: forward TCP 16480 on your router (NAT) or VPN, and allow it in your firewall.`
        : base;
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
    <div className="card">
      <h2>Node</h2>
      <p className="muted">The node this site reads the chain from and submits blocks to. Leave it empty for the default (your own node).</p>
      <input className="wide" value={draft} placeholder="http://127.0.0.1:16481" onChange={(e) => setDraft(e.target.value)} />
      <button onClick={() => use(draft)}>Use this node</button>{' '}
      <button onClick={test}>Test</button>{' '}
      <button onClick={save} disabled={!clean(draft)}>Save</button>
      {nodeInUse().kind === 'local' && <p>Reading from: <b>this app's own node</b> (127.0.0.1)</p>}
      {nodeInUse().kind === 'public' && <p>Reading from: <b>the public node</b> <code>{nodeInUse().url}</code> (the local node is not running)</p>}
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
  );
}
