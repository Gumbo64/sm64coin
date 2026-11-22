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
  // Progress after each block, from get_sm64_cycle (blocks never change, so
  // cached by hash). One call gives the whole cycle's block list (power-on
  // flags, and the point count after each block: every block adds exactly
  // one) but the stars/keys split only for the block asked about, so exact
  // numbers are filled in one block at a time, newest first.
  const prog = useRef({});
  const busy = useRef(false);
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
      fillProgress(hs.filter((h) => h.height > 0));
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

  async function fillProgress(hs) {
    if (busy.current) return;
    busy.current = true;
    try {
      for (const h of hs) {
        if (prog.current[h.hash] && prog.current[h.hash].exact) continue;
        const c = await daemon('get_sm64_cycle', { hash: h.hash });
        c.blocks.forEach((b, i) => {
          const old = prog.current[b.hash] || {};
          prog.current[b.hash] = { ...old, powerOn: b.power_on, points: i + 1, cycleStart: c.start_height, event: b.event };
        });
        prog.current[h.hash] = { ...prog.current[h.hash], exact: true, stars: c.stars, keys: c.keys, grand: c.grand_star, ends: c.ends_cycle };
        bump((x) => x + 1);
      }
    } catch { /* the node may be busy: the next refresh retries */ } finally { busy.current = false; }
  }

  const progressCell = (b) => {
    if (b.height === 0) return '—';
    const p = prog.current[b.hash];
    if (!p) return '…';
    const got = p.event ? <span className="muted" title="What this block added: cNNsK = course NN star K (c00 = castle secret star), key1/key2 = Bowser keys"> · {eventName(p.event)}</span> : null;
    if (p.exact) return <>{p.stars} ★{p.keys ? ` ${p.keys} 🔑` : ''}{p.grand ? ' · Grand Star, cycle ends' : ''}{got}</>;
    return <span title="stars and keys are filled in shortly">{p.points} pts{got}</span>;
  };

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
      {task && <p className="muted">Rules <b>{task.sm64_rules}</b>: {task.sm64_goal}</p>}
      <form onSubmit={search} className="search">
        <input placeholder="Block height or hash" value={q} onChange={(e) => setQ(e.target.value)} />
        <button>Go</button>
      </form>
      <table>
        <thead><tr><th>Height</th><th>Hash</th><th>Age</th><th>Difficulty</th><th>Reward</th><th>Txs</th><th title="Block weight: transactions and coinbase">Size</th><th title="The proof of play (encoded inputs), stored with the block on top of its size">Proof</th><th title="Length of the run that sealed the block">Run</th><th title="The game's progress after this block: stars and Bowser keys (the 70-star cycle)">Progress</th></tr></thead>
        <tbody>
          {blocks.map((b) => (
            <tr key={b.hash}>
              <td><Link to={`/block/${b.height}`}>{b.height}</Link>
                {b.height > 0 && prog.current[b.hash] && prog.current[b.hash].powerOn && <span className="tag" title="This block started from power-on: the start of a cycle">⏻ cycle start</span>}</td>
              <td className="mono"><Link to={`/block/${b.hash}`}>{short(b.hash, 16)}</Link></td>
              <td>{b.height === 0 ? 'genesis' : ago(b.timestamp)}</td>
              <td>{b.difficulty}</td>
              <td>{coins(b.reward)}</td>
              <td>{b.num_txes}</td>
              <td>{(b.block_size / 1024).toFixed(1)} kB</td>
              <td>{b.height === 0 ? '—' : proofs.current[b.hash] ? `${(proofs.current[b.hash].bytes / 1024).toFixed(1)} kB` : '…'}</td>
              <td>{b.height === 0 ? '—' : proofs.current[b.hash] ? `${proofs.current[b.hash].frames} frames (${frames(proofs.current[b.hash].frames)})` : '…'}</td>
              <td>{progressCell(b)}</td>
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

// "c01s6" -> "BoB star 6", "key1" -> "Bowser key 1", "grand" -> "Grand Star".
const COURSES = ['Castle', 'BoB', 'WF', 'JRB', 'CCM', 'BBH', 'HMC', 'LLL', 'SSL', 'DDD', 'SL', 'WDW', 'TTM', 'THI', 'TTC', 'RR',
  'BitDW', 'BitFS', 'BitS', 'PSS', 'CotMC', 'TotWC', 'VCutM', 'WMotR', 'SA', 'Cake'];
export function eventName(e) {
  const m = /^c(\d\d)s(\d)$/.exec(e);
  if (m) return `${COURSES[Number(m[1])] || 'course ' + Number(m[1])} star ${m[2]}`;
  if (e.startsWith('key')) return `Bowser key ${e.slice(3)}`;
  if (e === 'grand') return 'Grand Star';
  return e;
}
