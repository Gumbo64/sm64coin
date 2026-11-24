import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { Recorder } from '../lib/recorder.js';
import CycleTimeline from '../lib/CycleTimeline.jsx';
import LimitChart from '../lib/LimitChart.jsx';
import { daemon } from '../lib/rpc.js';
import { rawProof, proofFromRun, seal, toM64, download, bytesToHex, withTimestamp, templateTimestamp } from '../lib/proof.js';
import { createEngine, prepareStart } from '../lib/cycle.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';
import { announceMined } from '../lib/payout.js';

// The manual miner: play Super Mario 64 yourself. A block is the game's next
// progress point (a star, a Bowser key, the Grand Star after 70 stars), played
// on from where the previous block ended (the "70-star cycle"). Each attempt is
// sealed to a block template (its seed drives the controller pokes), and a
// successful run is submitted as the block's proof of play. Opens straight onto the game:
// the reward address comes from the wallet app's "Play to mine" link
// (?address=), or is pasted (and remembered).
export default function Mine() {
  return <RomGate><Miner /></RomGate>;
}

const isAddress = (a) => a.startsWith('SM6') && a.length > 90;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;

function Miner() {
  const canvas = useRef(null);
  // ?address= (the GUI wallet's "Play to mine" link) beats the stored address.
  const [address, setAddress] = useState(() => {
    const q = new URLSearchParams(window.location.search).get('address');
    return q && isAddress(q.trim()) ? q.trim() : localStorage.getItem('sm64coin.address') || '';
  });
  const addr = useRef(address);
  addr.current = address;
  useEffect(() => {
    if (isAddress(address)) localStorage.setItem('sm64coin.address', address);
  }, [address]);
  const [task, setTask] = useState(null);
  const [log, setLog] = useState([]);
  const [clock, setClock] = useState(null);
  // What the miner is doing: {kind: 'loading'|'ff'|'play'|'waiting'|'submitting', ...}
  const [phase, setPhase] = useState({ kind: 'loading' });
  // The start of the current attempt: {kind, progress, cycle}, and the chain's start mode.
  const [start, setStart] = useState(null);
  const [startMode, setStartMode] = useState('parent');
  const [limit, setLimit] = useState(null);
  // Runs finished on this tip (the limit chart's dots): {t: seconds after the parent, frames}.
  const [runs, setRuns] = useState([]);
  // The run over the limit waiting for it (the auction): {validAt, frames}.
  const [pendingRun, setPending] = useState(null);
  const [tick, setTick] = useState(0);
  const rec = useRef(null);
  const ctl = useRef({ restart: false, forcePower: false, discard: false, pending: null, discardPending: false });
  // The last attempt, for debugging: its pads and the template it was played for.
  const [last, setLast] = useState(null);
  const say = (m) => setLog((l) => [`${new Date().toLocaleTimeString()}  ${m}`, ...l].slice(0, 50));

  useEffect(() => {
    const id = setInterval(() => setTick((x) => x + 1), 1000); // countdowns
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    let stopped = false;
    let head = null;
    const c = ctl.current;
    const setLim = (n) => { setLimit(n); if (rec.current) rec.current.setLimit(n); };
    // Cheap per-frame kill check; timers watch the chain.
    const timer = setInterval(async () => {
      try {
        const i = await daemon('get_info');
        if (head && i.top_block_hash !== head) { c.restart = true; say(`new block ${i.height - 1} on the chain: new template; your cycle continues from its end`); }
      } catch { /* daemon hiccup */ }
    }, 2000);
    // The time limit grows every second from the parent's timestamp, and the chain
    // may become stalled: re-read both from a fresh template every ~5 s while
    // the tip is unchanged (only these fields: this attempt keeps its own template).
    let refreshing = false; // one template request at a time (a busy node answers slower than 5 s)
    const refresh = setInterval(async () => {
      if (!head || !isAddress(addr.current) || refreshing) return;
      refreshing = true;
      try {
        const t = await daemon('get_block_template', { wallet_address: addr.current, reserve_size: 0 });
        if (t.prev_hash !== head) return;
        c.tipTemplate = t;
        setLim(t.sm64_max_frames);
        setStartMode(t.sm64_start);
      } catch { /* daemon hiccup */ } finally { refreshing = false; }
    }, 5000);
    const kill = async () => stopped || c.restart;
    // Seals and submits a finished run on its own template (a newer one has other
    // mempool transactions, which would change the seed), stamped when it is valid.
    const submitRun = async (r) => {
      const { t, done, st } = r;
      try {
        let ts = Math.max(Math.floor(Date.now() / 1000), r.validAt, templateTimestamp(t.blocktemplate_blob));
        if (st.kind === 'power-on' && t.sm64_start === 'either' && t.sm64_power_on_at) ts = Math.max(ts, t.sm64_power_on_at);
        const blob = withTimestamp(t.blocktemplate_blob, ts);
        await daemon('submit_block', [seal(blob, proofFromRun(done.pads, done.poked, st.kind === 'power-on'))]);
        setLast((l) => ({ ...l, result: 'accepted' }));
        announceMined(t.height, t.expected_reward);
        say(ts > Date.now() / 1000 + 1
          ? `★ BLOCK ${t.height} SENT: ${r.frames} frames (${frames(r.frames)}); nodes hold it until ${new Date(ts * 1000).toLocaleTimeString()}, when it fits the limit`
          : `★ BLOCK ${t.height} ACCEPTED: ${r.frames} frames (${frames(r.frames)}), ${coins(t.expected_reward)} SM64 to you`);
      } catch (e) {
        setLast((l) => ({ ...l, result: `rejected: ${e}` }));
        say(`block rejected: ${e} (download the attempt below to debug)`);
      }
    };
    // The pending run (over the limit when finished): sent once its valid time is within the
    // nodes' future-timestamp window (CRYPTONOTE_BLOCK_FUTURE_TIME_LIMIT, 120 s); dropped if a
    // new block lands first.
    const SEND_AHEAD = 100;
    const pendingTimer = setInterval(async () => {
      const p = c.pending;
      if (!p) return;
      if (head && p.t.prev_hash !== head) {
        c.pending = null; setPending(null);
        say('A new block landed before your pending run became valid, so it can no longer be a block (it was played for the old tip).');
        return;
      }
      if (c.discardPending) { c.discardPending = false; c.pending = null; setPending(null); say('pending run discarded'); return; }
      if (p.validAt - Date.now() / 1000 > SEND_AHEAD || p.sending) return;
      p.sending = true;
      await submitRun(p);
      if (c.pending === p) { c.pending = null; setPending(null); }
    }, 1000);

    (async () => {
      let engine;
      try { engine = await createEngine(canvas.current); } catch (e) { say(`could not start the game: ${e}`); return; }
      if (stopped) { engine.set_audio_enabled(0); return; }
      while (!stopped) {
        c.restart = false; c.discard = false;
        if (!isAddress(addr.current)) { setPhase({ kind: 'loading' }); await sleep(1000); continue; }
        let t;
        try {
          const paysTo = addr.current;
          t = await daemon('get_block_template', { wallet_address: paysTo, reserve_size: 0 });
          t.paysTo = paysTo; // the template's coinbase pays this address
        } catch (e) { say(String(e)); await sleep(3000); continue; }
        const won = successFor(t.sm64_rules);
        if (!won) { say(`unknown rules "${t.sm64_rules}": update web/src/lib/rules.js`); return; }
        if (head !== t.prev_hash) { c.forcePower = false; setRuns([]); }
        head = t.prev_hash;
        c.tipTemplate = t;
        setTask(t); setLim(t.sm64_max_frames); setStartMode(t.sm64_start);
        const power = c.forcePower && t.sm64_start === 'either';
        say(`block ${t.height}: seed ${t.sm64_seed}, limit ${frames(t.sm64_max_frames)}, ${t.sm64_random_amount} pokes/${t.sm64_window_length}, start ${power ? 'power-on (restart)' : t.sm64_start}`);

        // The start state: power-on, or the parent block's end (fast-forward the cycle).
        setPhase({ kind: 'ff', done: 0, total: 0 });
        let st;
        try {
          st = await prepareStart(engine, t, power, (done, total) => setPhase({ kind: 'ff', done, total }), () => stopped);
        } catch (e) { say(`could not reach the start: ${e}`); await sleep(3000); continue; }
        if (!st) break;
        setStart(st);
        if (st.cycle) say(`cycle replayed to the tip: ${st.progress.stars} stars, ${st.progress.keys} keys (${st.cycle.blocks.length} blocks)`);
        if (c.restart) continue; // a block landed meanwhile: the cache makes the next start quick
        setPhase({ kind: 'play' });
        const r = new Recorder(engine, withConfig(t), st, won, setClock);
        r.setLimit(c.tipTemplate && c.tipTemplate.prev_hash === head ? c.tipTemplate.sm64_max_frames : t.sm64_max_frames);
        rec.current = r;
        const done = await r.run(kill);
        rec.current = null;
        const result = done ? 'won' : r.skipped ? 'skipped' : c.restart ? 'interrupted' : 'out of frames';
        setLast({ template: t, pads: [...r.pads], won: !!done, result });
        if (stopped) break;
        if (!done) { if (!c.restart && !r.skipped) say('ran past the longest possible time limit: new attempt'); continue; }
        const nFrames = done.pads.length;
        say(`goal reached after ${nFrames} frames (${frames(nFrames)})`);
        setRuns((rs) => [...rs, { t: Date.now() / 1000 - t.sm64_parent_timestamp, frames: nFrames }]);

        // When does a run this long become valid? (The limit grows with time.)
        let validAt = 0;
        try {
          const q = await daemon('get_block_template', { wallet_address: t.paysTo, reserve_size: 0, sm64_frames: nFrames });
          validAt = q.sm64_valid_at || 0;
        } catch (e) { say(`could not ask when the run is valid: ${e}`); }
        if (stopped) break;
        if (validAt > Date.now() / 1000) {
          // Over the current limit: the auction. Keep it as the pending run (the one that becomes
          // valid soonest) and play on for a shorter one; it is sent, stamped with its valid
          // time, once that is within the nodes' future-timestamp window (they hold it until then).
          const p = c.pending;
          if (p && p.t.prev_hash === t.prev_hash && p.validAt <= validAt) {
            say(`over the limit, and your pending run (${frames(p.frames)}) becomes valid sooner: this one is dropped. Play on for a shorter run.`);
          } else {
            c.pending = { t, done, st, validAt, frames: nFrames };
            setPending({ validAt, frames: nFrames });
            say(`over the time limit by ${nFrames - (c.tipTemplate ? c.tipTemplate.sm64_max_frames : 0)} frames: kept as your pending run, valid at ${new Date(validAt * 1000).toLocaleTimeString()} and sent automatically then. Play on: a shorter run becomes valid sooner.`);
          }
          continue;
        }
        setPhase({ kind: 'submitting' });
        await submitRun({ t, done, st, validAt, frames: nFrames });
      }
      engine.set_audio_enabled(0);
    })();
    return () => { stopped = true; clearInterval(timer); clearInterval(refresh); clearInterval(pendingTimer); };
  }, []);

  const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;
  const [editing, setEditing] = useState(false);
  const paying = task ? task.paysTo : address;
  const pending = task && isAddress(address) && address !== task.paysTo;
  const used = clock ? clock.frame : 0;
  const lim = clock ? clock.limit : limit;
  const over = lim && used > lim;

  return (
    <>
    <CycleTimeline attempt={clock ? clock.frame : 0} limit={lim || 0} />
    <div className="mine2">
      <div className="gamecol">
        <canvas ref={canvas} className="sm64canvas" />
        <div className="budget" title="Frames you have used of the current time limit. The limit grows from the last block's timestamp up to 20 minutes; you can play past it and wait for it to catch up.">
          <div className={`bar${over ? ' over' : ''}`}><div className="fill" style={{ width: `${lim ? Math.min(100, (used / lim) * 100) : 0}%` }} /></div>
          <span className="stat">{lim ? `${used} / ${lim} frames (${frames(used)} / ${frames(lim)})` : '—'}{over ? ` — ${used - lim} over: it becomes valid when the limit has grown to your run` : ''}</span>
        </div>
        <div className="statusbar">
          <span className="stat" title="Time left before your run is over the current limit">
            {clock && lim ? (over ? 'over budget' : `${frames(Math.max(0, lim - used))} left`) : '—'}
          </span>
          <span className="stat" title="Playback speed">{clock ? `${clock.speed}×` : '—'}</span>
          {clock && clock.paused && <span className="stat warn">PAUSED</span>}
          {clock && clock.mode === 'catchup' && phase.kind === 'play' && <span className="stat warn">replaying to frame {clock.target} (T to play from here)</span>}
          {phase.kind === 'ff' && <span className="stat warn">fast-forwarding the cycle{phase.total ? `: ${Math.floor((phase.done / phase.total) * 100)}% (${phase.done} / ${phase.total} frames)` : '…'}</span>}
          {pendingRun && <span className="stat warn">pending run {frames(pendingRun.frames)}: valid in {mmss(Math.max(0, pendingRun.validAt - Date.now() / 1000))} (sent automatically; play on for a shorter one)
            {' '}<button onClick={() => { ctl.current.discardPending = true; }}>Discard</button></span>}
          {phase.kind === 'submitting' && <span className="stat warn">submitting…</span>}
        </div>
        <Controls rec={rec} />
      </div>

      <aside className="card side">
        {task && <LimitChart difficulty={task.sm64_retarget_difficulty} parentTs={task.sm64_parent_timestamp} now={Date.now() / 1000 + tick * 0} used={used} runs={runs} />}
        <div className="paysto-row">
          Mining for {isAddress(paying || '') ? <span className="mono paysto" title={paying}>{short(paying)}</span> : <em>no address</em>}{' '}
          <button className="linkish" title="Change the reward address" onClick={() => setEditing((e) => !e)}>✎ change</button>
        </div>
        {(editing || !isAddress(address)) && (
          <div className="addrpop">
            <input className="mono wide" value={address} onChange={(e) => setAddress(e.target.value.trim())} placeholder="SM64… (paste an address)" />
            {!isAddress(address) && <p className="muted">Paste your address (in the SM64Coin wallet: Receive), or open
              this page from the wallet's Mining → Play to mine.</p>}
          </div>
        )}
        {pending && (
          <div className="notice">
            The game in progress pays {short(task.paysTo)}; the new address applies from the next attempt
            (the reward address is part of the block template the run is sealed to).{' '}
            <button onClick={() => rec.current && rec.current.skip()}>Switch now (restarts the attempt)</button>
          </div>
        )}
        {task && start ? <CyclePanel task={task} start={start} /> : null}
        {start && start.kind === 'parent' && startMode === 'either' && (
          <div className="notice">
            No block for over an hour: the chain has stalled, so a block may also start over from power-on.{' '}
            <button onClick={() => { ctl.current.forcePower = true; ctl.current.restart = true; }}>Start over from power-on (the chain has stalled)</button>
          </div>
        )}
        {task ? (
          <dl className="kv">
            <dt>Block</dt><dd>{task.height}</dd>
            <dt>Difficulty</dt><dd>{task.difficulty}</dd>
            <dt>Time limit now</dt><dd>{frames(limit ?? task.sm64_max_frames)} ({limit ?? task.sm64_max_frames} frames)</dd>
            <dt>Pokes</dt><dd>{task.sm64_random_amount} × {task.sm64_burst_length} frames per {task.sm64_window_length}</dd>
            <dt>Reward</dt><dd>{coins(task.expected_reward)} SM64</dd>
          </dl>
        ) : <p className="muted">Getting a block template…</p>}
        <details className="sec"><summary>How to play</summary><Help /></details>
        <details className="sec"><summary>Debug</summary>
          {last && (
            <div className="debug">
              <p className="muted">Last attempt: {last.pads.length} frames · block {last.template.height} · {last.result}</p>
              <button onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.m64`, toM64(last.pads))}>Download .m64 (this block's run)</button>{' '}
              <button onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.json`, JSON.stringify({
                height: last.template.height,
                difficulty: last.template.difficulty,
                seed_hash: last.template.sm64_seed_hash,
                reseed_frames: last.template.sm64_reseed_frames,
                max_frames: last.template.sm64_max_frames,
                start: last.template.sm64_start,
                won: last.won,
                result: last.result,
                blocktemplate_blob: last.template.blocktemplate_blob,
                pads_raw_hex: bytesToHex(rawProof(last.pads).subarray(1)),
              }, null, 1), 'application/json')}>Download debug JSON</button>
            </div>
          )}
          <pre className="log">{log.join('\n')}</pre>
        </details>
        <p className="muted links"><Link to="/leaderboard">Leaderboard</Link></p>
        <p className="muted">Rather not play by hand? <Link to="/bots">Build a bot</Link> to mine for you.</p>
      </aside>
    </div>
    </>
  );
}

