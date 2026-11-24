import React, { useEffect, useRef, useState } from 'react';
import RomGate from '../lib/RomGate.jsx';
import { daemon } from '../lib/rpc.js';
import { getCycle } from '../lib/replay/client.js';
import { hexToBytes, seal, withTimestamp, templateTimestamp } from '../lib/proof.js';
import { frames as fmtFrames } from '../lib/format.js';
import { RESET_MODES, initialStart, afterFailure, restartAt } from '../lib/resetPolicy.js';

// The automated miner in the browser: the native miner's RRT campaign (objective-RRT search,
// pow/webminer) compiled to WebAssembly, one campaign per Web Worker. Each worker searches the
// same block template with its own salt (as `automine --salt`); the first run found is sealed,
// stamped when it is valid and submitted. A new tip restarts them on the new template.

const isAddress = (a) => a.startsWith('SM6') && a.length > 90;
const now = () => Date.now() / 1000;
const SEND_AHEAD = 100; // the nodes' future-timestamp window (120 s), with a margin

export default function AutoMine() {
  return (
    <RomGate fallback={<div className="card"><h2>Auto-mine</h2><p>The automated miner plays the game in this browser: it needs your US Super Mario 64 ROM (it stays in this browser).</p></div>}>
      <AutoMiner />
    </RomGate>
  );
}

