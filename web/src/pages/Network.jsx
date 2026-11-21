import React, { useEffect, useState } from 'react';
import { daemonAt, getNode, setNode } from '../lib/rpc.js';
import { short } from '../lib/format.js';
import useInterval from '../lib/useInterval.js';

const LIST_KEY = 'sm64coin.nodes';
const loadList = () => { try { return JSON.parse(localStorage.getItem(LIST_KEY) || '[]'); } catch { return []; } };
const saveList = (l) => { try { localStorage.setItem(LIST_KEY, JSON.stringify(l)); } catch { /* ignore */ } };
const clean = (u) => (u || '').trim().replace(/\/+$/, '');

// Pick which daemon the whole app talks to, see its peers, keep a list of
// nodes. A node reached directly from the browser needs CORS enabled
// (scripts/public-node.sh).
export default function Network() {
  const [node, setNodeState] = useState(getNode());
  const [draft, setDraft] = useState(getNode());
  const [list, setList] = useState(loadList());
  const [info, setInfo] = useState(null);
  const [err, setErr] = useState(null);
  const [peers, setPeers] = useState(null);
  const [peerErr, setPeerErr] = useState(null);
  const [tests, setTests] = useState({});

  const refresh = async () => {
    try { setInfo(await daemonAt(node, 'get_info')); setErr(null); }
    catch (e) { setInfo(null); setErr(String(e)); }
    try {
      const c = await daemonAt(node, 'get_connections');
      setPeers({ kind: 'connections', rows: c.connections || [] }); setPeerErr(null);
    } catch {
      // restricted (public) nodes hide get_connections: try the peer list
      try {
        const r = await fetch(`${node}/get_peer_list`, { method: 'POST', body: '{}' });
        const p = await r.json();
        setPeers({ kind: 'peerlist', rows: [...(p.white_list || []), ...(p.gray_list || [])] }); setPeerErr(null);
      } catch { setPeers(null); setPeerErr('Peer details are not available on this node (restricted RPC).'); }
    }
  };
  useInterval(refresh, 10000);
  useEffect(() => { refresh(); }, [node]); // eslint-disable-line react-hooks/exhaustive-deps

  const use = (u) => { const v = clean(u); setNode(v); setNodeState(v); setDraft(v); setInfo(null); setPeers(null); };
  const test = async (u) => {
    const v = clean(u);
    setTests((t) => ({ ...t, [v]: 'testing...' }));
    try {
      const i = await daemonAt(v, 'get_info');
      setTests((t) => ({ ...t, [v]: `ok: height ${i.height}, top ${short(i.top_block_hash)}, ${i.synchronized ? 'synced' : 'syncing'}, ${i.outgoing_connections_count + i.incoming_connections_count} peers` }));
    } catch (e) {
      setTests((t) => ({ ...t, [v]: `failed: ${e.message} (wrong URL, node down, or CORS not enabled on the node)` }));
    }
  };
  const add = () => {
    const v = clean(draft);
    if (!v || list.includes(v)) return;
    const l = [...list, v]; setList(l); saveList(l);
  };
  const remove = (u) => { const l = list.filter((x) => x !== u); setList(l); saveList(l); };

  return (
    <>
      <div className="card">
        <h2>Node</h2>
        <p className="muted">Where this page reads the chain from. Empty means this site's own proxy (<code>/json_rpc</code>). A direct URL needs the node to allow CORS.</p>
        <input className="wide" value={draft} placeholder="empty = same-origin proxy, or https://node.example.org:16481" onChange={(e) => setDraft(e.target.value)} />
        <button onClick={() => use(draft)}>Use this node</button>{' '}
        <button onClick={() => test(draft)}>Test</button>{' '}
        <button onClick={add} disabled={!clean(draft)}>Save to list</button>
        {tests[clean(draft)] && <p>{tests[clean(draft)]}</p>}
        <p>Current: <b>{node || '(same-origin proxy)'}</b></p>
        {err && <p style={{ color: 'var(--red)' }}>{err}</p>}
        {info && (
          <table>
            <tbody>
              <tr><td>height</td><td>{info.height}</td></tr>
              <tr><td>top block</td><td><code>{info.top_block_hash}</code></td></tr>
              <tr><td>synchronized</td><td>{String(info.synchronized)}{info.target_height ? ` (target ${info.target_height})` : ''}</td></tr>
              <tr><td>connections</td><td>{info.outgoing_connections_count} out, {info.incoming_connections_count} in</td></tr>
              <tr><td>rules</td><td>{info.sm64_rules}</td></tr>
              <tr><td>difficulty</td><td>{info.sm64_difficulty ?? info.difficulty}</td></tr>
            </tbody>
          </table>
        )}
      </div>

      <div className="card">
        <h2>Peers of this node{peers ? ` (${peers.rows.length})` : ''}</h2>
        {peerErr && <p className="muted">{peerErr}</p>}
        {peers && peers.rows.length === 0 && <p className="muted">No peers.</p>}
        {peers && peers.rows.length > 0 && (
          <table>
            <thead><tr><th>address</th><th>{peers.kind === 'connections' ? 'height / state' : 'last seen'}</th></tr></thead>
            <tbody>
              {peers.rows.map((p, i) => peers.kind === 'connections'
                ? <tr key={i}><td>{p.incoming ? 'in ' : 'out'} {p.address}</td><td>{p.height} {p.state}</td></tr>
                : <tr key={i}><td>{p.host}:{p.port}</td><td>{p.last_seen ? new Date(p.last_seen * 1000).toLocaleString() : ''}</td></tr>)}
            </tbody>
          </table>
        )}
      </div>

      <div className="card">
        <h2>Saved nodes</h2>
        {list.length === 0 && <p className="muted">None yet. Type a URL above and press "Save to list".</p>}
        {list.map((u) => (
          <div key={u} className="card inner">
            <code>{u}</code>{' '}
            <button onClick={() => use(u)}>Use</button>{' '}
            <button onClick={() => test(u)}>Test</button>{' '}
            <button onClick={() => remove(u)}>Remove</button>
            {tests[u] && <div>{tests[u]}</div>}
          </div>
        ))}
      </div>

      <div className="card">
        <h2>Run or join a network</h2>
        <p><b>Run a node</b> (syncs from the peers in <code>seeds.txt</code>): <code>scripts/start-node.sh</code>. The very first node of a brand new network uses <code>SOLO=1</code>.</p>
        <p><b>Join</b> a known node: <code>scripts/join.sh host:16480</code>.</p>
        <p><b>Public node</b> for browsers and other users: <code>scripts/public-node.sh</code> (restricted RPC on 0.0.0.0 with CORS; open ports 16480 p2p and 16481 RPC). Put its URL here, for example <code>http://host:16481</code>. Pages served over https can only call https nodes, so put a TLS reverse proxy in front of public nodes used from https sites.</p>
        <p className="muted">See the "Decentralisation" section of the README.</p>
      </div>
    </>
  );
}
