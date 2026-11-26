import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { Recorder } from '../lib/recorder.js';
import { SAVE_KEYS, LOAD_KEYS, courseLabel } from '../lib/saveslots.js';
import CycleTimeline from '../lib/CycleTimeline.jsx';
import LimitChart from '../lib/LimitChart.jsx';
import { daemon } from '../lib/rpc.js';
import { newMinerKey, templateParams, signRun } from '../lib/minerkey.js';
import { rawProof, proofFromRun, seal, toM64, download, bytesToHex, withTimestamp, templateTimestamp } from '../lib/proof.js';
import { prepareStart } from '../lib/cycle.js';
import { withConfig } from '../lib/task.js';
import { successFor } from '../lib/rules.js';
import { coins, frames } from '../lib/format.js';
import PlayView from '../components/PlayView.jsx';
import EngineSlot, { acquireEngine } from '../components/EngineSlot.jsx';
import { Toolbar, ToolBtn, Cols, Row, SideCard, Panel, InfoPop, useStored } from '../components/ui.jsx';
import RomGate from '../lib/RomGate.jsx';
import { miner, hybrid, isAddress } from '../lib/webminer/controller.js';
import { useMiner } from '../lib/webminer/useMiner.js';
import { encPads } from '../lib/webminer/hybrid.js';
import { DEFAULT_LEAD } from '../lib/webminer/bestRun.js';
import { setPlayFromStart } from '../lib/hybridSetting.js';
import { useVisible } from './automine/useHub.js';
import ControlCard from './automine/ControlCard.jsx';
import CampaignStrips from './automine/CampaignStrips.jsx';
import { levelName } from './automine/levels.js';
import { localInputs } from './automine/localNode.js';
import { announceMined } from '../lib/payout.js';
import { signedSeeds } from '../lib/webminer/bestRun.js';
import { decideAfterWin, effectiveValidAt, launchFor, refusedBeforeClaim, refusalReason, confirmBlock, REWIND_DEFAULT, REWIND_CHOICES } from '../lib/mineflow.js';
import { ALL_STARS, secondsUntilAllowed } from '../lib/resetPolicy.js';
import { e2e, e2eHook } from '../lib/e2e.js';

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

// The auto-miner's views, loaded only when they are needed (three.js, the visual game's reel, the charts).
const CloudView = React.lazy(() => import('./automine/CloudView.jsx'));
const ReelView = React.lazy(() => import('./automine/GameView.jsx'));
const ReelSettings = React.lazy(() => import('./automine/GameView.jsx').then((m) => ({ default: m.ReelSettings })));
const ReelChip = React.lazy(() => import('./automine/GameView.jsx').then((m) => ({ default: m.ReelChip })));
const HybridDriver = React.lazy(() => import('./automine/HybridPlay.jsx'));
const VizTop = React.lazy(() => import('./automine/VizPanel.jsx').then((m) => ({ default: m.VizTop })));
const VizBottom = React.lazy(() => import('./automine/VizPanel.jsx').then((m) => ({ default: m.VizBottom })));
const VizToasts = React.lazy(() => import('./automine/VizPanel.jsx').then((m) => ({ default: m.VizToasts })));
const loadingCard = <div className="viz-card"><p className="muted">Loading…</p></div>;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mmss = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;

