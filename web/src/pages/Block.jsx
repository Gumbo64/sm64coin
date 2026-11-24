import React, { useEffect, useState } from 'react';
import { Link, useParams } from 'react-router';
import { CycleStrip } from '../lib/CycleTimeline.jsx';
import { daemon } from '../lib/rpc.js';
import { getCycle } from '../lib/replay/client.js';
import { rawToPads, toM64, download } from '../lib/proof.js';
import { coins, short, when, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';
import BlockReplay from './BlockReplay.jsx';

// This block's run from its cycle (the last block): every frame's played pad, what it added, and
// the proof's inputs (the non-poked frames; a stateless proof's are in get_block already).
function ownRun(cycle, blk) {
  const last = cycle.blocks[cycle.blocks.length - 1];
  if (!last) return {};
  const played = cycle.played_pads.slice(cycle.played_pads.length - last.frames * 8);
  let wanted = blk.sm64_pads || '';
  const poked = cycle.poked?.[cycle.poked.length - 1];
  if (!wanted && poked) {
    const parts = [];
    for (let i = 0; i < last.frames; i++) if (!poked[i]) parts.push(played.substr(i * 8, 8));
    wanted = parts.join('');
  }
  return { sm64_played_pads: played, sm64_pads: wanted, sm64_event: last.event };
}

export default function Block() {
  const { id } = useParams();
  const [b, setB] = useState(null);
  const [err, setErr] = useState(null);
  const [cycle, setCycle] = useState(null); // get_sm64_cycle up to this block

  useEffect(() => {
    // `live`: answers for a block we've since navigated away from are dropped.
    let live = true;
    setB(null); setErr(null); setCycle(null);
    const params = /^\d+$/.test(id) ? { height: Number(id) } : { hash: id };
    // The block (cheap for the node), then the cycle it belongs to, from its first block up to
    // it: where its run starts, and every frame as played (pokes included). getCycle replays it
    // in this browser when it has the ROM (else the node does); this block's own played pads,
    // event and inputs are the cycle's last block.
    daemon('get_block', params).then((blk) => {
      if (!live) return;
      setB(blk);
      if (blk.block_header.height > 0) {
        getCycle(blk.block_header.hash)
          .then((c) => {
            if (!live) return;
            const cyc = { ...c, blocks: c.blocks || [] };
            setCycle(cyc);
            setB((x) => ({ ...x, ...ownRun(cyc, x) }));
          })
          .catch(() => live && setCycle(false));
      } else setCycle(false);
    }).catch((e) => live && setErr(String(e)));
    return () => { live = false; };
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
  const classic = (
    <>
      {h.height > 0 && (
        <div className="card block-top">
          {cycle ? <CycleStrip cycle={cycle} highlight={h.hash} />
            : <p className="muted">{cycle === false ? "The node could not give this block's cycle." : 'Loading the cycle…'}</p>}
        </div>
      )}
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
            <button disabled={!b.sm64_played_pads} onClick={() => download(`sm64coin-block${h.height}.m64`, toM64(rawToPads(b.sm64_played_pads)))}>Download .m64: this block's run</button>{' '}
            <span className="muted">every frame as played, pokes included ({b.sm64_frames} frames{b.sm64_pads ? `; the proof stores ${b.sm64_pads.length / 8} inputs` : ''})</span>
          </p>
          <p>
            <button disabled={!cycle} onClick={() => download(`sm64coin-cycle-to-block${h.height}.m64`, toM64(rawToPads(cycle.played_pads)))}>Download .m64: the whole cycle up to this block</button>{' '}
            <span className="muted">{cycle ? `from power-on, ${cycle.played_pads.length / 8} frames over ${cycle.blocks.length} block${cycle.blocks.length === 1 ? '' : 's'}; replays unperturbed (no pokes needed) to the state this block ends in` : 'loading the cycle…'}</span>
          </p>
        </div>
      )}
    </>
  );
  // With the cycle and a ROM the whole page is the replay viewer, and what the
  // cards show goes into its side panel.
  if (h.height > 0 && cycle) {
    return (
      <RomGate fallback={classic}>
        <BlockReplay key={h.hash} block={b} cycle={cycle} panel={(view) => <BlockPanel b={b} cycle={cycle} json={json} view={view} />} />
      </RomGate>
    );
  }
  return classic;
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

// The block's details for the viewer's side panel (where the viewer lists regions).
function BlockPanel({ b, cycle, json, view }) {
  const h = b.block_header;
  const v = view?.verify;
  let verdict = 'starting…';
  if (v) {
    if (v.bad !== null) verdict = `✗ Frame ${v.bad - v.start} of the block does not match the perturbation.`;
    else if (v.okTo === v.total) verdict = v.wonEnd ? '★ ✓ Success condition met on the last frame: the proof replays.' : 'The replay ended without success.';
    else if (v.unchecked) verdict = `jumped into the block (headless): ${v.okTo - v.start} / ${v.total - v.start} of its frames checked so far; play from the block's start to check the rest`;
    else verdict = `checking every frame: ${v.okTo - v.start} / ${v.total - v.start} of the block's frames verified`;
  }
  return (
    <div className="replay-info">
      <h3>Block {h.height}</h3>
      <p className={`verdict small${v && v.bad !== null ? ' bad' : v && v.okTo === v.total && v.wonEnd ? ' good' : ''}`}>{verdict}</p>
      <dl className="kv small">
        <dt>Hash</dt><dd className="mono">{h.hash}</dd>
        <dt>Previous</dt><dd className="mono"><Link to={`/block/${h.prev_hash}`}>{short(h.prev_hash, 16)}</Link></dd>
        <dt>Next</dt><dd><Link to={`/block/${h.height + 1}`}>block {h.height + 1}</Link></dd>
        <dt>Time</dt><dd>{when(h.timestamp)}</dd>
        <dt>Difficulty</dt><dd>{h.difficulty} (cumulative {h.cumulative_difficulty})</dd>
        <dt>Reward</dt><dd>{coins(h.reward)} SM64</dd>
        <dt>Size</dt><dd>{h.block_size} bytes + a {b.sm64_proof_bytes}-byte proof of play</dd>
        <dt>Transactions</dt><dd>{h.num_txes} + coinbase <span className="mono">{short(b.miner_tx_hash, 16)}</span></dd>
        <dt>Version</dt><dd>{json.major_version}.{json.minor_version}</dd>
      </dl>
      <h3>Proof of play</h3>
      <dl className="kv small">
        <dt>Run length</dt><dd>{b.sm64_frames} frames ({frames(b.sm64_frames)}), limit {b.sm64_max_frames} ({frames(b.sm64_max_frames)})</dd>
        <dt>Perturbation</dt><dd>{b.sm64_random_amount} poke(s) of {b.sm64_burst_length} frames per {b.sm64_window_length}, reseeded every {b.sm64_reseed_frames} frames</dd>
        <dt>Seed hash</dt><dd className="mono">{b.sm64_seed_hash}</dd>
      </dl>
      <h3>Cycle</h3>
      <CycleInfo cycle={cycle} block={b} />
      <h3>Downloads</h3>
      <p className="small">
        <button disabled={!b.sm64_played_pads} onClick={() => download(`sm64coin-block${h.height}.m64`, toM64(rawToPads(b.sm64_played_pads)))}>.m64: this block's run</button>
        <br /><span className="muted">every frame as played, pokes included</span>
      </p>
      <p className="small">
        <button onClick={() => download(`sm64coin-cycle-to-block${h.height}.m64`, toM64(rawToPads(cycle.played_pads)))}>.m64: the whole cycle</button>
        <br /><span className="muted">{cycle.played_pads.length / 8} frames over {cycle.blocks.length} block{cycle.blocks.length === 1 ? '' : 's'}, unperturbed</span>
      </p>
    </div>
  );
}
