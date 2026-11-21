import React, { useEffect, useRef, useState } from 'react';
import { Recorder } from '../lib/recorder.js';
import { daemon, wallet } from '../lib/rpc.js';
import { rawProof, seal, toM64, download, bytesToHex } from '../lib/proof.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';
import { announceMined } from '../lib/payout.js';

// The manual miner: play Super Mario 64 yourself. Each attempt is sealed to a
// block template (its seed drives the controller pokes), and a successful run
// is submitted as the block's proof of play. Opens straight onto the game:
// the reward address comes from the wallet (or the last one used).
export default function Mine() {
  return <RomGate><Miner /></RomGate>;
}

const isAddress = (a) => a.startsWith('SM6') && a.length > 90;

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
  useEffect(() => {
    if (isAddress(addr.current)) return;
    wallet('get_address', { account_index: 0 }).then((r) => setAddress(r.address)).catch(() => {});
  }, []);
  const [task, setTask] = useState(null);
  const [log, setLog] = useState([]);
  const [clock, setClock] = useState(null);
  const rec = useRef(null);
  // The last attempt, for debugging: its pads and the template it was played for.
  const [last, setLast] = useState(null);
  const pads = useRef([]);
  const say = (m) => setLog((l) => [`${new Date().toLocaleTimeString()}  ${m}`, ...l].slice(0, 50));

  useEffect(() => {
    let stopped = false;
    let head = null;
    let easierAt = 0;
    let restart = false;
    // Cheap per-frame kill check; a timer watches the chain.
    const timer = setInterval(async () => {
      try {
        const i = await daemon('get_info');
        if (head && i.top_block_hash !== head) { restart = true; say(`new block ${i.height - 1} on the chain: new template, the game restarts from power-on`); }
        if (easierAt && Date.now() / 1000 >= easierAt) { restart = true; easierAt = 0; say('the difficulty decayed: restarting on an easier task'); }
      } catch { /* daemon hiccup */ }
    }, 2000);
    const kill = async () => stopped || restart;

    (async () => {
      while (!stopped) {
        restart = false;
        if (!isAddress(addr.current)) { await new Promise((r) => setTimeout(r, 1000)); continue; }
        let t;
        try {
          const paysTo = addr.current;
          t = await daemon('get_block_template', { wallet_address: paysTo, reserve_size: 0 });
          t.paysTo = paysTo; // the template's coinbase pays this address
        } catch (e) { say(String(e)); await new Promise((r) => setTimeout(r, 3000)); continue; }
        const won = successFor(t.sm64_rules);
        if (!won) { say(`unknown rules "${t.sm64_rules}": update web/src/lib/rules.js`); return; }
        head = t.prev_hash; easierAt = t.sm64_easier_at;
        setTask(t);
        say(`block ${t.height}: seed ${t.sm64_seed}, limit ${frames(t.sm64_max_frames)}, ${t.sm64_random_amount} pokes/${t.sm64_window_length}`);
        const r = new Recorder(canvas.current, withConfig(t), won, setClock);
        rec.current = r;
        const done = await r.run(kill);
        rec.current = null;
        pads.current = r.pads;
        setLast({ template: t, pads: [...r.pads], won: !!done, result: done ? 'submitting' : r.skipped ? 'skipped' : restart ? 'interrupted' : 'out of time' });
        if (stopped) break;
        if (!done) { if (!restart && !r.skipped) say('out of time: new attempt'); continue; }
        const pads_ = done;
        try {
          await daemon('submit_block', [seal(t.blocktemplate_blob, rawProof(pads_))]);
          setLast((l) => ({ ...l, result: 'accepted' }));
          announceMined(t.height, t.expected_reward);
          say(`★ BLOCK ${t.height} ACCEPTED: ${pads_.length} frames (${frames(pads_.length)}), ${coins(t.expected_reward)} SM64 to you`);
        } catch (e) {
          setLast((l) => ({ ...l, result: `rejected: ${e}` }));
          say(`block rejected: ${e} (download the attempt below to debug)`);
        }
      }
    })();
    return () => { stopped = true; clearInterval(timer); };
  }, []);

  const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;
  const [editing, setEditing] = useState(false);
  const paying = task ? task.paysTo : address;
  const pending = task && isAddress(address) && address !== task.paysTo;

  return (
    <div className="mine">
      <canvas ref={canvas} className="sm64canvas" />
      <div className="statusbar">
        <span className="stat" title="Block being mined">{task ? `block ${task.height}` : 'connecting…'}</span>
        <span className="stat" title="Time left in this attempt">
          {clock ? `${frames(Math.max(0, clock.max - clock.frame))} left` : '—'}
        </span>
        <span className="stat" title="Playback speed">{clock ? `${clock.speed}×` : '—'}</span>
        {clock && clock.paused && <span className="stat warn">PAUSED</span>}
        {clock && clock.mode === 'catchup' && <span className="stat warn">replaying to frame {clock.target} (T to play from here)</span>}
        <span className="grow" />
        <button className="linkish" title="Click to change the reward address" onClick={() => setEditing((e) => !e)}>
          Mining for {isAddress(paying || '') ? <span className="mono paysto" title={paying}>{short(paying)}</span> : <em>no address</em>} ✎
        </button>
      </div>
      {(editing || !isAddress(address)) && (
        <div className="addrpop">
          <label>Reward address
            <input className="mono wide" value={address} onChange={(e) => setAddress(e.target.value.trim())} placeholder="SM64… (paste an address)" />
          </label>
          {!isAddress(address) && <p className="muted">Waiting for a valid address.</p>}
        </div>
      )}
      {pending && (
        <div className="notice">
          The game in progress pays {short(task.paysTo)}; the new address applies from the next attempt
          (the reward address is part of the block template the run is sealed to).{' '}
          <button onClick={() => rec.current && rec.current.skip()}>Switch now (restarts the game)</button>
        </div>
      )}
      <Controls rec={rec} />
      <p className="muted links"><a href="/wallet">Wallet &amp; leaderboard →</a> <a href="/leaderboard">Leaderboard</a></p>

      <details className="sec"><summary>How to play</summary><Help /></details>
      <details className="sec"><summary>This block</summary>
        {task ? (
          <dl className="kv">
            <dt>Block</dt><dd>{task.height}</dd>
            <dt>Difficulty</dt><dd>{task.difficulty}</dd>
            <dt>Time limit</dt><dd>{frames(task.sm64_max_frames)}</dd>
            <dt>Pokes</dt><dd>{task.sm64_random_amount} × {task.sm64_burst_length} frames per {task.sm64_window_length}</dd>
            <dt>Reward</dt><dd>{coins(task.expected_reward)} SM64</dd>
            <dt>Goal</dt><dd>{task.sm64_goal}</dd>
          </dl>
        ) : <p className="muted">No template yet.</p>}
      </details>
      <details className="sec"><summary>Debug</summary>
        {last && (
          <div className="debug">
            <p className="muted">Last attempt: {last.pads.length} frames · block {last.template.height} · {last.result}</p>
            <button onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.m64`, toM64(last.pads))}>Download .m64</button>{' '}
            <button onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.json`, JSON.stringify({
              height: last.template.height,
              difficulty: last.template.difficulty,
              seed_hash: last.template.sm64_seed_hash,
              reseed_frames: last.template.sm64_reseed_frames,
              max_frames: last.template.sm64_max_frames,
              won: last.won,
              result: last.result,
              blocktemplate_blob: last.template.blocktemplate_blob,
              pads_raw_hex: bytesToHex(rawProof(last.pads).subarray(1)),
            }, null, 1), 'application/json')}>Download debug JSON</button>
          </div>
        )}
        <pre className="log">{log.join('\n')}</pre>
      </details>
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
      <p><b>Goal:</b> from power-on, get a star within the time limit. The controller is randomly poked while you play
        (the pokes come from the block template's seed and Mario's position and speed), so every attempt is different.</p>
      <p><b>Game keys</b> (sm64-port defaults; a gamepad works too): WASD stick · <b>I</b> = A · <b>J</b> = B ·
        <b>O</b> = Z · <b>Space</b> = Start · arrows = C buttons · Q = L · Right Shift = R · D-pad: R key up, Left Shift down.</p>
      <p><b>Miner keys:</b> <b>G</b> restart the attempt · <b>Backspace</b> back 5 s (<b>Shift+Backspace</b> 30 s, <b>[</b> 1 s) ·
        <b>P</b> pause · <b>T</b> or Start take over during a replay · <b>1</b>–<b>6</b> speed ¼×, ½×, 1×, 2×, 4×, 10× ·
        hold D-pad down (Left Shift) for 10×, D-pad up (R) to go back 10 s · <b>N</b> new template.</p>
      <p>Going back replays your inputs on a fresh game (fast, then the last 3 s at normal speed): the pokes are
        deterministic, so it's exactly the same game.</p>
      <p><b>Why mining restarts when a new block is mined:</b> every attempt is sealed to one block template. The
        template builds on the current top block, and the controller pokes are seeded from it. Once a new block is on
        top, a run for the old template can't seal a block any more, so you get a fresh template (and fresh pokes) for
        the next block, and the game starts again from power-on.</p>
    </div>
  );
}