function Miner() {
  // The auto-miner (lib/webminer/controller.js) lives as long as the site's page: this page is its view and the manual miner's.
  // The reward address is the controller's (?address= from the wallet's links beats the stored one there).
  const s = useMiner();
  const { address, running, vizOn, playFromStart, hybridOK } = s;
  const hub = miner.hub;
  const addr = useRef(address);
  addr.current = address;
  // The game: one engine for everything on this page (EngineSlot), driven by one of: the manual miner (the classic
  // page: the block's start), the highlight reel (the auto-miner runs and nobody plays), or a segment played by hand from
  // a node of the search tree (hybrid mining).
  const [engine, setEngine] = useState(null);
  const [engineErr, setEngineErr] = useState('');
  // The search views (the reel, the cloud) show while the auto-miner runs with its visualisations and "Play from the start"
  // is off; otherwise this is the classic manual miner.
  const showAuto = running && vizOn && !playFromStart;
  const manualActive = !showAuto;
  const [task, setTask] = useState(null);
  const [log, setLog] = useState([]);
  const [clock, setClock] = useState(null);
  // What the miner is doing: {kind: 'loading'|'ff'|'play'|'waiting'|'submitting', ...}
  const [phase, setPhase] = useState({ kind: 'loading' });
  // The start of the current attempt: {kind, progress, cycle}, and the chain's start mode.
  const [start, setStart] = useState(null);
  const [startMode, setStartMode] = useState('parent');
  const [powerOnAt, setPowerOnAt] = useState(0); // when a parent-only template turns "either" (unix s)
  const [limit, setLimit] = useState(null);
  // Runs finished on this tip (the limit chart's dots): {t: seconds after the parent, frames}.
  const [runs, setRuns] = useState([]);
  // The run over the limit waiting for it (the auction): {validAt, frames}.
  const [pendingRun, setPending] = useState(null);
  // Where the last run stands: {kind: 'sent' | 'accepted' | 'refused' | 'notaccepted', height, frames, reason?, at?} (shown as a notice;
  // only 'accepted' means the block is in the chain). `practice`: this template's one run is sent, so more runs on it only practise.
  const [outcome, setOutcome] = useState(null);
  const [practice, setPractice] = useState(false);
  const [launchAt, setLaunchAt] = useState(0); // the chain's launch (unix s) while it lies ahead, else 0
  const [rewindSec, setRewindSec] = useStored('sm64coin.rewindSec', REWIND_DEFAULT);
  const rewindRef = useRef(rewindSec);
  rewindRef.current = rewindSec;
  const [tick, setTick] = useState(0);
  const rec = useRef(null);
  useEffect(() => { e2eHook('__miner', { rec }); }, []); // test hook, ?e2e only (read-only use: the touch-controls test)
  const ctl = useRef({ restart: false, forcePower: false, discard: false, pending: null, discardPending: false, used: new Set(), cur: null, launch: 0, signed: null, tipNoted: false });
  useEffect(() => { e2eHook('__mineflow', { ctl, get outcome() { return outcomeRef.current; } }); }, []);
  const outcomeRef = useRef(null);
  outcomeRef.current = outcome;
  // The last attempt, for debugging: its pads and the template it was played for.
  const [last, setLast] = useState(null);
  const say = (m) => setLog((l) => [`${new Date().toLocaleTimeString()}  ${m}`, ...l].slice(0, 50));

  useEffect(() => {
    const id = setInterval(() => setTick((x) => x + 1), 1000); // countdowns
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    if (!engine || !manualActive) return undefined;
    let stopped = false;
    let head = null;
    const c = ctl.current;
    const setLim = (n) => { setLimit(n); if (rec.current) rec.current.setLimit(n); };
    // Cheap per-frame kill check; timers watch the chain.
    const timer = setInterval(async () => {
      try {
        const i = await daemon('get_info');
        const launch = launchFor(i, e2e() ? Number(window.__launchAt || 0) : 0);
        c.launch = launch;
        setLaunchAt(launch > Date.now() / 1000 ? launch : 0);
        if (head && i.top_block_hash !== head) {
          if (c.cur && c.used.has(c.cur.sm64_seed_hash)) {
            // This template's run is sent: the block (yours, usually) lands on top of it. Keep the practice state; the next attempt waits for the player.
            if (!c.tipNoted) { c.tipNoted = true; say(`new block ${i.height - 1} on the chain: you can keep practising here, but only a new template (▶| new) can be submitted`); }
          } else { c.restart = true; say(`new block ${i.height - 1} on the chain: new template; your cycle continues from its end`); }
        }
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
        setPowerOnAt(t.sm64_power_on_at || 0);
      } catch { /* daemon hiccup */ } finally { refreshing = false; }
    }, 5000);
    const kill = async () => stopped || c.restart;
    // Seals and submits a finished run on its own template (a newer one has other mempool transactions, which would
    // change the seed), stamped when it is valid. Returns {sent}: whether the node took the block (it may still hold it
    // until it is due). The key signs ONE run: the template is marked used at once, and only a refusal by the node (it
    // stored nothing) frees it again. "Block mined" is announced only when the block is seen in the chain (confirm below).
    // The debug panel's "last attempt" while the attempt is still running (the run being submitted, or confirmed).
    const markLast = (r, result) => setLast((l) => (l && l.template === r.t ? { ...l, result } : { template: r.t, pads: r.done.pads, won: true, result }));
    const submitRun = async (r) => {
      const { t, done, st } = r;
      const seed = t.sm64_seed_hash;
      const refused = (reason, kind = 'refused') => {
        markLast(r, `rejected: ${reason}`);
        setOutcome({ kind, height: t.height, frames: r.frames, reason });
        say(`block not accepted: ${reason}`);
        return { sent: false };
      };
      const proof = proofFromRun(done.pads, done.poked, st.kind === 'power-on');
      if (!c.signed) c.signed = signedSeeds((() => { try { return localStorage; } catch { return null; } })());
      const other = c.signed.signedOther(seed, bytesToHex(proof));
      if (other !== null) return refused(`this template's key already signed a different run (${other} frames); only one run per template: start a new template (▶| new)`, 'notaccepted');
      c.signed.record(seed, bytesToHex(proof), r.frames);
      c.used.add(seed);
      const now = Date.now() / 1000;
      let ts = Math.max(Math.floor(now), r.validAt, templateTimestamp(t.blocktemplate_blob));
      if (st.kind === 'power-on' && t.sm64_start === 'either' && t.sm64_power_on_at) ts = Math.max(ts, t.sm64_power_on_at);
      try {
        const sig = await signRun(t.minerKey, seed, proof);
        await daemon('submit_block', [seal(withTimestamp(t.blocktemplate_blob, ts), proof, sig)]);
      } catch (e) {
        const reason = refusalReason(e);
        if (refusedBeforeClaim(e)) {
          // Refused before the node recorded a claim for this template: a shorter run may still be tried here.
          c.signed.forget(seed); c.used.delete(seed);
          return refused(reason);
        }
        // The node may already hold a signed claim for this template (a second, different run would be equivocation), or the
        // submission may have gone through: the key stays spent. Next attempt: a new template; save slots belong to this one.
        setLast((l) => (l && l.template === t ? { ...l, result: `rejected: ${reason}` } : l));
        setOutcome({ kind: 'notaccepted', fresh: true, height: t.height, frames: r.frames, reason: `${reason}. The node may already hold this run for the template, so it can't sign another: moving to a new template (your save slots belong to the old one and are dropped)` });
        say(`block not accepted: ${reason}; new template (one run per template, the node may hold this one)`);
        if (c.cur === t) c.restart = true;
        return { sent: false };
      }
      if (c.cur && c.cur.sm64_seed_hash === seed) setPractice(true);
      markLast(r, 'sent');
      setOutcome({ kind: 'sent', height: t.height, frames: r.frames, at: ts });
      say(`block ${t.height} SENT: ${r.frames} frames (${frames(r.frames)}), stamped ${new Date(ts * 1000).toLocaleTimeString()}; waiting for the node to put it in the chain`);
      // Not announced yet: look for it in the chain until a few seconds past its due time.
      confirmBlock({
        blockAt: async (h) => { try { const b = await daemon('get_block', { height: h }); return { prev_hash: b.block_header.prev_hash, blob: b.blob }; } catch { return null; } },
        now: () => (stopped ? Infinity : Date.now() / 1000),
        sleep,
      }, t, t.minerKey.public_hex, ts).then((res) => {
        if (stopped) return;
        if (res.status === 'accepted') {
          markLast(r, 'accepted');
          setOutcome({ kind: 'accepted', height: t.height, frames: r.frames, reward: t.expected_reward });
          announceMined(t.height, t.expected_reward);
          say(`★ BLOCK ${t.height} ACCEPTED: ${r.frames} frames (${frames(r.frames)}), ${coins(t.expected_reward)} SM64 to you`);
        } else {
          markLast(r, `not accepted: ${res.why}`);
          setOutcome({ kind: 'notaccepted', fresh: true, height: t.height, frames: r.frames, reason: res.why });
          say(`block ${t.height} NOT accepted: ${res.why}; new template`);
          if (c.cur === t) c.restart = true; // this template's one run is spent
        }
      });
      return { sent: true };
    };
    // The pending run (over the limit when finished): sent DEFAULT_LEAD (5 s) before it is valid,
    // like the auto-miner (v0.3 nodes refuse an RPC submission due more than 30 s ahead:
    // rules.rs RPC_SUBMIT_AHEAD_SECONDS); dropped if a new block lands first.
    const SEND_AHEAD = DEFAULT_LEAD;
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
      const release = await acquireEngine();
      if (stopped) { release(); return; }
      try {
        while (!stopped) {
          c.restart = false; c.discard = false;
          if (!isAddress(addr.current)) { setPhase({ kind: 'loading' }); await sleep(1000); continue; }
          let t;
          try {
            const paysTo = addr.current;
            const key = await newMinerKey(); // v0.3: this template's miner key, chosen before the run is played
            t = await daemon('get_block_template', templateParams(paysTo, key));
            t.minerKey = key;
            t.paysTo = paysTo; // the template's coinbase pays this address
          } catch (e) { say(String(e)); await sleep(3000); continue; }
          const won = successFor(t.sm64_rules);
          if (!won) { say(`unknown rules "${t.sm64_rules}": update web/src/lib/rules.js`); return; }
          if (head !== t.prev_hash) { c.forcePower = false; setRuns([]); }
          c.cur = t; c.tipNoted = false; setPractice(false); setOutcome((o) => (o && (o.kind === 'sent' || o.fresh) ? { ...o, fresh: false } : null));
          head = t.prev_hash;
          c.tipTemplate = t;
          setTask(t); setLim(t.sm64_max_frames); setStartMode(t.sm64_start); setPowerOnAt(t.sm64_power_on_at || 0);
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
          let done = await r.run(kill);
          // A finished run that is not final does not end the attempt: the game goes back a few seconds on the SAME template
          // (slots kept) for a shorter run, until the run is sent, the tip changes, or the player asks for a new template.
          while (done && !stopped && !c.restart) {
            const nFrames = done.pads.length;
            say(`goal reached after ${nFrames} frames (${frames(nFrames)})`);
            const used = c.used.has(t.sm64_seed_hash);
            if (used) {
              say(`practice run: ${nFrames} frames; not submitted (this template's run is already sent). Start a new template (▶| new) for the next block.`);
            } else {
              setRuns((rs) => [...rs, { t: Date.now() / 1000 - t.sm64_parent_timestamp, frames: nFrames }]);
              // When does a run this long become valid? (The limit grows with time; never before the chain's launch.)
              let validAt = 0;
              try {
                const q = await daemon('get_block_template', templateParams(t.paysTo, t.minerKey, { sm64_frames: nFrames }));
                validAt = q.sm64_valid_at || 0;
              } catch (e) { say(`could not ask when the run is valid: ${e}`); }
              if (stopped) break;
              validAt = effectiveValidAt(validAt, c.launch);
              const p = c.pending && c.pending.t.prev_hash === t.prev_hash ? c.pending : null;
              const d = decideAfterWin({ now: Date.now() / 1000, validAt, frames: nFrames, used: c.used.has(t.sm64_seed_hash), pending: p });
              const run = { t, done: { pads: [...done.pads], poked: [...done.poked] }, st, validAt, frames: nFrames }; // copies: the recorder rewinds its own arrays
              if (d.action === 'keep-pending') {
                say(`your pending run (${frames(p.frames)}) becomes valid sooner or as soon: this one (${frames(nFrames)}) is dropped. Play on for a shorter run.`);
              } else if (d.action === 'hold') {
                // Over the limit (or the chain is not open yet): keep it as the pending run, replacing a longer one (one run per
                // template is signed, and signing happens when it is sent). Sent DEFAULT_LEAD s before it is valid, stamped with that time.
                c.pending = run;
                setPending({ validAt, frames: nFrames });
                const early = c.launch && validAt <= c.launch;
                say(early
                  ? `found before the chain opens: kept (${nFrames} frames), sent automatically at ${new Date(validAt * 1000).toLocaleString()}. Play on for a shorter one.`
                  : `over the time limit by ${nFrames - (c.tipTemplate ? c.tipTemplate.sm64_max_frames : 0)} frames: kept as your pending run${d.replaces ? ' (replacing the longer one)' : ''}, valid at ${new Date(validAt * 1000).toLocaleTimeString()} and sent automatically then. Play on: a shorter run becomes valid sooner.`);
              } else if (d.action === 'send') {
                if (c.pending) { c.pending = null; setPending(null); }
                setPhase({ kind: 'submitting' });
                await submitRun(run);
              }
            }
            if (stopped || c.restart) break;
            setPhase({ kind: 'play' });
            r.rewindFromEnd(rewindRef.current);
            done = await r.run(kill);
          }
          rec.current = null;
          const result = done ? 'won' : r.skipped ? 'skipped' : c.restart ? 'interrupted' : 'out of frames';
          setLast((l) => (c.used.has(t.sm64_seed_hash) && l && l.template === t ? { ...l, pads: [...r.pads] } : { template: t, pads: [...r.pads], won: !!done, result }));
          if (stopped) break;
          if (!done && !c.restart && !r.skipped) say('ran past the longest possible time limit: new attempt');
        }
      } finally { engine.set_audio_enabled(0); release(); }
    })();
    return () => { stopped = true; clearInterval(timer); clearInterval(refresh); clearInterval(pendingTimer); setClock(null); setPhase({ kind: 'loading' }); };
  }, [engine, manualActive]); // eslint-disable-line react-hooks/exhaustive-deps

  const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;
  const paying = task ? task.paysTo : address;
  const pending = task && isAddress(address) && address !== task.paysTo;

  // The auto-miner's views: the page-side tree and the reel need the miner's hub to be collecting.
  useEffect(() => {
    const q = new URLSearchParams(window.location.search);
    if (q.get('vizmock') === '1' && !miner.running) miner.startMock({ count: 3, runAfter: Number(q.get('run') || 0) * 1000 });
    miner.setViewing(true);
    return () => miner.setViewing(false);
  }, []);

  // ----- hybrid mining: play by hand from a node of the search tree -----
  // {phase: 'asking' | 'playing' | 'error', sel, msg?, state?, text?}
  const [play, setPlay] = useState(null);
  const stopRef = useRef(null); // 'finish' | 'discard' once the player ended the segment
  const [playText, setPlayText] = useState('');
  const gameBox = useRef(null);
  const gameVisible = useVisible(gameBox);
  const mode = play ? 'hybrid' : showAuto ? 'reel' : 'manual';
  const groups = useControls(rec, mode === 'hybrid');
  useEffect(() => { if (!showAuto && play) { stopRef.current = 'discard'; setPlay(null); } }, [showAuto]); // eslint-disable-line react-hooks/exhaustive-deps
  const reelPlay = (w, node) => { hub.game = { id: hub.game.id + 1, kind: node ? 'node' : 'best', w, step: node?.step, nid: node?.id }; hub.found = null; hub.notify(); gameBox.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' }); };
  const playFrom = async (sel) => {
    const w = hub.workers[sel.w];
    if (!w?.cur) return;
    stopRef.current = null;
    setClock(null);
    // Fast path: the page has the step's start state, its root perturbation and the node's path (its tree mirror), so it
    // rebuilds the node's state itself. Otherwise (or when that fails) the search is asked, which answers between rounds.
    const inp = (e2e() && window.__forceMailbox) ? null : localInputs(hub, sel);
    if (inp) {
      setPlayText('rebuilding the node\'s state…');
      setPlay({ phase: 'playing', sel, local: inp, msg: { level: sel.level, area: sel.area, stars: sel.stars, pos: sel.pos } });
      gameBox.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
      return;
    }
    playFromSearch(sel);
  };
  const playFromSearch = async (sel) => {
    const w = hub.workers[sel.w];
    if (!w?.cur) return;
    setPlayText('asking the search for the node\'s state…');
    setPlay({ phase: 'asking', sel });
    gameBox.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    try {
      const r = await hybrid.requestState(sel.w, sel.id, w.cur.key, 180000);
      setPlay((p) => (p && p.sel === sel && p.phase === 'asking' ? { phase: 'playing', sel, msg: r.msg, state: r.state } : p));
    } catch (e) {
      setPlay((p) => (p && p.sel === sel ? { phase: 'error', sel, text: String(e.message || e) } : p));
    }
  };
  // The segment ended (hotkey): back to the visualiser at once, the segment goes into the search trees meanwhile.
  const played = async (res) => {
    const { sel, msg } = play;
    setPlay(null);
    document.querySelector('.viz-cloud')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    if (res.discard || res.frames < 1) { hub.toast('segment discarded', 'info'); hub.notify(); return; }
    const info = { w: sel.w, step: sel.step, nid: null, parentSlot: sel.slot, pos: res.pos, level: msg.level, area: msg.area, stars: msg.stars, frame: sel.frame + res.frames, trail: res.trail, frames: res.frames, note: 'adding it to the search tree…' };
    hub.playedSegment(info);
    hub.toast(`played ${res.frames} frames (${(res.frames / 30).toFixed(1)} s) from node ${sel.id}: adding it to the search trees`, 'info');
    let first = true, added = 0;
    const onReply = (slot, m) => {
      if (!m.ok) return;
      added++;
      const own = slot === sel.w;
      if (first || own) {
        first = false;
        const wk = hub.workers[slot];
        const off = Math.hypot(m.pos[0] - res.pos[0], m.pos[1] - res.pos[1], m.pos[2] - res.pos[2]);
        hub.playedSegment({ ...info, w: slot, step: own ? sel.step : wk?.cur?.n ?? sel.step, nid: m.nid, parentSlot: own ? sel.slot : -1, pos: m.pos, level: m.level, area: m.area, stars: m.stars, frame: (wk?.played || 0) + m.frame, trail: res.trail,
          note: `in worker ${slot + 1}'s tree as node ${m.nid} with a high weight${off < 1 ? '; the replay in the search ends exactly where you finished' : `; WARNING: the replay in the search ends ${off.toFixed(1)} units from where you finished`}` });
      }
    };
    try {
      await hybrid.insert({ tag: msg.tag, path: msg.path, seg: encPads(res.pads), weight: 8 }, onReply, 180000);
      hub.toast(`your segment is a node of ${added} search tree${added === 1 ? '' : 's'}: the searches start from it first`, 'found');
    } catch (e) {
      hub.playedSegment({ ...info, note: `not added: ${e.message || e}` });
      hub.toast(`the segment was not added: ${e.message || e}`, 'info');
    }
    hub.notify();
  };
  const finish = () => { stopRef.current ??= 'finish'; };
  const discard = () => { stopRef.current ??= 'discard'; };
  const cancelPlay = () => { stopRef.current = 'discard'; setPlay(null); };

  // ----- the game panel -----
  const [gameOpen, setGameOpen] = useStored('sm64coin.gameOpen', false);
  const used = mode === 'manual' && clock ? clock.frame : 0;
  const lim = mode === 'manual' ? (clock ? clock.limit : limit) : null;
  const over = lim && used > lim;
  const fromStartBtn = running ? (
    <ToolBtn key="fs" on={playFromStart} onClick={() => setPlayFromStart(!playFromStart)}
      title="On: classic manual mining from the block's beginning (power-on / the parent's end): no search tree, nothing to click into. Off: this panel follows the auto-miner and you can play on from any node of its search.">⏮ Play from the start</ToolBtn>
  ) : null;
  const hybridWhy = !hybridOK ? 'Playing from a node needs the running auto-miner with worker sync (the Auto-mine card: Enable worker sync, then Start).' : '';
  const gameSettings = (
    <form className="viz-settings" onSubmit={(e) => e.preventDefault()}>
      <label htmlFor="game-pfs">Play from the start</label>
      <div className="ctl">
        <input id="game-pfs" type="checkbox" checked={playFromStart} title="On: manual mining ignores hybrid mining entirely: always from the block's beginning, no search tree to play from, no clicking into nodes. Off: while the auto-miner runs, click a node of the search cloud and press Play from here." onChange={(e) => setPlayFromStart(e.target.checked)} />
        <span className="muted">{playFromStart ? 'classic manual miner, hybrid mining is off' : running ? 'hybrid: the game follows the auto-miner, click a node to play on from it' : 'applies once you start auto-mining'}</span>
      </div>
      {mode === 'reel' && <React.Suspense fallback={null}><ReelSettings hub={hub} /></React.Suspense>}
    </form>
  );
  const chip = mode === 'reel' ? <React.Suspense fallback={null}><ReelChip hub={hub} /></React.Suspense>
    : mode === 'hybrid' ? <span className="chip">from node {play.sel.id} of worker {play.sel.w + 1}</span> : null;
  const hybridNormal = play && (
    <>
      {play.phase === 'asking' && <div className="viz-status"><span>Loading node {play.sel.id} of worker {play.sel.w + 1}: {playText}</span><span className="muted small">The search answers between two rounds (a few seconds, longer on a loaded machine).</span></div>}
      {play.phase === 'error' && <div className="viz-status"><span className="error">Could not play from node {play.sel.id}: {play.text}</span></div>}
      {play.phase === 'playing' && (
        <>
          <div className="statusbar">
            <span className="stat" id="hy-status">{clock ? `playing: ${clock.frame} frames played (${frames(clock.frame)})` : playText || 'loading…'}</span>
            {clock && clock.paused && <span className="stat warn">PAUSED</span>}
            {clock && clock.mode === 'catchup' && <span className="stat warn">replaying to frame {clock.target} (T to play from here)</span>}
            <span className="stat">{clock ? `${clock.speed}×` : ''}</span>
            <span className="stat">{levelName(play.msg.level)} · area {play.msg.area} · node frame {play.sel.frame}</span>
          </div>
          <Controls groups={groups} />
          <p className="muted small" style={{ margin: '8px 0 0' }}>
            <b>Enter</b> (or Esc) ends the segment: it becomes a node of the search tree with a high weight and you return to the cloud. G goes back to the node's state.
          </p>
        </>
      )}
    </>
  );
  const hybridActions = play && (
    <>
      {play.phase !== 'playing' && <ToolBtn onClick={cancelPlay} title="Throw the segment away and go back to the cloud">Cancel</ToolBtn>}
    </>
  );
  const nowS = Date.now() / 1000 + tick * 0;
  const outcomeText = outcome && (outcome.kind === 'sent'
    ? `Block ${outcome.height} sent (${frames(outcome.frames)}); waiting for the node to put it in the chain${outcome.at > nowS + 1 ? `, due ${new Date(outcome.at * 1000).toLocaleTimeString()}` : ''}…`
    : outcome.kind === 'accepted' ? `Block ${outcome.height} accepted: it is in the chain (${frames(outcome.frames)}, ${coins(outcome.reward)} SM64).`
    : `Block ${outcome.height} not accepted: ${outcome.reason}.${outcome.kind === 'refused' ? ' Still on this template: play on for a shorter run.' : ''}`);
  const manualNormal = (
    <>
      {launchAt > nowS && (
        <div className="notice" data-testid="prelaunch" role="status">
          The chain opens at <b>{new Date(launchAt * 1000).toLocaleString()}</b> (in {mmss(launchAt - nowS)}). Runs found earlier are kept and sent then: play on for a shorter one.
        </div>
      )}
      {outcome && <div className={`notice${outcome.kind === 'accepted' || outcome.kind === 'sent' ? '' : ' warn'}`} data-testid="outcome" role="status">{outcomeText}</div>}
      {practice && <div className="notice warn" data-testid="practice" role="status">Practice only: this template's run is already sent, so nothing played here can be submitted. The next block is a new attempt on a new template: press ▶| new (N).</div>}
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
        {pendingRun && <span className="stat warn">pending run {frames(pendingRun.frames)}: valid in {mmss(Math.max(0, pendingRun.validAt - Date.now() / 1000))} (sent automatically; play on for a shorter one)</span>}
        {phase.kind === 'submitting' && <span className="stat warn">submitting…</span>}
      </div>
      <Controls groups={groups} />
    </>
  );
  const manualActions = (
    <>
      <NewTemplateBtn rec={rec} />
      <ToolBtn title={`After a finished run that is not final (over the limit, or the chain not open yet) the game goes back ${rewindSec} s to try for a shorter one on the same template. Click to change.`}
        onClick={() => setRewindSec(REWIND_CHOICES[(REWIND_CHOICES.indexOf(rewindSec) + 1) % REWIND_CHOICES.length])}>↺ {rewindSec}s</ToolBtn>
      {pendingRun && <ToolBtn onClick={() => { ctl.current.discardPending = true; }} title="Throw away the pending run (over the limit when it was finished)">Discard pending run</ToolBtn>}
      {fromStartBtn}
    </>
  );
  const reelNormal = (
    <React.Suspense fallback={<p className="muted small">Loading…</p>}>
      <ReelView hub={hub} engine={engine} visible={gameVisible} toolbar={fromStartBtn} />
    </React.Suspense>
  );
  const canPlayFrom = showAuto && hybridOK && !play;
  const autoTemplate = hub.template;

  return (
    <>
      <CycleTimeline attempt={mode === 'manual' && clock ? clock.frame : 0} limit={(mode === 'manual' ? lim : autoTemplate ? autoTemplate.sm64_max_frames : 0) || 0} />
      <p className="ui-oneline">
        Play Super Mario 64 yourself to mine blocks, or let your browser play for you (Start in the Auto-mine card).
        <InfoPop>
          Manual mining: you play the block's next star or key, from where the previous block ended; every attempt is sealed to a block template.
          Auto-mining: the same campaign search as the native <code>sm64coin-automine</code> (objective-RRT, compiled to WebAssembly), one per worker; it uses a lot of CPU and keeps running while you visit other pages.
          While it runs, this page shows its search: click a node of the cloud and <b>Play from here</b> to play on by hand; the segment you play becomes a high-weight node the search builds on. <b>Play from the start</b> turns hybrid mining off.
        </InfoPop>
      </p>
      <Cols
        main={(
          <>
            {showAuto && <React.Suspense fallback={null}><VizToasts hub={hub} /></React.Suspense>}
            <Panel className="viz-game" innerRef={gameBox} title="Game" chip={chip} open={gameOpen} onToggle={() => setGameOpen(!gameOpen)} settings={gameSettings}>
              <PlayView rec={rec} clock={clock} groups={mode === 'reel' ? null : groups} limit={mode === 'manual' ? lim : 0} interactive={mode !== 'reel'} onFinish={mode === 'hybrid' && play.phase === 'playing' ? finish : null} onDiscard={mode === 'hybrid' && play.phase === 'playing' ? discard : null}
                normal={mode === 'hybrid' ? hybridNormal : mode === 'reel' ? reelNormal : manualNormal}
                actions={mode === 'hybrid' ? hybridActions : manualActions}>
                <EngineSlot onEngine={setEngine} onError={setEngineErr} />
              </PlayView>
              {engineErr && <p className="error">The game could not start: {engineErr}{/webgl|context/i.test(engineErr) ? ' (this browser may not allow WebGL here; try another browser or switch off battery saver)' : ''}</p>}
              {!engine && !engineErr && <p className="muted small">Loading the game engine…</p>}
              {mode === 'hybrid' && play.phase === 'playing' && engine && (
                <React.Suspense fallback={null}>
                  <HybridDriver hub={hub} engine={engine} req={play} rec={rec} stop={stopRef} setClock={(c) => setClock(c && { ...c, limit: 0 })} setStatus={setPlayText}
                    onDone={played} onReady={(m) => setPlay((p) => (p && p.local ? { ...p, msg: m } : p))}
                    onError={(text) => { if (play.local) { hub.toast('rebuilding the node here failed (' + text + '): asking the search instead', 'info'); playFromSearch(play.sel); } else setPlay((p) => (p ? { phase: 'error', sel: p.sel, text } : p)); }} />
                </React.Suspense>
              )}
            </Panel>
            {showAuto && (
              <React.Suspense fallback={loadingCard}>
                <CloudView hub={hub} onPlay={reelPlay} onPlayFrom={playFrom} onGoal={(g) => miner.setGoal(g)} canPlayFrom={canPlayFrom} whyNot={hybridWhy} />
              </React.Suspense>
            )}
          </>
        )}
        side={(
          <>
            <ControlCard s={s} />
            {showAuto ? (
              <React.Suspense fallback={null}><VizTop hub={hub} mode={s.mode} /></React.Suspense>
            ) : (
              <>
                {task && <SideCard><LimitChart difficulty={task.sm64_retarget_difficulty} parentTs={task.sm64_parent_timestamp} now={Date.now() / 1000 + tick * 0} used={clock ? clock.frame : 0} runs={runs} /></SideCard>}
                {pending && (
                  <div className="notice">
                    The game in progress pays {short(task.paysTo)}; the new address applies from the next attempt
                    (the reward address is part of the block template the run is sealed to).{' '}
                    <button onClick={() => rec.current && rec.current.skip()}>Switch now (restarts the attempt)</button>
                  </div>
                )}
                {task && start ? <SideCard title="Cycle"><CyclePanel task={task} start={start} /></SideCard> : null}
                {start && startMode !== 'power-on' && <ResetCycle start={start} startMode={startMode} powerOnAt={powerOnAt} task={task} tick={tick}
                  onReset={() => { ctl.current.forcePower = true; ctl.current.restart = true; say('reset cycle: starting over from power-on'); }}
                  onParent={() => { ctl.current.forcePower = false; ctl.current.restart = true; say('back to continuing from the parent\'s end state'); }} />}
                <SideCard title="Block">
                  {task ? (
                    <dl className="kv">
                      <dt>Block</dt><dd>{task.height}</dd>
                      <dt>Difficulty</dt><dd>{task.difficulty}</dd>
                      <dt>Time limit now</dt><dd>{frames(limit ?? task.sm64_max_frames)} ({limit ?? task.sm64_max_frames} frames)</dd>
                      <dt>Pokes</dt><dd>{task.sm64_random_amount} × {task.sm64_burst_length} frames per {task.sm64_window_length}</dd>
                      <dt>Reward</dt><dd>{coins(task.expected_reward)} SM64</dd>
                      <dt>Paying</dt><dd>{isAddress(paying || '') ? <span className="mono" title={paying}>{short(paying)}</span> : <em>no address</em>}</dd>
                    </dl>
                  ) : <p className="muted">Getting a block template…</p>}
                </SideCard>
              </>
            )}
            {running && vizOn && <React.Suspense fallback={null}><VizBottom hub={hub} /></React.Suspense>}
          </>
        )}
      />
      <Row title="Workers" count={s.workers.length || undefined} storeKey="workers">
        {s.workers.length > 0 && (
          <table className="small">
            <tbody>
              {s.workers.map((w, i) => <tr key={i}><td>worker {i + 1}</td><td className="muted">salt {w.salt}{w.runs ? ` · ${w.runs} search${w.runs === 1 ? '' : 'es'} done` : ''}</td><td className="mono">{w.line}</td></tr>)}
            </tbody>
          </table>
        )}
        {vizOn && running ? <CampaignStrips hub={hub} /> : !s.workers.length && <p className="muted small">No workers running.</p>}
      </Row>
      <Row title="Log" storeKey="log"><pre className="log small">{s.log.join('\n') || (running ? '' : 'Nothing yet (the auto-miner\'s log).')}</pre></Row>
      <Row title="How to play" storeKey="mine-help"><Help /></Row>
      <Row title="Debug" storeKey="mine-debug">
        {last && (
          <div className="debug">
            <p className="muted">Last manual attempt: {last.pads.length} frames · block {last.template.height} · {last.result}</p>
            <Toolbar label="Debug downloads">
              <ToolBtn onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.m64`, toM64(last.pads))}>Download .m64 (this block's run)</ToolBtn>
              <ToolBtn onClick={() => download(`sm64coin-h${last.template.height}-${last.template.sm64_seed}.json`, JSON.stringify({
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
              }, null, 1), 'application/json')}>Download debug JSON</ToolBtn>
            </Toolbar>
          </div>
        )}
        <pre className="log">{log.join('\n')}</pre>
      </Row>
      <p className="muted small">
        <Link to="/leaderboard">Leaderboard</Link> · <a href="https://github.com/Gumbo64/sm64coin#writing-your-own-bot" target="_blank" rel="noreferrer">Write a bot</a> to mine for you.
      </p>
    </>
  );
}

// The "reset cycle" (power-on start): enabled exactly when the chain allows it (template
// sm64_start "either"), with a countdown while it is parent-only, and suggested when the
// parent's state looks stuck (every star taken, or a long time without a block).
function ResetCycle({ start, startMode, powerOnAt, task, onReset, onParent, tick }) {
  const nowS = Date.now() / 1000 + tick * 0;
  const wait = secondsUntilAllowed(startMode, powerOnAt, nowS);
  const onPower = start.kind === 'power-on';
  const stuck = !onPower && task && (start.progress.stars >= ALL_STARS || nowS - task.sm64_parent_timestamp > 1800);
  if (onPower) {
    return (
      <div className="notice">
        Playing a reset cycle: this block starts over from power-on.{' '}
        <button onClick={onParent}>Continue from the parent's end state instead</button>
      </div>
    );
  }
  return (
    <div className={`notice${stuck ? ' warn' : ''}`}>
      {startMode === 'either'
        ? <>The chain has stalled, so a block may also start over from power-on (a reset cycle){stuck ? <b>: suggested, the parent's state looks stuck.</b> : '.'}{' '}</>
        : <>Reset cycle (start over from power-on) becomes allowed in <b>{mmss(wait)}</b> if no block lands first.{' '}</>}
      <button disabled={startMode !== 'either'} onClick={onReset} title={startMode === 'either' ? 'Restart this block from power-on' : 'Not allowed yet: the chain only accepts the parent\'s start until the stall time passes'}>Reset cycle (start over from power-on)</button>
    </div>
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

// On-screen buttons and keyboard shortcuts for the recorder (keys the game doesn't use), as grouped toolbars: playback,
// speed, save states and load states (the fifth group, the actions, is PlayView's: new template, touch controls).
// useControls returns the groups as nodes so the page shows them under the game and the full-screen view lays them out
// around the picture.
const fmtT = (f) => { const s = Math.floor(f / 30); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };
export function useControls(rec, hybrid = false) {
  const [, force] = useState(0);
  useEffect(() => {
    const onKey = (e) => {
      const r = rec.current;
      if (!r || e.target.tagName === 'INPUT') return;
      const k = e.key;
      const mod = e.ctrlKey || e.metaKey || e.altKey;
      if (!mod && SAVE_KEYS.includes(k.toLowerCase()) && !e.shiftKey) r.saveSlot(SAVE_KEYS.indexOf(k.toLowerCase()));
      else if (!mod && LOAD_KEYS.includes(k) && !e.shiftKey) { if (!r.loadSlot(LOAD_KEYS.indexOf(k))) return; }
      else if (k === 'g' || k === 'G') r.restart();
      else if (k === 'Backspace') { e.preventDefault(); r.back(e.shiftKey ? 30 : 5); }
      else if (k === '[') r.back(1);
      else if (k === 'p' || k === 'P') r.togglePause();
      else if (k === 't' || k === 'T') r.takeOver();
      else if ((k === 'n' || k === 'N') && !hybrid) r.skip();
      else if (k >= '1' && k <= '6') r.setSpeed(SPEEDS[Number(k) - 1]);
      else return;
      force((x) => x + 1);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [rec, hybrid]);
  const act = (f) => () => { if (rec.current) { f(rec.current); force((x) => x + 1); } };
  const btn = (label, tip, f, on, extra = {}) => <ToolBtn key={tip} title={tip} aria-label={tip} on={on} onClick={act(f)} {...extra}>{label}</ToolBtn>;
  const r = rec.current;
  const playback = (
    <Toolbar label="Playback" key="playback">
      {btn('⟲', hybrid ? 'Back to the node\'s state (G)' : 'Restart the attempt (G)', (x) => x.restart())}
      {[[1, '['], [5, 'Backspace'], [10, 'D-pad up'], [30, 'Shift+Backspace']].map(([sec, k]) =>
        btn(`◀${sec}s`, `Back ${sec} s (${k})`, (x) => x.back(sec), false, { className: sec === 5 ? '' : 'minor' }))}
      {btn('⏸', 'Pause / resume (P)', (x) => x.togglePause(), r && r.paused)}
      {btn('▶', 'Take over during a replay (T)', (x) => x.takeOver())}
    </Toolbar>
  );
  const speed = (
    <Toolbar label="Speed" key="speed">
      {SPEEDS.map((sp, i) => btn(`${sp}×`, `Speed ${sp}× (${i + 1})`, (x) => x.setSpeed(sp), r && r.speed === sp))}
    </Toolbar>
  );
  const bank = r ? r.slots : null;
  const slotTip = (i, sl) => (sl ? `slot ${i + 1}: frame ${sl.frame} (${fmtT(sl.frame)}), ${courseLabel(sl.info.course)} area ${sl.info.area}, ${sl.info.stars} stars` : `slot ${i + 1} is empty`);
  const save = (
    <Toolbar label="Save state" key="save">
      {SAVE_KEYS.map((k, i) => btn(<>💾<b>{i + 1}</b></>, `Save the game and your recording to slot ${i + 1} (${k.toUpperCase()}); now: ${slotTip(i, bank && bank.get(i))}`, (x) => x.saveSlot(i), false, { className: 'slot' }))}
    </Toolbar>
  );
  const load = (
    <Toolbar label="Load state" key="load">
      {LOAD_KEYS.map((k, i) => {
        const sl = bank && bank.get(i);
        return <ToolBtn key={i} className="slot" disabled={!sl} aria-label={`Load slot ${i + 1}`} title={sl ? `Load ${slotTip(i, sl)}: the game and your recording go back to it (${k})` : `Slot ${i + 1} is empty: save first (${SAVE_KEYS[i].toUpperCase()})`}
          onClick={act((x) => x.loadSlot(i))}>⏏<b>{i + 1}</b>{sl && <span className="sl-t"> {fmtT(sl.frame)}</span>}</ToolBtn>;
      })}
    </Toolbar>
  );
  const used = bank ? bank.slots.map((sl, i) => (sl ? `${i + 1}: ${fmtT(sl.frame)} ${courseLabel(sl.info.course)}` : null)).filter(Boolean) : [];
  const info = used.length ? <p className="muted small slot-info" key="info">Slots · {used.join(' · ')}</p> : null;
  return { playback, speed, save, load, info };
}

// The groups, one after the other, under the game on the page.
export function Controls({ groups }) {
  return <>{groups.playback}{groups.speed}{groups.save}{groups.load}{groups.info}</>;
}

export function NewTemplateBtn({ rec }) {
  return <ToolBtn title="New template: restart on a fresh block template (N)" aria-label="New template (N)" onClick={() => rec.current && rec.current.skip()}>▶| new</ToolBtn>;
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
        start the next block from that one). When you finish a run that can't be sent yet, the game goes back a few seconds
        (↺, 3 s by default) on the same template so you can try for a shorter one; a shorter run replaces the waiting one,
        and your save states stay. Before the chain opens (mainnet: 2026-11-26 00:00 UTC) a finished run is kept and sent at
        the opening. Once a run is sent the template is used up (one run per template): you can practise on, but the next
        block is a new attempt (▶| new). "Block mined" shows only once the block is seen in the chain. Ties go to the shorter run: of several runs waiting, the shortest
        becomes valid first.</p>
      <p><b>If the chain stalls</b> (no block for 4 hours, e.g. nobody can get past a hard spot), a block may start over
        from power-on instead: the "Reset cycle" button here turns on (it counts down until then). The Grand Star also ends the
        cycle by itself.</p>
      <p><b>Game keys</b> (sm64-port defaults; a gamepad works too): WASD stick · <b>I</b> = A · <b>J</b> = B ·
        <b>O</b> = Z · <b>Space</b> = Start · arrows = C buttons · Q = L · Right Shift = R · D-pad: R key up, Left Shift down.</p>
      <p><b>Miner keys:</b> <b>G</b> restart the attempt (from the block's start) · <b>Backspace</b> back 5 s (<b>Shift+Backspace</b> 30 s, <b>[</b> 1 s) ·
        <b>P</b> pause · <b>T</b> or Start take over during a replay · <b>1</b>–<b>6</b> speed ¼×, ½×, 1×, 2×, 4×, 10× ·
        hold D-pad down (Left Shift) for 10× , D-pad up (R) to go back 10 s · <b>N</b> new template.</p>
      <p><b>Save states</b> (4 slots, like an emulator): <b>Z X C V</b> (or the 💾 buttons) save slot 1–4, <b>7 8 9 0</b> (or ⏏) load it. A slot keeps the game and your recording up to that moment; loading puts both back (the recording is exactly the pads that led there, even if it is longer than your current one), and you play on, so the run is still a valid proof. Slots belong to the current attempt's block template and are cleared when it changes (a new block, a new template, Play from the start toggled). In hybrid play they are relative to the segment.</p>
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
