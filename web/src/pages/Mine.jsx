import React, { useEffect, useRef, useState } from 'react';
import { Recorder } from '../lib/recorder.js';
import { daemon, wallet } from '../lib/rpc.js';
import { rawProof, seal, toM64, download, bytesToHex } from '../lib/proof.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, frames } from '../lib/format.js';
import RomGate from '../lib/RomGate.jsx';
import { announceMined } from '../lib/payout.js';
import useInterval from '../lib/useInterval.js';

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
  const [address, setAddress] = useState(localStorage.getItem('sm64coin.address') || '');
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
          t = await daemon('get_block_template', { wallet_address: addr.current, reserve_size: 0 });
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

  return (
    <div className="mine">
      <div>
        <canvas ref={canvas} className="sm64canvas" />
        {clock && (
          <div className="clock">
            frame {clock.frame} · {frames(Math.max(0, clock.max - clock.frame))} left · {clock.speed}×
            {clock.paused && ' · PAUSED'}
            {clock.mode === 'catchup' && ` · replaying to frame ${clock.target} (take over to play from here)`}
          </div>
        )}
        <Controls rec={rec} />
        <Help />
      </div>
      <div className="card side">
        <label>Reward address
          <input className="mono wide" value={address} onChange={(e) => setAddress(e.target.value.trim())} placeholder="SM64… (open a wallet, or paste an address)" />
        </label>
        {!isAddress(address) && <p className="muted">Waiting for an address: open a wallet on the Wallet page or paste one. Changes apply from the next attempt.</p>}
        <Winnings address={address} />
        {task && (
          <dl className="kv">
            <dt>Block</dt><dd>{task.height}</dd>
            <dt>Difficulty</dt><dd>{task.difficulty}</dd>
            <dt>Time limit</dt><dd>{frames(task.sm64_max_frames)}</dd>
            <dt>Pokes</dt><dd>{task.sm64_random_amount} × {task.sm64_burst_length} frames per {task.sm64_window_length}</dd>
            <dt>Reward</dt><dd>{coins(task.expected_reward)} SM64</dd>
            <dt>Goal</dt><dd>{task.sm64_goal}</dd>
          </dl>
        )}
        {last && (
          <div className="debug">
            <h4>Last attempt</h4>
            <p className="muted">{last.pads.length} frames · block {last.template.height} · {last.result}</p>
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
      </div>
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
  return (
    <div className="controls">
      <button onClick={act((r) => r.restart())}>⟲ Restart</button>
      {[1, 5, 10, 30].map((s) => <button key={s} onClick={act((r) => r.back(s))}>◀ {s} s</button>)}
      <button onClick={act((r) => r.togglePause())}>⏯ Pause</button>
      <button onClick={act((r) => r.takeOver())}>✋ Take over</button>
      <span className="sep" />
      {SPEEDS.map((s) => <button key={s} className={rec.current && rec.current.speed === s ? 'on' : ''} onClick={act((r) => r.setSpeed(s))}>{s}×</button>)}
      <span className="sep" />
      <button onClick={act((r) => r.skip())}>⏭ New template</button>
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

// Balance and past winnings of the open wallet (the wallet RPC), shown when it
// is the reward address. Mined coins unlock after 10 blocks.
function Winnings({ address }) {
  const [w, setW] = useState(null);
  useInterval(async () => {
    try {
      const [a, b, t] = await Promise.all([
        wallet('get_address', { account_index: 0 }),
        wallet('get_balance', { account_index: 0 }),
        wallet('get_transfers', { in: true }),
      ]);
      const blocks = (t.in || []).filter((x) => x.type === 'block').sort((x, y) => y.height - x.height);
      setW({ address: a.address, balance: b.balance, unlocked: b.unlocked_balance, toUnlock: b.blocks_to_unlock, blocks });
    } catch { setW(null); }
  }, 5000);
  if (!w) return <div className="card inner"><p className="muted">Open a wallet (Wallet page) to see your balance and winnings here.</p></div>;
  const same = w.address === address;
  const total = w.blocks.reduce((s, x) => s + x.amount, 0);
  return (
    <div className="card inner">
      <h4>{same ? 'Your winnings' : "Open wallet's winnings (not the reward address above)"}</h4>
      <dl className="kv">
        <dt>Balance</dt><dd>{coins(w.balance)} SM64</dd>
        <dt>Spendable</dt><dd>{coins(w.unlocked)} SM64{w.toUnlock > 0 && ` (rest unlocks in ${w.toUnlock} blocks)`}</dd>
        <dt>Blocks mined</dt><dd>{w.blocks.length} · {coins(total)} SM64 in total</dd>
      </dl>
      {w.blocks.length > 0 && (
        <table className="small">
          <thead><tr><th>Block</th><th>Reward</th><th>When</th></tr></thead>
          <tbody>
            {w.blocks.slice(0, 8).map((x) => (
              <tr key={x.txid}><td><a href={`/block/${x.height}`}>{x.height}</a></td><td>+{coins(x.amount)}</td><td>{new Date(x.timestamp * 1000).toLocaleString()}</td></tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
