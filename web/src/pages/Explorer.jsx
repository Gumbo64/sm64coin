import React, { useRef, useState } from 'react';
import { Link, useNavigate } from 'react-router';
import { daemon } from '../lib/rpc.js';
import { coins, short, ago, frames } from '../lib/format.js';
import useInterval from '../lib/useInterval.js';

const PAGE = 25;

export default function Explorer() {
  const [info, setInfo] = useState(null);
  const [task, setTask] = useState(null);
  const [blocks, setBlocks] = useState([]);
  // Proof sizes and run lengths come from get_block (headers don't carry
  // them); blocks never change, so cache them by hash.
  const proofs = useRef({});
  const [, bump] = useState(0);
  const [err, setErr] = useState(null);
  const [q, setQ] = useState('');
  const nav = useNavigate();

  useInterval(async () => {
    try {
      const i = await daemon('get_info');
      setInfo(i);
      const top = i.height - 1;
      const start = Math.max(0, top - PAGE + 1);
      const r = await daemon('get_block_headers_range', { start_height: start, end_height: top });
      const hs = [...r.headers].reverse();
      setBlocks(hs);
      const missing = hs.filter((h) => h.height > 0 && !proofs.current[h.hash]);
      Promise.all(missing.map((h) => daemon('get_block', { hash: h.hash })
        .then((b) => { proofs.current[h.hash] = { bytes: b.sm64_proof_bytes, frames: b.sm64_frames }; })
        .catch(() => {})))
        .then(() => { if (missing.length) bump((x) => x + 1); });
      // get_info carries the task a block found now would have to meet.
      setTask(i.sm64_max_frames ? { ...i, difficulty: i.sm64_difficulty } : null);
      setErr(null);
    } catch (e) { setErr(String(e)); }
  }, 5000);

  const search = (e) => {
    e.preventDefault();
    if (q.trim()) nav(`/block/${q.trim()}`);
  };

  return (
    <>
      {err && <div className="card error">Daemon unreachable: {err}</div>}
      {info && (
        <div className="stats">
          <Stat label="Height" value={info.height - 1} />
          <Stat label="Retarget difficulty" value={info.difficulty} />
          <Stat label="Difficulty now" value={task ? task.difficulty : '–'} />
          <Stat label="Time limit now" value={task ? `${(task.sm64_max_frames / 30 / 60).toFixed(2)} min` : '–'} />
          <Stat label="Pokes / window" value={task ? `${task.sm64_random_amount}/${task.sm64_window_length}` : '–'} />
          <Stat label="Target block time" value={`${info.target} s`} />
          <Stat label="Peers" value={info.outgoing_connections_count + info.incoming_connections_count} />
          <Stat label="Tx pool" value={info.tx_pool_size} />
        </div>
      )}
      {task && <p className="muted">Rules <b>{task.sm64_rules}</b>: {task.sm64_goal}
        {task.sm64_easier_at ? <> Easier task from {new Date(task.sm64_easier_at * 1000).toLocaleTimeString()} if no block is found.</> : null}</p>}
      <form onSubmit={search} className="search">
        <input placeholder="Block height or hash" value={q} onChange={(e) => setQ(e.target.value)} />
        <button>Go</button>
      </form>
      <table>
        <thead><tr><th>Height</th><th>Hash</th><th>Age</th><th>Difficulty</th><th>Reward</th><th>Txs</th><th title="Block weight: transactions and coinbase">Size</th><th title="The proof of play (encoded inputs), stored with the block on top of its size">Proof</th><th title="Length of the run that sealed the block">Run</th></tr></thead>
        <tbody>
          {blocks.map((b) => (
            <tr key={b.hash}>
              <td><Link to={`/block/${b.height}`}>{b.height}</Link></td>
              <td className="mono"><Link to={`/block/${b.hash}`}>{short(b.hash, 16)}</Link></td>
              <td>{b.height === 0 ? 'genesis' : ago(b.timestamp)}</td>
              <td>{b.difficulty}</td>
              <td>{coins(b.reward)}</td>
              <td>{b.num_txes}</td>
              <td>{(b.block_size / 1024).toFixed(1)} kB</td>
              <td>{b.height === 0 ? '—' : proofs.current[b.hash] ? `${(proofs.current[b.hash].bytes / 1024).toFixed(1)} kB` : '…'}</td>
              <td>{b.height === 0 ? '—' : proofs.current[b.hash] ? `${proofs.current[b.hash].frames} frames (${frames(proofs.current[b.hash].frames)})` : '…'}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

function Stat({ label, value }) {
  return <div className="stat"><div className="label">{label}</div><div className="value">{value}</div></div>;
}