// Where in the cycle you are, and what to do.
function CyclePanel({ task, start }) {
  const p = start.progress;
  const c = start.cycle;
  const n = c ? c.blocks.length + 1 : 1;
  const height = task.height;
  return (
    <div className="cyclebox">
      <div><b>{start.kind === 'power-on' ? 'A new cycle: from power-on' : `Block ${n} of the cycle`}</b>
        {' '}<span className="muted">(height {height}{c ? `, cycle started at height ${task.sm64_cycle_start_height}` : ''})</span></div>
      <div>So far: <b>{p.stars} ★</b> {p.keys > 0 ? <>and <b>{p.keys} Bowser key{p.keys > 1 ? 's' : ''}</b></> : null}
        {' '}({p.points} point{p.points === 1 ? '' : 's'}){p.stars >= 70 ? ' — the Grand Star is next' : ''}</div>
      <div><b>Goal:</b> {p.stars >= 70 && !p.grand ? 'get the Grand Star (the cycle ends there)' : 'get your next star or key'}.</div>
    </div>
  );
}

const SPEEDS = [0.25, 0.5, 1, 2, 4, 10];

// On-screen buttons and keyboard shortcuts for the recorder (keys the game
// doesn't use).
function Controls({ rec }) {
  const [, force] = useState(0);
  useEffect(() => {
    const onKey = (e) => {
      const r = rec.current;
      if (!r || e.target.tagName === 'INPUT') return;
      const k = e.key;
      if (k === 'g' || k === 'G') r.restart();
      else if (k === 'Backspace') { e.preventDefault(); r.back(e.shiftKey ? 30 : 5); }
      else if (k === '[') r.back(1);
      else if (k === 'p' || k === 'P') r.togglePause();
      else if (k === 't' || k === 'T') r.takeOver();
      else if (k === 'n' || k === 'N') r.skip();
      else if (k >= '1' && k <= '6') r.setSpeed(SPEEDS[Number(k) - 1]);
      else return;
      force((x) => x + 1);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [rec]);
  const act = (f) => () => { if (rec.current) { f(rec.current); force((x) => x + 1); } };
  const btn = (label, tip, f, on) => (
    <button key={tip} title={tip} aria-label={tip} className={on ? 'on' : ''} onClick={act(f)}>{label}</button>
  );
  return (
    <div className="controls">
      {btn('⟲', 'Restart the attempt (G)', (r) => r.restart())}
      {[[1, '[ '], [5, 'Backspace'], [10, 'D-pad up'], [30, 'Shift+Backspace']].map(([s, k]) =>
        btn(`◀${s}s`, `Back ${s} s (${k.trim()})`, (r) => r.back(s)))}
      {btn('II', 'Pause / resume (P)', (r) => r.togglePause())}
      {btn('▶ play', 'Take over during a replay (T)', (r) => r.takeOver())}
      <span className="sep" />
      {SPEEDS.map((s, i) => btn(`${s}×`, `Speed ${s}× (${i + 1})`, (r) => r.setSpeed(s), rec.current && rec.current.speed === s))}
      <span className="sep" />
      {btn('▶| new', 'New template: restart on a fresh block template (N)', (r) => r.skip())}
    </div>
  );
}

function Help() {
  return (
    <div className="help muted">
      <p><b>The cycle:</b> a block is the game's next progress point: a new <b>star</b>, a <b>Bowser key</b>, or the
        <b> Grand Star</b> (once you have 70 stars). You play on from where the previous block ended, so the game
        carries on through the whole 70-star run, one block at a time, whoever mines them. The Grand Star ends the
        cycle and the next block starts again from power-on. When you open this page the game is brought to the
        chain's current state first (the cycle's inputs are replayed fast; you'll see a progress bar), then you play
        your block from there. <b>Goal: get your next star or key</b> within the time limit.</p>
      <p>The controller is randomly poked while you play (the pokes come from the block template's seed and Mario's
        position and speed), so every attempt is different. The pokes are the same at every difficulty.</p>
      <p><b>The time limit grows continuously.</b> Difficulty is only the time limit. From the moment the last block
        was found the limit grows <i>every second</i>, so the shortest run becomes valid first and runs never have to
        restart because the difficulty changed. You can keep playing past the limit (the bar turns red): if you reach the goal over budget, the run
        isn't lost, it just has to <b>wait</b> until the limit has grown to its length, shown as "valid in mm:ss", and
        is submitted automatically then, unless another block lands first (a run is for one parent block; then you
        start the next block from that one). Ties go to the shorter run: of several runs waiting, the shortest
        becomes valid first.</p>
      <p><b>If the chain stalls</b> (no block for an hour, e.g. nobody can get past a hard spot), a block may start over
        from power-on instead: a button then appears here ("Start over from power-on"). The Grand Star also ends the
        cycle by itself.</p>
      <p><b>Game keys</b> (sm64-port defaults; a gamepad works too): WASD stick · <b>I</b> = A · <b>J</b> = B ·
        <b>O</b> = Z · <b>Space</b> = Start · arrows = C buttons · Q = L · Right Shift = R · D-pad: R key up, Left Shift down.</p>
      <p><b>Miner keys:</b> <b>G</b> restart the attempt (from the block's start) · <b>Backspace</b> back 5 s (<b>Shift+Backspace</b> 30 s, <b>[</b> 1 s) ·
        <b>P</b> pause · <b>T</b> or Start take over during a replay · <b>1</b>–<b>6</b> speed ¼×, ½×, 1×, 2×, 4×, 10× ·
        hold D-pad down (Left Shift) for 10× , D-pad up (R) to go back 10 s · <b>N</b> new template.</p>
      <p>Going back is instant: the game is saved every 5 s of play (in the browser), so it restores the nearest save
        and replays your inputs from there (fast, then the last 3 s at normal speed). The pokes are deterministic, so
        it is exactly the same game.</p>
      <p><b>Why mining restarts when a new block is mined:</b> every attempt is sealed to one block template. The
        template builds on the current top block, and the controller pokes are seeded from it. Once a new block is on
        top, a run for the old template can't seal a block any more, so you get a fresh template (and fresh pokes) for
        the next block, starting from the new block's end state.</p>
    </div>
  );
}