function AutoMiner() {
  const [address, setAddress] = useState(() => {
    const q = new URLSearchParams(window.location.search).get('address');
    return q && isAddress(q.trim()) ? q.trim() : localStorage.getItem('sm64coin.address') || '';
  });
  useEffect(() => { if (isAddress(address)) localStorage.setItem('sm64coin.address', address); }, [address]);
  const [count, setCount] = useState(() => Math.max(1, Math.min(8, (navigator.hardwareConcurrency || 2) - 1)));
  const [running, setRunning] = useState(false);
  const [log, setLog] = useState([]);
  const [workers, setWorkers] = useState([]); // [{salt, line}]
  const [stats, setStats] = useState({ rounds: 0, found: 0, accepted: 0 });
  const [resetMode, setResetMode] = useState(() => { try { return localStorage.getItem('sm64coin.resetMode') || 'when-stuck'; } catch { return 'when-stuck'; } });
  const mode = RESET_MODES.includes(resetMode) ? resetMode : 'when-stuck';
  const modeRef = useRef(mode);
  modeRef.current = mode;
  useEffect(() => { try { localStorage.setItem('sm64coin.resetMode', mode); } catch { /* private window */ } }, [mode]);
  const ctl = useRef(null);

  const say = (m) => setLog((l) => [`${new Date().toLocaleTimeString()}  ${m}`, ...l].slice(0, 200));

  function stopAll() {
    const c = ctl.current;
    if (!c) return;
    c.stopped = true;
    for (const w of c.pool) if (w) w.terminate();
    c.pool = [];
    clearInterval(c.watch);
    ctl.current = null;
    setRunning(false);
    setWorkers([]);
  }
  useEffect(() => stopAll, []);

  async function round(c) {
    c.pool.forEach((w) => w && w.terminate());
    c.pool = [];
    if (c.stopped) return;
    let t;
    try {
      t = await daemon('get_block_template', { wallet_address: address, reserve_size: 0 });
    } catch (e) {
      say(`no template: ${e}; retrying in 10 s`);
      setTimeout(() => round(c), 10000);
      return;
    }
    c.template = t;
    c.attempt += 1;
    setStats((s) => ({ ...s, rounds: s.rounds + 1 }));
    const first = initialStart(modeRef.current, t.sm64_start, t.sm64_parent_stars);
    say(`block ${t.height}: reset policy ${modeRef.current}, chain start ${t.sm64_start}: starting from ${first.start} (${first.why})`);
    const start = first.start;
    c.restartAt = restartAt(modeRef.current, t.sm64_start, t.sm64_power_on_at || 0);
    let parentPads = '';
    let parentState = null; // the parent's end state from the node (it has it cached)
    // Parent start state (also fetched for "either": a worker may fall back to power-on later,
    // and the parent search needs it first).
    if (start === 'parent') {
      try {
        const s = await daemon('get_sm64_state', { hash: t.prev_hash });
        const gz = new Blob([hexToBytes(s.state_gz)]).stream().pipeThrough(new DecompressionStream('gzip'));
        parentState = await new Response(gz).arrayBuffer();
        say(`block ${t.height}: the parent's end state from the node (${t.sm64_parent_stars} stars, ${t.sm64_parent_keys} keys)`);
      } catch (e) {
        // An older node, or the state isn't cached: rebuild it from the cycle.
        say(`block ${t.height}: rebuilding the parent's end state (${t.sm64_parent_stars} stars, ${t.sm64_parent_keys} keys)`);
        try {
          parentPads = (await getCycle(t.prev_hash)).played_pads;
        } catch (e2) {
          say(`no cycle: ${e2}; retrying in 10 s`);
          setTimeout(() => round(c), 10000);
          return;
        }
      }
    }
    if (c.stopped || c.template !== t) return;
    say(`block ${t.height}: ${count} worker${count === 1 ? '' : 's'} searching (start ${start}, limit ${fmtFrames(t.sm64_max_frames)} now)`);
    const status = Array.from({ length: count }, (_, i) => ({ salt: c.attempt * 1000 + i, line: 'loading the game…', runs: 0, fails: 0, start }));
    const show = () => setWorkers(status.map((s) => ({ ...s })));
    show();
    // Each worker searches on its own: one that ends without a run (or fails) starts again with
    // a new salt, after a pause that grows if it keeps failing; the first run found wins.
    const launch = (i) => {
      if (c.stopped || c.template !== t) return;
      const w = new Worker(new URL('../lib/webminer/worker.js', import.meta.url), { type: 'module' });
      c.pool[i] = w;
      w.onmessage = (e) => {
        const m = e.data;
        if (c.stopped || c.template !== t) return;
        if (m.type === 'log') {
          status[i].line = m.line;
          show();
          return;
        }
        w.terminate();
        const r = m.result;
        if (m.type === 'result' && r.status === 'found') {
          setStats((s) => ({ ...s, found: s.found + 1 }));
          say(`worker ${i + 1}: run found, ${r.frames} frames (${fmtFrames(r.frames)}), ${(r.sim_frames / 1e6).toFixed(0)} M frames searched`);
          c.pool.forEach((x) => x && x.terminate());
          c.pool = [];
          submit(c, t, { ...r, start: status[i].start });
          return;
        }
        const failed = m.type === 'error';
        status[i].fails = failed ? status[i].fails + 1 : 0;
        status[i].runs += 1;
        status[i].line = failed ? `failed: ${m.message}` : `no run (${r.why}); starting again`;
        if (failed) say(`worker ${i + 1} failed: ${m.message.slice(0, 200)}`);
        status[i].salt += count;
        // A parent-start search that ends without a run on an "either" template: reset cycle.
        if (!failed && status[i].start === 'parent' && afterFailure(modeRef.current, t.sm64_start)) {
          status[i].start = 'power-on';
          say(`worker ${i + 1}: no run from the parent (${r.why}); reset policy ${modeRef.current}: next attempt from power-on (reset cycle)`);
        }
        show();
        setTimeout(() => launch(i), Math.min(60, 2 ** status[i].fails) * 1000);
      };
      w.postMessage({
        type: 'mine',
        request: { start: status[i].start, parent_pads: parentPads, seed_hash: t.sm64_seed_hash, retarget_difficulty: t.sm64_retarget_difficulty, salt: status[i].salt, threads: 1 },
        parentState: status[i].start === 'parent' && parentState ? parentState.slice(0) : null,
      });
    };
    for (let i = 0; i < count; i++) launch(i);
  }

  async function submit(c, t, r) {
    try {
      let validAt = 0;
      try {
        validAt = (await daemon('get_block_template', { wallet_address: address, reserve_size: 0, sm64_frames: r.frames })).sm64_valid_at || 0;
      } catch { /* the node decides */ }
      while (validAt - now() > SEND_AHEAD) {
        if (c.stopped || c.template !== t) return;
        say(`the run is over the time limit: valid at ${new Date(validAt * 1000).toLocaleTimeString()}, held until then (a new block first makes it worthless)`);
        await new Promise((res) => setTimeout(res, Math.min(30, validAt - now() - SEND_AHEAD) * 1000));
      }
      const info = await daemon('get_info');
      if (info.top_block_hash !== t.prev_hash) {
        say('a new block landed first; the run was for the old tip');
        return;
      }
      let ts = Math.max(Math.floor(now()), validAt, templateTimestamp(t.blocktemplate_blob));
      // A reset cycle (power-on on an "either" template) is judged against an earlier limit and is valid from sm64_power_on_at.
      if (r.start === 'power-on' && t.sm64_start === 'either' && t.sm64_power_on_at) ts = Math.max(ts, t.sm64_power_on_at);
      await daemon('submit_block', [seal(withTimestamp(t.blocktemplate_blob, ts), hexToBytes(r.proof))]);
      setStats((s) => ({ ...s, accepted: s.accepted + 1 }));
      say(`★ BLOCK ${t.height} SUBMITTED: ${r.frames} frames, to ${address.slice(0, 12)}…`);
    } catch (e) {
      say(`block rejected: ${e}`);
    } finally {
      if (!c.stopped) round(c);
    }
  }

  function startAll() {
    if (!isAddress(address)) { say('enter your SM64… address first'); return; }
    stopAll();
    const c = { stopped: false, pool: [], attempt: 0, template: null };
    ctl.current = c;
    setRunning(true);
    // A new tip: the template is stale, start over on the new one.
    c.watch = setInterval(async () => {
      try {
        // A parent-only template turns "either" at sm64_power_on_at: refresh it to learn that.
        if (c.template && c.restartAt && now() >= c.restartAt && c.pool.some(Boolean)) {
          c.restartAt = 0;
          say('the stall time passed: refreshing the template (power-on start now allowed)');
          round(c);
          return;
        }
        const i = await daemon('get_info');
        if (c.template && i.top_block_hash !== c.template.prev_hash && c.pool.some(Boolean)) {
          say(`new block ${i.height - 1} on the chain: restarting on the new template`);
          round(c);
        }
      } catch { /* node hiccup */ }
    }, 5000);
    round(c);
  }

  return (
    <div className="card">
      <h2>Auto-mine</h2>
      <p className="muted">
        The automated miner in your browser: the same campaign search as the native <code>sm64coin-automine</code>
        (objective-RRT, compiled to WebAssembly), one per worker, each with its own seed. It uses a lot of CPU while it runs.
        Rewards go to your address; nothing leaves this browser but the block.
      </p>
      <p>
        <input className="mono" style={{ width: '100%' }} placeholder="your SM64… address" value={address} onChange={(e) => setAddress(e.target.value.trim())} disabled={running} />
      </p>
      <p>
        Workers <input type="number" min="1" max="32" value={count} onChange={(e) => setCount(Math.max(1, Math.min(32, Number(e.target.value) || 1)))} disabled={running} style={{ width: '4em' }} />{' '}
        <select value={mode} onChange={(e) => setResetMode(e.target.value)} title="Reset cycle (power-on start) policy when the chain allows it">
          <option value="never">reset: never</option>
          <option value="when-stuck">reset: when stuck</option>
          <option value="always-when-allowed">reset: whenever allowed</option>
        </select>{' '}
        {running ? <button onClick={stopAll}>Stop</button> : <button onClick={startAll} disabled={!isAddress(address)}>Start mining</button>}{' '}
        <span className="muted">rounds {stats.rounds} · runs found {stats.found} · blocks submitted {stats.accepted}</span>
      </p>
      {workers.length > 0 && (
        <table className="small">
          <tbody>
            {workers.map((w, i) => <tr key={i}><td>worker {i + 1}</td><td className="muted">salt {w.salt}{w.runs ? ` · ${w.runs} search${w.runs === 1 ? '' : 'es'} done` : ''}</td><td className="mono">{w.line}</td></tr>)}
          </tbody>
        </table>
      )}
      <pre className="log small">{log.join('\n')}</pre>
    </div>
  );
}
