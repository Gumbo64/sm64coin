import React, { useEffect, useRef, useState } from 'react';
import { Link, useParams } from 'react-router';
import { playRun } from '../lib/recorder.js';
import { createEngine, reachTip } from '../lib/cycle.js';
import { readProgress } from '../lib/perturb.js';
import { restoreState } from '../lib/savestate.js';
import { daemon } from '../lib/rpc.js';
import { rawToPads, toM64, download } from '../lib/proof.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, short, when, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';

export default function Block() {
  const { id } = useParams();
  const [b, setB] = useState(null);
  const [err, setErr] = useState(null);
  const [watch, setWatch] = useState(false);
  const [cycle, setCycle] = useState(null); // get_sm64_cycle up to this block

  useEffect(() => {
    setB(null); setErr(null); setWatch(false); setCycle(null);
    const params = /^\d+$/.test(id) ? { height: Number(id) } : { hash: id };
    // sm64_played: the daemon replays the run and returns every frame's pad
    // (the proof itself leaves out the poked frames).
    daemon('get_block', { ...params, sm64_played: true }).then((blk) => {
      setB(blk);
      // The cycle this block belongs to, from its first block up to it: where
      // its run starts, and what replaying it from power-on takes.
      if (blk.block_header.height > 0) daemon('get_sm64_cycle', { hash: blk.block_header.hash }).then(setCycle).catch(() => setCycle(false));
    }).catch((e) => setErr(String(e)));
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
          <dt>Size</dt><dd>{h.block_size} bytes, plus a {b.sm64_proof_bytes}-byte proof of play</dd>
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
          <CycleInfo cycle={cycle} block={b} />
          <p>
            <button onClick={() => download(`sm64coin-block${h.height}.m64`, toM64(rawToPads(b.sm64_played_pads)))}>Download .m64: this block's run</button>{' '}
            <span className="muted">every frame as played, pokes included ({b.sm64_frames} frames; the proof stores {b.sm64_pads.length / 8} inputs)</span>
          </p>
          <p>
            <button disabled={!cycle} onClick={() => download(`sm64coin-cycle-to-block${h.height}.m64`, toM64(rawToPads(cycle.played_pads)))}>Download .m64: the whole cycle up to this block</button>{' '}
            <span className="muted">{cycle ? `from power-on, ${cycle.played_pads.length / 8} frames over ${cycle.blocks.length} block${cycle.blocks.length === 1 ? '' : 's'}; replays unperturbed (no pokes needed) to the state this block ends in` : 'loading the cycle…'}</span>
          </p>
          {!watch ? <button disabled={!cycle} onClick={() => setWatch(true)}>▶ Watch the run that sealed this block{cycle && cycle.blocks.length > 1 ? ' (the cycle before it is fast-forwarded first)' : ''}</button>
            : <RomGate><Replay block={b} cycle={cycle} /></RomGate>}
        </div>
      )}
    </>
  );
}

function CycleInfo({ cycle, block }) {
  if (cycle === null) return <p className="muted">Loading the cycle…</p>;
  if (cycle === false) return <p className="muted">The node could not give this block's cycle.</p>;
  const idx = cycle.blocks.length; // this block is the last one
  const first = cycle.blocks[0];
  return (
    <dl className="kv">
      <dt>Cycle</dt><dd>block {idx} of its cycle (started at height <Link to={`/block/${first.height}`}>{first.height}</Link>
        {first.power_on ? ', from power-on' : ''}){cycle.blocks[idx - 1].power_on && idx > 1 ? '; this block started over from power-on' : ''}</dd>
      <dt>Starts from</dt><dd>{idx === 1 || cycle.blocks[idx - 1].power_on ? 'power-on' : `the end of block ${block.block_header.height - 1}`}</dd>
      <dt>Progress after</dt><dd>{cycle.stars} ★{cycle.keys ? `, ${cycle.keys} Bowser key${cycle.keys > 1 ? 's' : ''}` : ''}{cycle.grand_star ? ', the Grand Star (the cycle ends)' : ''}</dd>
    </dl>
  );
}

function Replay({ block, cycle }) {
  const canvas = useRef(null);
  const [status, setStatus] = useState('starting…');
  const [speed, setSpeed] = useState(1);
  const speedRef = useRef(1);
  speedRef.current = speed;
  const [frame, setFrame] = useState(0);
  useEffect(() => {
    let killed = false;
    const pads = rawToPads(block.sm64_played_pads);
    let engine = null;
    daemon('get_info')
      .then(async (info) => {
        const won = successFor(info.sm64_rules);
        if (!won) throw new Error(`this page doesn't know the success condition of rules "${info.sm64_rules}" (see web/src/lib/rules.js)`);
        engine = await createEngine(canvas.current);
        // The block's start: power-on, or the end of the cycle up to its parent.
        const n = cycle.blocks.length;
        const mine = cycle.blocks[n - 1];
        if (n === 1 || mine.power_on) {
          restoreState(engine, null, engine.powerOn);
        } else {
          setStatus('fast-forwarding the cycle to where this block starts…');
          const parentFrames = cycle.blocks.slice(0, n - 1).reduce((s, x) => s + x.frames, 0);
          const upTo = { hash: cycle.blocks[n - 2].hash, blocks: cycle.blocks.slice(0, n - 1), played_pads: cycle.played_pads.slice(0, parentFrames * 8) };
          const r = await reachTip(engine, upTo, (d, t) => !killed && setStatus(`fast-forwarding the cycle to where this block starts: ${Math.floor((d / t) * 100)}%`), () => killed);
          if (!r) return null;
        }
        if (killed) return null;
        const startProgress = readProgress(engine);
        setStatus(`replaying ${pads.length} frames from ${startProgress.stars} ★ ${startProgress.keys} keys; the perturbation is re-derived and checked on every frame`);
        return playRun(engine, withConfig(block), startProgress, pads, won, async () => killed, () => speedRef.current, setFrame);
      })
      .then((o) => {
        if (killed || o == null) return;
        if (o === 'won') setStatus('★ Success condition met on the last frame: the proof replays.');
        else if (o.startsWith('mismatch')) setStatus(`✗ Frame ${o.split(':')[1]} does not match the perturbation.`);
        else setStatus('The replay ended without success.');
      })
      .catch((e) => !killed && setStatus(`Replay failed: ${e}`));
    return () => { killed = true; if (engine) engine.set_audio_enabled(0); };
  }, [block, cycle]);
  return (
    <>
      <canvas ref={canvas} className="sm64canvas" />
      <p className="muted">{status} · frame {frame}/{block.sm64_frames} ·{' '}
        {[1, 2, 4, 8].map((s) => <button key={s} disabled={s === speed} onClick={() => setSpeed(s)}>{s}×</button>)}
      </p>
    </>
  );
}
