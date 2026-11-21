import React, { useEffect, useRef, useState } from 'react';
import { Link, useParams } from 'react-router';
import { playRun } from '../lib/recorder.js';
import { daemon } from '../lib/rpc.js';
import { rawToPads } from '../lib/proof.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, short, when, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';

export default function Block() {
  const { id } = useParams();
  const [b, setB] = useState(null);
  const [err, setErr] = useState(null);
  const [watch, setWatch] = useState(false);

  useEffect(() => {
    setB(null); setErr(null); setWatch(false);
    const params = /^\d+$/.test(id) ? { height: Number(id) } : { hash: id };
    daemon('get_block', params).then(setB).catch((e) => setErr(String(e)));
  }, [id]);

  if (err) return <div className="card error">{err}</div>;
  if (!b) return <div className="card">Loading…</div>;
  const h = b.block_header;
  // b.json is the daemon's own serialization; tolerate it being malformed
  // (older daemons emitted `"sm64_proof": 0""`) rather than crashing the page.
  let json = {};
  try { json = JSON.parse(b.json); } catch {
    const num = (k) => (new RegExp(`"${k}":\\s*(\\d+)`).exec(b.json || '') || [])[1];
    json = { major_version: num('major_version') ?? '?', minor_version: num('minor_version') ?? '?' };
  }
  return (
    <>
      <div className="card">
        <h2>Block {h.height}</h2>
        <dl className="kv">
          <dt>Hash</dt><dd className="mono">{h.hash}</dd>
          <dt>Previous</dt><dd className="mono">{h.height > 0 ? <Link to={`/block/${h.prev_hash}`}>{h.prev_hash}</Link> : '—'}</dd>
          <dt>Next</dt><dd><Link to={`/block/${h.height + 1}`}>{h.height + 1}</Link></dd>
          <dt>Time</dt><dd>{h.height === 0 ? 'genesis' : when(h.timestamp)}</dd>
          <dt>Difficulty</dt><dd>{h.difficulty} (cumulative {h.cumulative_difficulty})</dd>
          <dt>Reward</dt><dd>{coins(h.reward)} SM64</dd>
          <dt>Size</dt><dd>{h.block_size} bytes ({b.sm64_proof_bytes} of them proof-of-play)</dd>
          <dt>Transactions</dt><dd>{h.num_txes} + coinbase <span className="mono">{short(b.miner_tx_hash, 16)}</span></dd>
          {b.tx_hashes && b.tx_hashes.map((t) => <React.Fragment key={t}><dt></dt><dd className="mono">{t}</dd></React.Fragment>)}
          <dt>Version</dt><dd>{json.major_version}.{json.minor_version}</dd>
        </dl>
      </div>
      {h.height > 0 && (
        <div className="card">
          <h3>Proof of play</h3>
          <dl className="kv">
            <dt>Run length</dt><dd>{b.sm64_frames} frames ({frames(b.sm64_frames)}), limit {b.sm64_max_frames} ({frames(b.sm64_max_frames)})</dd>
            <dt>Perturbation</dt><dd>{b.sm64_random_amount} poke(s) of {b.sm64_burst_length} frames per {b.sm64_window_length}, reseeded every {b.sm64_reseed_frames} frames</dd>
            <dt>Seed hash</dt><dd className="mono">{b.sm64_seed_hash}</dd>
          </dl>
          {!watch ? <button onClick={() => setWatch(true)}>▶ Watch the run that sealed this block</button>
            : <RomGate><Replay block={b} /></RomGate>}
        </div>
      )}
    </>
  );
}

function Replay({ block }) {
  const canvas = useRef(null);
  const [status, setStatus] = useState('starting…');
  const [speed, setSpeed] = useState(1);
  const speedRef = useRef(1);
  speedRef.current = speed;
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    let killed = false;
    const pads = rawToPads(block.sm64_pads);
    setStatus(`replaying ${pads.length} frames; the perturbation is re-derived and checked on every frame`);
    daemon('get_info')
      .then((info) => {
        const won = successFor(info.sm64_rules);
        if (!won) throw new Error(`this page doesn't know the success condition of rules "${info.sm64_rules}" (see web/src/lib/rules.js)`);
        return playRun(canvas.current, withConfig(block), pads, won, async () => killed, () => speedRef.current, setFrame);
      })
      .then((o) => {
        if (killed) return;
        if (o === 'won') setStatus('★ Success condition met on the last frame: the proof replays.');
        else if (o.startsWith('mismatch')) setStatus(`✗ Frame ${o.split(':')[1]} does not match the perturbation.`);
        else setStatus('The replay ended without success.');
      })
      .catch((e) => !killed && setStatus(`Replay failed: ${e}`));
    return () => { killed = true; };
  }, [block]);
  return (
    <>
      <canvas ref={canvas} className="sm64canvas" />
      <p className="muted">{status} · frame {frame}/{block.sm64_frames} ·{' '}
        {[1, 2, 4, 8].map((s) => <button key={s} disabled={s === speed} onClick={() => setSpeed(s)}>{s}×</button>)}
      </p>
    </>
  );
}
