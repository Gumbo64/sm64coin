import React, { useEffect, useMemo, useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { CycleStrip, eventName } from '../lib/CycleTimeline.jsx';
import LimitChart from '../lib/LimitChart.jsx';
import { Toolbar, ToolBtn, GearBtn } from '../components/ui.jsx';
import { runsAt, groupRuns, limitShift, replayOptions } from '../lib/blockRuns.js';
import { blockProof, splitProof, SIG_BYTES } from '../lib/replay/core.js';
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

// Facts read from the block's own bytes (get_block's `blob`): the proof's start kind and codec
// and the miner's signature that ends it. {} when the blob is missing or unreadable.
function blobFacts(blk) {
  try {
    const { start, codec } = splitProof(blockProof(blk));
    const sig = blk.blob.slice(blk.blob.length - SIG_BYTES * 2);
    return { start: start === 1 ? 'power-on' : "the parent's end state", codec, sig };
  } catch { return {}; }
}
const CODECS = ['raw', 'run-length', 'deltas'];
const payoutKey = (json) => json?.miner_tx?.vout?.[0]?.target?.tagged_key?.key || json?.miner_tx?.vout?.[0]?.target?.key || null;

export default function Block() {
  const { id } = useParams();
  const [params] = useSearchParams();
  const replaying = params.has('replay');
  const [b, setB] = useState(null);
  const [err, setErr] = useState(null);
  const [cycle, setCycle] = useState(null); // get_sm64_cycle up to this block
  const [env, setEnv] = useState(null); // {parentTs, alts}: what the envelope graph needs besides the block

  useEffect(() => {
    // `live`: answers for a block we've since navigated away from are dropped.
    let live = true;
    setB(null); setErr(null); setCycle(null); setEnv(null);
    const params = /^\d+$/.test(id) ? { height: Number(id) } : { hash: id };
    // The block (cheap for the node), then the cycle it belongs to, from its first block up to
    // it: where its run starts, and every frame as played (pokes included). getCycle replays it
    // in this browser when it has the ROM (else the node does); this block's own played pads,
    // event and inputs are the cycle's last block.
    daemon('get_block', params).then((blk) => {
      if (!live) return;
      setB(blk);
      const h = blk.block_header;
      if (h.height > 0) {
        getCycle(h.hash)
          .then((c) => {
            if (!live) return;
            const cyc = { ...c, blocks: c.blocks || [] };
            setCycle(cyc);
            setB((x) => ({ ...x, ...ownRun(cyc, x) }));
          })
          .catch(() => live && setCycle(false));
        loadEnvelope(h).then((e) => live && setEnv(e)).catch(() => live && setEnv({ parentTs: 0, alts: [] }));
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
  const details = <BlockDetails b={b} cycle={cycle} json={json} env={env} />;
  // ?replay: the replay viewer (needs the cycle and a ROM; the details stay below while either is missing).
  // Without it the page is only the block's details, whatever has loaded.
  if (replaying && h.height > 0 && cycle) {
    return (
      <RomGate fallback={details}>
        <BlockReplay key={h.hash} block={b} cycle={cycle} panel={(view) => <BlockPanel b={b} cycle={cycle} json={json} view={view} />} />
      </RomGate>
    );
  }
  return details;
}

// The parent's timestamp and the sibling blocks the node knows at this height (an unrestricted
// node's alternate chains; a restricted one has none: the winner alone is drawn).
async function loadEnvelope(h) {
  const parent = await daemon('get_block_header_by_height', { height: h.height - 1 });
  const parentTs = parent.block_header.timestamp;
  let alts = [];
  try {
    const r = await daemon('get_alternate_chains');
    const hashes = [...new Set((r.chains || []).flatMap((c) => c.block_hashes || []))].filter((x) => x !== h.hash).slice(0, 24);
    alts = (await Promise.all(hashes.map((hash) => daemon('get_block', { hash }).catch(() => null)))).filter(Boolean);
  } catch { /* not exposed */ }
  return { parentTs, alts };
}

function BlockDetails({ b, cycle, json, env }) {
  const h = b.block_header;
  const [more, setMore] = useState(false);
  const [pick, setPick] = useState(null); // the chart's selected dot (a group of runs)
  const facts = blobFacts(b);
  const own = cycle ? cycle.blocks[cycle.blocks.length - 1] : null;
  const cycleEnd = cycle ? cycle.played_pads.length / 8 : null;
  const parentTs = env?.parentTs || 0;
  const runs = useMemo(() => (env && parentTs ? runsAt(b, parentTs, env.alts) : []), [b, env, parentTs]);
  const groups = useMemo(() => groupRuns(runs), [runs]);
  const winner = runs[0];
  const shift = own?.power_on && winner ? limitShift(h.difficulty, winner.t, b.sm64_max_frames) : 0;
  const gap = parentTs ? h.timestamp - parentTs : null;
  const key = payoutKey(json);
  const replayTo = `/block/${h.hash}?replay`;
  const dl = (what) => {
    if (what === 'run') download(`sm64coin-block${h.height}.m64`, toM64(rawToPads(b.sm64_played_pads)));
    else download(`sm64coin-cycle-to-block${h.height}.m64`, toM64(rawToPads(cycle.played_pads)));
  };
  const progress = own ? `${eventName(own.event)}; the cycle stands at ${cycle.stars} ★${cycle.keys ? `, ${cycle.keys} Bowser key${cycle.keys > 1 ? 's' : ''}` : ''}${cycle.grand_star ? ', the Grand Star (the cycle ends)' : ''}` : null;
  return (
    <>
      <div className="card">
        <div className="block-head">
          <h2>Block {h.height}</h2>
          <Toolbar label="Block actions">
            {h.height > 0 && <Link className="ui-btn" to={`/block/${h.height - 1}`} title="Previous block">‹ {h.height - 1}</Link>}
            <Link className="ui-btn" to={`/block/${h.height + 1}`} title="Next block">{h.height + 1} ›</Link>
            {h.height > 0 && (cycle
              ? <Link className="ui-btn on" to={replayTo} title="Watch this block's run in the replay player (needs your ROM)">▶ Replay</Link>
              : <ToolBtn disabled title={cycle === false ? "The node could not give this block's cycle" : 'Loading the cycle…'}>▶ Replay</ToolBtn>)}
            <GearBtn open={more} onClick={() => setMore(!more)} label="More details" />
          </Toolbar>
        </div>
        <dl className="kv">
          <dt>Height</dt><dd>{h.height}</dd>
          <dt>Hash</dt><dd className="mono">{h.hash}</dd>
          <dt>Parent</dt><dd className="mono">{h.height > 0 ? <Link to={`/block/${h.prev_hash}`}>{h.prev_hash}</Link> : '—'}</dd>
          <dt>Time</dt><dd>{h.height === 0 ? 'genesis' : when(h.timestamp)}</dd>
          {h.height > 0 && <><dt>Since parent</dt><dd>{gap === null ? '…' : `${frames(Math.max(0, gap) * 30)} (${gap} s)`}</dd></>}
          <dt>Difficulty</dt><dd>{h.difficulty}</dd>
          <dt>Reward</dt><dd>{coins(h.reward)} SM64{key && <> to <span className="mono" title={key}>{short(key, 16)}</span></>}</dd>
          {h.height > 0 && <>
            <dt>Run</dt><dd>{b.sm64_frames} frames ({frames(b.sm64_frames)}) of a {b.sm64_max_frames}-frame limit ({frames(b.sm64_max_frames)})</dd>
            <dt>Proof</dt><dd>{b.sm64_proof_bytes} bytes{facts.codec !== undefined ? `, codec ${facts.codec}${CODECS[facts.codec] ? ` (${CODECS[facts.codec]})` : ''}` : ''}</dd>
            <dt>Starts from</dt><dd>{facts.start || (own ? (own.power_on ? 'power-on' : "the parent's end state") : '…')}</dd>
            <dt>Progress</dt><dd>{progress || (cycle === false ? 'unknown' : 'loading the cycle…')}</dd>
            {facts.sig && <><dt>Signature</dt><dd className="mono" title={facts.sig}>{short(facts.sig, 24)}</dd></>}
          </>}
        </dl>
        {more && (
          <dl className="kv" style={{ marginTop: 10 }}>
            <dt>Cumulative difficulty</dt><dd>{h.cumulative_difficulty}</dd>
            <dt>Size</dt><dd>{h.block_size} bytes{b.sm64_proof_bytes ? `, plus the ${b.sm64_proof_bytes}-byte proof of play` : ''}</dd>
            <dt>Transactions</dt><dd>{h.num_txes} + coinbase <span className="mono">{short(b.miner_tx_hash, 16)}</span></dd>
            {b.tx_hashes && b.tx_hashes.map((t) => <React.Fragment key={t}><dt></dt><dd className="mono">{t}</dd></React.Fragment>)}
            <dt>Version</dt><dd>{json.major_version}.{json.minor_version}</dd>
            {h.height > 0 && <>
              <dt>Perturbation</dt><dd>{b.sm64_random_amount} poke(s) of {b.sm64_burst_length} frames per {b.sm64_window_length}, reseeded every {b.sm64_reseed_frames} frames</dd>
              <dt>Seed hash</dt><dd className="mono">{b.sm64_seed_hash}</dd>
              {facts.sig && <><dt>Signature (full)</dt><dd className="mono">{facts.sig}</dd></>}
            </>}
          </dl>
        )}
      </div>
      {h.height > 0 && (
        <div className="card block-top">
          {cycle ? <CycleStrip cycle={cycle} highlight={h.hash} />
            : <p className="muted">{cycle === false ? "The node could not give this block's cycle." : 'Loading the cycle…'}</p>}
        </div>
      )}
      {h.height > 0 && (
        <section className="card run-chart">
          <div className="viz-head"><h3>Runs at height {h.height}</h3></div>
          {env && parentTs ? (
            <>
              <LimitChart difficulty={h.difficulty} parentTs={parentTs} now={null} runs={runs} shift={shift} groups={groups}
                title="Time limit" selected={pick} onDot={(g) => setPick(pick === g ? null : g)} />
              {pick
                ? <RunPicker group={pick} ctx={{ cycleEnd, hasPads: !!b.sm64_played_pads }} onDownload={dl} onClose={() => setPick(null)} />
                : <p className="muted small">{runs.length === 1 ? 'The winning run is the only one the node knows at this height.' : `${runs.length} runs at this height.`} Click a dot to pick a replay.</p>}
            </>
          ) : <p className="muted">{env ? 'No parent timestamp for the chart.' : 'Loading the envelope…'}</p>}
        </section>
      )}
    </>
  );
}

// The replays of the runs on one dot (every run that shares the point is listed).
function RunPicker({ group, ctx, onDownload, onClose }) {
  return (
    <div className="run-picker" role="group" aria-label="Replays of the selected run">
      <div className="viz-head"><h4>{group.frames} frames ({frames(group.frames)}){group.runs.length > 1 ? `, ${group.runs.length} runs` : ''}</h4>
        <div className="viz-tools"><button type="button" className="viz-gear" aria-label="Close" title="Close" onClick={onClose}>✕</button></div></div>
      {group.runs.map((r) => (
        <div className="rp-run" key={r.id}>
          <div className="rp-who">{r.winner ? '★ Winner' : r.source === 'alt' ? 'Sibling block' : 'Included run'}
            {r.hash && <> <span className="mono muted">{short(r.hash, 12)}</span></>}</div>
          <div className="rp-opts">
            {replayOptions(r, ctx).map((o) => {
              const title = o.disabled || o.hint;
              if (o.to) return <Link key={o.key} className="ui-btn" to={o.to} title={title} aria-disabled={!!o.disabled || undefined} tabIndex={o.disabled ? -1 : undefined}>{o.label}</Link>;
              if (o.download) return <ToolBtn key={o.key} title={title} disabled={!!o.disabled} onClick={() => onDownload(o.download)}>{o.label}</ToolBtn>;
              return <ToolBtn key={o.key} title={title} disabled>{o.label}</ToolBtn>;
            })}
          </div>
        </div>
      ))}
    </div>
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
