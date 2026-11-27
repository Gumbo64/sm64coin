import { MAX_RUN_FRAMES } from '../lib/rules.js';
import React, { useEffect, useRef, useState } from 'react';
import { Link, useNavigate } from 'react-router';
import CycleTimeline, { RecentCycles, eventName } from '../lib/CycleTimeline.jsx';
import { daemon, NODE_HINT } from '../lib/rpc.js';
import { getCycle, blockSummary } from '../lib/replay/client.js';
import { coins, short, ago, frames } from '../lib/format.js';
import { useTip, useTipInfo, tipStore } from '../lib/tip.js';
import { limitNow } from '../lib/limit.js';

const PAGE = 25;

export default function Explorer() {
  const tip = useTip();
  const info = useTipInfo();
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { const id = setInterval(() => setNow(Date.now() / 1000), 1000); return () => clearInterval(id); }, []);
  // the task a block found now has to meet: the page's own clock and the Rust rules (lib/limit.js), not a polled number
  const task = info && info.sm64_max_frames && tip ? { ...info, difficulty: info.sm64_difficulty, sm64_max_frames: limitNow(tip, now) } : null;
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

  // The block list is read once per new tip (the store follows the tip by push or a slow poll), and again after an error.
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!tip) return undefined;
    let live = true;
    (async () => {
      try {
        const top = tip.height;
        const start = Math.max(0, top - PAGE + 1);
        const r = await daemon('get_block_headers_range', { start_height: start, end_height: top });
        if (!live) return;
        const hs = [...r.headers].reverse();
        setBlocks(hs);
        fillProgress(hs.filter((h) => h.height > 0));
        const missing = hs.filter((h) => h.height > 0 && !proofs.current[h.hash]);
        // blockSummary: fetched once per block and kept in IndexedDB (lib/replay/client.js).
        Promise.all(missing.map((h) => blockSummary(h.hash)
          .then((x) => { proofs.current[h.hash] = x; })
          .catch(() => {})))
          .then(() => { if (missing.length) bump((x) => x + 1); });
        setErr(null);
      } catch (e) { if (live) { setErr(String(e)); setTimeout(() => setRetry((x) => x + 1), 10000); } }
    })();
    return () => { live = false; };
  }, [tip && tip.hash, retry]);

  // One get_sm64_cycle per cycle on the page (not per row): every block adds exactly one progress
  // point and the reply names each block's event, so the stars/keys after every block of a cycle
  // are counted from it, and checked against the node's own totals for the cycle's last block.
  // (Each reply carries the whole cycle's played pads, and a cold node replays the cycle to make
  // it: asking once per row made a page load cost the node minutes on small boards.)
  async function fillProgress(hs) {
    if (busy.current) return;
    busy.current = true;
    try {
      for (const h of hs) { // newest first: h is the last block of its cycle among the rows not done yet
        if (prog.current[h.hash] && prog.current[h.hash].exact) continue;
        const c = await getCycle(h.hash);
        const blocks = c.blocks || [];
        let stars = 0, keys = 0, known = true;
        const counted = blocks.map((b, i) => {
          const ev = b.event || '';
          if (/^c\d+s\d+$/.test(ev)) stars += 1;
          else if (/^key\d$/.test(ev)) keys += 1;
          else if (ev !== 'grand' && ev !== 'wing' && ev !== 'metal' && ev !== 'vanish') known = false; // a cap switch adds no star or key
          return { b, i, stars, keys };
        });
        const exact = known && stars === c.stars && keys === c.keys;
        counted.forEach(({ b, i, stars: s, keys: k }) => {
          const old = prog.current[b.hash] || {};
          if (old.exact) return;
          const last = i === blocks.length - 1;
          prog.current[b.hash] = { ...old, powerOn: b.power_on, points: i + 1, cycleStart: c.start_height, event: b.event,
            ...(exact || last ? { exact: true, stars: s, keys: k, grand: last ? c.grand_star : b.event === 'grand', ends: last ? c.ends_cycle : b.event === 'grand' } : {}) };
        });
        bump((x) => x + 1);
      }
    } catch { /* the node may be busy: the next refresh retries */ } finally { busy.current = false; }
  }
  const progressCell = (b) => {
    if (b.height === 0) return '—';
    const p = prog.current[b.hash];
    if (!p) return '…';
    const got = p.event && p.event !== 'grand' ? <span className="muted" title="What this block added: cNNsK = course NN star K (c00 = castle secret star), key1/key2 = Bowser keys, wing/metal/vanish = the cap switches"> · {eventName(p.event)}</span> : null;
    if (p.exact) return <>{p.stars} ★{p.keys ? ` ${p.keys} 🔑` : ''}{p.grand ? ' · Grand Star, cycle ends' : ''}{got}</>;
    return <span title="stars and keys are filled in shortly">{p.points} pts{got}</span>;
  };

  const search = (e) => {
    e.preventDefault();
    if (q.trim()) nav(`/block/${q.trim()}`);
  };

  return (
    <>
      {(err || (!tip && tipStore().status().error)) && <div className="card error">Daemon unreachable: {String(err || tipStore().status().error)}. {NODE_HINT}</div>}
      <CycleTimeline />
      {info && (
        <div className="stats">
          <Stat label="Height" value={info.height - 1} />
          <Stat label="Retarget difficulty" value={info.difficulty} />
          <Stat label="Difficulty now" value={task ? task.difficulty : '–'} />
          <Stat label="Accepting runs up to" value={task ? `${frames(task.sm64_max_frames)}${task.sm64_max_frames >= MAX_RUN_FRAMES ? '' : ` (loosens to ${frames(MAX_RUN_FRAMES)})`}` : '–'} />
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
      <div className="explorer-grid">
      <RecentCycles />
      <div className="blocklist">
      <table>
        <thead><tr><th>Height</th><th>Hash</th><th>Age</th><th>Difficulty</th><th>Reward</th><th>Txs</th><th title="Block weight: transactions and coinbase">Size</th><th title="The proof of play (encoded inputs), stored with the block on top of its size">Proof</th><th title="Length of the run that sealed the block">Run</th><th title="The game's progress after this block: stars, Bowser keys and cap switches (the 70-star cycle)">Progress</th></tr></thead>
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
      </div>
      </div>
    </>
  );
}

function Stat({ label, value }) {
  return <div className="stat"><div className="label">{label}</div><div className="value">{value}</div></div>;
}

