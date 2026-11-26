//! The miner's input normaliser (no consensus change; nodes never call it). Port of the round-2
//! canonicaliser of branch compressor-research-2 (docs/COMPRESS2.md section 1).
//!
//! Many frames' pads don't change what the game does: the stick is ignored in the air and in
//! cutscenes, buttons mostly don't matter. A run's size depends on how regular its pads are, so the
//! miner may replace a pad by an *equivalent* one (the cheapest by a guide model) before it seals.
//! Equivalent means: the game's whole memory and the perturbation's state are bit-identical to the
//! original run's two frames later. Every normalised run is replayed again from its start and must
//! end in the same state with the same perturbation state, the same frame count, and the success
//! condition holding for the first time on the last frame; otherwise the caller keeps the original.
//!
//! Two settings (the write-up: the better one depends on the data): [`CONSERVATIVE`] (cheapest of 2
//! equivalents, only for a saving of at least 4 bits) and [`AGGRESSIVE`] (cheapest of up to 32).
//! [`seal_proof_normalised_at`] seals the original and both, in every sealing codec, and keeps the
//! smallest proof. The cost is seconds per block on the encoder; keep it off the search's hot path.
//! Poked frames are never touched (their pad comes from the perturbation).

use crate::game::{Features, SM64MiningGame, from_pad, perturbed_step, read_features, to_pad};
use crate::mining::{proof_from_played_at, seal_proof_at};
use crate::proof::Start;
use crate::rules::Params;
use crate::seed::Seed;
use anyhow::Result;
use sm64_binds::GamePad;
use sm64coin_statecodec::CostMeter;
use sm64coin_statecodec::mix2lab::{Cfg, Model};

/// One canonicaliser setting.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Try at most this many candidates (cheapest first) per frame.
    pub max_tries: usize,
    /// Take a substitution only if it saves at least this many bits now.
    pub margin_bits: f64,
}

/// Cheapest of 2 equivalent pads, only for a saving of at least 4 bits (best for miner runs).
pub const CONSERVATIVE: Settings = Settings { max_tries: 2, margin_bits: 4.0 };
/// Cheapest of up to 32 equivalent pads (better for human-like inputs).
pub const AGGRESSIVE: Settings = Settings { max_tries: 32, margin_bits: 1e-6 };

fn guide_cfg() -> Cfg {
    let mut c = Cfg::default();
    c.mru = true;
    c.stk_mru = true;
    c.ring = true;
    c.xc = 16;
    c
}

/// The game's whole memory, hashed.
fn mem_hash(g: &SM64MiningGame) -> u64 {
    g.game.with_memory(|m| {
        let mut h: u64 = 0xcbf29ce484222325;
        let (pre, mid, post) = unsafe { m.align_to::<u64>() };
        for &b in pre {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        let mut a = [h, 1, 2, 3];
        for c in mid.chunks_exact(4) {
            for k in 0..4 {
                a[k] = (a[k] ^ c[k]).wrapping_mul(0x9E3779B97F4A7C15).rotate_left(29);
            }
        }
        for &w in mid.chunks_exact(4).remainder() {
            a[0] = (a[0] ^ w).wrapping_mul(0x9E3779B97F4A7C15).rotate_left(29);
        }
        for &b in post {
            a[1] = (a[1] ^ b as u64).wrapping_mul(0x100000001b3);
        }
        a[0] ^ a[1].rotate_left(16) ^ a[2].rotate_left(32) ^ a[3].rotate_left(48)
    })
}

/// The part of the features that identifies a state cheaply (a prefilter before the full hash).
fn feat_key(f: &Features) -> [i32; 21] {
    [
        f.action as i32, f.prev_action as i32, f.flags as i32, f.input as i32, f.action_state as i32, f.action_timer as i32,
        f.intended_yaw as i32, f.intended_mag, f.face_yaw as i32, f.cam_yaw as i32, f.pos[0], f.pos[1], f.pos[2], f.fwd,
        f.vel[0], f.vel[1], f.vel[2], f.health as i32, f.level as i32, f.area as i32, f.timer as i32,
    ]
}

fn bits(b: u16) -> impl Iterator<Item = u16> {
    (0..16).map(|i| 1u16 << i).filter(move |m| b & m != 0)
}

/// The original run: per frame the wanted pad, whether it was poked, the features before it, and
/// the state (hash, perturbation) after every frame.
struct Orig {
    wanted: Vec<GamePad>,
    poked: Vec<bool>,
    keys: Vec<[i32; 21]>,
    hash: Vec<u64>,
    pert: Vec<crate::game::Perturber>,
}

fn orig_pass(mk: &dyn Fn() -> Result<SM64MiningGame>, played: &[GamePad]) -> Result<Orig> {
    let mut g = mk()?;
    let mut o = Orig { wanted: vec![], poked: vec![], keys: vec![], hash: vec![mem_hash(&g)], pert: vec![g.pert.clone()] };
    for &w in played {
        o.keys.push(feat_key(&read_features(&g.game)?));
        let (got, poked) = perturbed_step(&mut g.game, &mut g.pert, w)?;
        anyhow::ensure!(got == w, "the run to normalise isn't what the perturbation plays");
        o.wanted.push(w);
        o.poked.push(poked);
        o.hash.push(mem_hash(&g));
        o.pert.push(g.pert.clone());
    }
    Ok(o)
}

fn canonicalise(mk: &dyn Fn() -> Result<SM64MiningGame>, o: &Orig, s: &Settings) -> Result<Vec<GamePad>> {
    let n = o.wanted.len();
    let mut t = mk()?;
    let mut x = mk()?;
    let mut out: Vec<GamePad> = Vec::with_capacity(n);
    let mut prev = GamePad::default();
    let mut model = Model::new(guide_cfg());
    let mut meter = CostMeter::default();
    for i in 0..n {
        let want = o.wanted[i];
        let mut cur = want;
        let f = read_features(&t.game)?;
        if o.poked[i] {
            model.poked();
            let (played, _) = t.step(want)?;
            out.push(played);
            continue;
        }
        // Playing `c` now and the original's pads after it: the original's state at frame i+2?
        let mut ok = |c: GamePad| -> Result<bool> {
            x.sync_from(&t)?;
            x.step(c)?;
            let mut j = i + 1;
            while j < n.min(i + 2) {
                x.step(o.wanted[j])?;
                j += 1;
                if j >= i + 2 {
                    let same_feat = j < n && feat_key(&read_features(&x.game)?) == o.keys[j];
                    if (j >= n || same_feat) && mem_hash(&x) == o.hash[j] && x.pert == o.pert[j] {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        };
        if want != prev && i + 2 <= n {
            let mk_pad = |b: u16, s: (i32, i32)| GamePad::new(b, s.0.clamp(-128, 127) as i8, s.1.clamp(-128, 127) as i8);
            let ws = (want.stick_x as i32, want.stick_y as i32);
            let ps = (prev.stick_x as i32, prev.stick_y as i32);
            let mut cand: Vec<GamePad> = vec![];
            let mut push = |p: GamePad| {
                if p != want && !cand.contains(&p) {
                    cand.push(p)
                }
            };
            let favs: Vec<GamePad> = model.candidates(&f).into_iter().map(from_pad).collect();
            for &p in &favs {
                push(p);
            }
            for p in model.ring_candidates(&f, to_pad(want)).into_iter().map(from_pad) {
                push(p);
            }
            push(prev);
            push(mk_pad(prev.button, ws));
            push(mk_pad(want.button, ps));
            push(mk_pad(want.button, (0, 0)));
            push(mk_pad(prev.button, (0, 0)));
            for m in bits(want.button ^ prev.button) {
                push(mk_pad(want.button ^ m, ws));
            }
            let targets: Vec<(i32, i32)> = favs.iter().map(|p| (p.stick_x as i32, p.stick_y as i32)).take(3).chain([ps]).collect();
            for tg in &targets {
                for k in 1..8 {
                    push(mk_pad(want.button, ((ws.0 * (8 - k) + tg.0 * k).div_euclid(8), (ws.1 * (8 - k) + tg.1 * k).div_euclid(8))));
                }
            }
            for dx in -3..=3 {
                for dy in -3..=3 {
                    if (dx, dy) != (0, 0) {
                        push(mk_pad(want.button, (ws.0 + dx, ws.1 + dy)));
                    }
                }
            }
            // Radial: the same direction, other magnitudes (full-deflection sticks clamp).
            let m2 = ws.0 * ws.0 + ws.1 * ws.1;
            if m2 > 64 * 64 {
                let mg = (m2 as f64).sqrt();
                for r in [127, 120, 110, 100, 90, 80, 72] {
                    push(mk_pad(want.button, ((ws.0 as f64 * r as f64 / mg).round() as i32, (ws.1 as f64 * r as f64 / mg).round() as i32)));
                }
            }
            let mut price = |p: GamePad| model.cost_pad(&f, to_pad(p));
            let cw = price(want);
            let mut priced: Vec<(f64, GamePad)> = cand.into_iter().map(|p| (price(p), p)).filter(|(c, _)| *c < cw - s.margin_bits).collect();
            priced.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            for &(_, p) in priced.iter().take(s.max_tries) {
                if ok(p)? {
                    cur = p;
                    break;
                }
            }
        }
        let (played, _) = t.step(cur)?;
        anyhow::ensure!(played == cur, "frame {i}: the perturbation changed an unpoked pad");
        out.push(cur);
        model.code_pad(&mut meter, &f, to_pad(cur));
        prev = cur;
    }
    anyhow::ensure!(mem_hash(&t) == o.hash[n], "the normalised run ends in a different state");
    anyhow::ensure!(t.pert == o.pert[n], "the normalised run ends with a different perturbation state");
    Ok(out)
}

/// The run `played` (the pad played on every frame, as [`seal_proof_at`] takes it) with every pad
/// replaced that could be by a cheaper equivalent, per `s`. Checked: the result ends in the same
/// game memory and perturbation state, with the same frame count; Err otherwise (use the original).
pub fn normalise_played(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad], s: &Settings) -> Result<Vec<GamePad>> {
    let mk = || -> Result<SM64MiningGame> { Ok(crate::game::mining_start(start, parent, seed, p)?.0) };
    let o = orig_pass(&mk, played)?;
    let pads = canonicalise(&mk, &o, s)?;
    anyhow::ensure!(pads.len() == played.len(), "frame count changed");
    // Replay the result from its start, as a verifier would.
    let mut g = mk()?;
    for (i, &w) in pads.iter().enumerate() {
        let (got, _) = g.step(w)?;
        anyhow::ensure!(got == w, "frame {i}: replay plays {got:?}, not {w:?}");
    }
    anyhow::ensure!(mem_hash(&g) == o.hash[played.len()] && g.pert == o.pert[played.len()], "replay of the normalised run ends elsewhere");
    Ok(pads)
}

/// The controller bits 0x0080 and 0x0040 are reserved on the N64: Mupen64Plus treats a sample with
/// both set as a console reset, so an exported run containing one doesn't play back. The game never
/// reads them (tests/reserved_bits.rs: identical game logic with them forced on or off; the
/// perturbation's digest doesn't include the raw controller memory), and the poke pads only use A, B
/// and Z. Consensus accepts the bits; the miner just never seals a pad with both set.
pub const RESERVED_BITS: u16 = 0x00C0;

/// `played` with [`RESERVED_BITS`] cleared on every pad that has both set, if that leaves the run's
/// game logic identical (per frame: the perturbation's view, the features and the progress); the
/// run as given if no pad has both set or anything differs.
pub fn without_reserved_bits(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Vec<GamePad> {
    if !played.iter().any(|q| q.button & RESERVED_BITS == RESERVED_BITS) {
        return played.to_vec();
    }
    let cleared: Vec<GamePad> = played.iter().map(|q| if q.button & RESERVED_BITS == RESERVED_BITS { GamePad::new(q.button & !RESERVED_BITS, q.stick_x, q.stick_y) } else { *q }).collect();
    let same = || -> Result<bool> {
        let (mut a, mut b) = (crate::game::mining_start(start, parent, seed, p)?.0, crate::game::mining_start(start, parent, seed, p)?.0);
        for (x, y) in played.iter().zip(&cleared) {
            let (ra, rb) = (a.step(*x)?, b.step(*y)?);
            let key = |g: &SM64MiningGame| -> Result<_> { Ok((crate::game::read_view(&g.game)?.digest, feat_key(&read_features(&g.game)?), g.progress()?.points())) };
            if ra.1 != rb.1 || key(&a)? != key(&b)? || a.pert != b.pert {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if same().unwrap_or(false) { cleared } else { played.to_vec() }
}

/// What [`seal_proof_normalised_at`] did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The original run's best blob size.
    pub original: usize,
    /// Per setting (conservative, aggressive): the blob size, or None if it failed its checks or wasn't smaller.
    pub tried: [Option<usize>; 2],
    /// The size sealed.
    pub sealed: usize,
}

/// [`seal_proof_at`] with the normaliser first: the original run and both settings, each in every
/// sealing codec; the smallest proof wins. A normalised run is used only if its proof decodes to
/// the run and replays valid (success first holding on the last frame) with the original's
/// end state; anything else falls back to the original run, which is always a candidate.
pub fn seal_proof_normalised_at(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Result<Vec<u8>> {
    Ok(seal_proof_normalised_report(start, parent, seed, p, played)?.0)
}

/// [`seal_proof_normalised_at`] and what it measured.
pub fn seal_proof_normalised_report(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad]) -> Result<(Vec<u8>, Report)> {
    let as_played = played;
    let stripped = without_reserved_bits(start, parent, seed, p, played);
    let played = &stripped[..];
    let mut best = seal_proof_at(start, parent, seed, p, played)?;
    // The run with the reserved bits cleared is sealed only if it, too, decodes to itself and replays
    // valid (success first on its last frame), like every normalised candidate below.
    if played != as_played && !check_sealed(start, parent, seed, p, played, played, &best).unwrap_or(false) {
        let blob = seal_proof_at(start, parent, seed, p, as_played)?;
        let n = blob.len();
        return Ok((blob, Report { original: n, tried: [None, None], sealed: n }));
    }
    let mut rep = Report { original: best.len(), ..Default::default() };
    for (k, s) in [CONSERVATIVE, AGGRESSIVE].iter().enumerate() {
        let Ok(pads) = normalise_played(start, parent, seed, p, played, s) else { continue };
        if pads == played {
            continue;
        }
        let Ok(blob) = seal_proof_at(start, parent, seed, p, &pads) else { continue };
        if check_sealed(start, parent, seed, p, played, &pads, &blob).unwrap_or(false) {
            rep.tried[k] = Some(blob.len());
            if blob.len() < best.len() {
                best = blob;
            }
        }
    }
    rep.sealed = best.len();
    Ok((best, rep))
}

/// The sealed `blob` of the normalised `pads` decodes to the run of `pads`, and that run is valid
/// where the original's was: same frames, success on the last frame, the original's end state.
fn check_sealed(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad], pads: &[GamePad], blob: &[u8]) -> Result<bool> {
    let pr = crate::mining::decode_proof_any(blob, parent, seed, p)?;
    if pr != proof_from_played_at(start, parent, seed, p, pads)? || pr.frames as usize != played.len() {
        return Ok(false);
    }
    let (g0, before) = crate::game::mining_start(start, parent, seed, p)?;
    let Ok((end, _)) = crate::replay::replay(g0.game, before, seed, p, &pr, None, None)? else { return Ok(false) };
    let (o, _) = {
        let mut g = crate::game::mining_start(start, parent, seed, p)?.0;
        for &w in played {
            g.step(w)?;
        }
        (g, ())
    };
    Ok(mem_hash(&end) == mem_hash(&o) && end.pert == o.pert)
}

// ---------------------------------------------------------------------------------------------
// The replay-free canonicaliser (exact.rs): the exact rules of the input analysis instead of
// probing. One pass reads Mario's action per frame; the rules need no game; the result is replayed
// once and must end in the same memory and perturbation state.

/// What [`seal_proof_exact_report`] does after the exact rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// The exact rules only (replay-free: two replays of the run, no per-frame probing).
    Exact,
    /// The exact rules, then the replay-guided [`CONSERVATIVE`] pass on their result (candidates are
    /// sealed separately and the smallest wins).
    ExactThenReplay,
}

/// Which member of a stick's class [`exact_pads`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StickPick {
    /// The class representative (the most central stick), neutral where the action reads none.
    Rep,
    /// The cheapest member by the sealing codecs' guide model: the stick as played, the previous
    /// frame's, the representative, neutral and the model's favourite and ring sticks, whichever of
    /// them lies in the class. Same rules, same guarantee; the choice only prices them.
    Model,
}

/// The exact rules' canonical run, replay-checked: the same frame count, the same end memory and
/// perturbation state, poked frames as played. Err when the check fails (use the original).
pub fn exact_pads(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad], pick: StickPick) -> Result<(Vec<GamePad>, crate::exact::Stats)> {
    let mk = || -> Result<SM64MiningGame> { Ok(crate::game::mining_start(start, parent, seed, p)?.0) };
    // One pass: Mario's action at the start of each frame, which frames the perturbation pokes.
    let mut g = mk()?;
    let n = played.len();
    let (mut actions, mut poked, mut feats, mut digests) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    for &w in played {
        let f = read_features(&g.game)?;
        actions.push(f.action);
        if pick == StickPick::Model {
            feats.push(f);
        }
        let (got, pk) = perturbed_step(&mut g.game, &mut g.pert, w)?;
        anyhow::ensure!(got == w, "the run to canonicalise isn't what the perturbation plays");
        poked.push(pk);
        let v = crate::game::read_view(&g.game)?;
        digests.push((v.clock, v.digest));
    }
    let (end_hash, end_pert) = (mem_hash(&g), g.pert.clone());
    let (pads, stats) = match pick {
        StickPick::Rep => crate::exact::canonicalise(played, &actions, &poked),
        StickPick::Model => {
            let mut pl = crate::exact::plan(played, &actions, &poked);
            let mut model = Model::new(guide_cfg());
            let mut meter = CostMeter::default();
            let mut out: Vec<GamePad> = Vec::with_capacity(n);
            let mut prev = GamePad::default();
            for i in 0..n {
                let want = played[i];
                if poked[i] {
                    model.poked();
                    out.push(want);
                    prev = want;
                    continue;
                }
                let f = &feats[i];
                let mut cur = GamePad::new(pl.buttons[i], want.stick_x, want.stick_y);
                if !pl.fixed[i] && pl.atoms[i] != crate::exact::ALL_ATOMS {
                    let ws = (want.stick_x, want.stick_y);
                    let mut cand: Vec<(i8, i8)> = vec![ws, (prev.stick_x, prev.stick_y), (0, 0)];
                    if pl.atoms[i] != 0 {
                        cand.push(crate::exact::classes(pl.atoms[i]).rep_of(ws.0, ws.1));
                    }
                    for q in model.candidates(f).into_iter().map(from_pad).chain(model.ring_candidates(f, to_pad(cur)).into_iter().map(from_pad)) {
                        cand.push((q.stick_x, q.stick_y));
                    }
                    let mut best = (model.cost_pad(f, to_pad(cur)), ws);
                    for &c in &cand {
                        if c != ws && pl.same_class(i, c, ws) {
                            let cost = model.cost_pad(f, to_pad(GamePad::new(cur.button, c.0, c.1)));
                            if cost < best.0 - 1e-9 {
                                best = (cost, c);
                            }
                        }
                    }
                    cur = GamePad::new(cur.button, best.1.0, best.1.1);
                    if best.1 != ws {
                        pl.stats.sticks_changed += 1;
                        if best.1 == (0, 0) {
                            pl.stats.sticks_neutral += 1;
                        }
                    }
                }
                out.push(cur);
                model.code_pad(&mut meter, f, to_pad(cur));
                prev = cur;
            }
            (out, pl.stats)
        }
    };
    // The check: replay from the start as a verifier would.
    let mut g = mk()?;
    for (i, &w) in pads.iter().enumerate() {
        let (got, pk) = g.step(w)?;
        anyhow::ensure!(
            got == w && pk == poked[i] && {
                let v = crate::game::read_view(&g.game)?;
                (v.clock, v.digest) == digests[i]
            },
            "frame {i} (action {:#x}): the canonical pad {w:?} (the run's {:?}) leads elsewhere (played {got:?}); the frame before: canonical {:?}, run's {:?}, action {:#x}",
            actions[i], played[i], if i > 0 { pads[i - 1] } else { w }, if i > 0 { played[i - 1] } else { w }, if i > 0 { actions[i - 1] } else { 0 }
        );
    }
    anyhow::ensure!(mem_hash(&g) == end_hash && g.pert == end_pert, "the canonical run ends in a different state");
    Ok((pads, stats))
}

/// What [`seal_proof_exact_report`] measured.
#[derive(Clone, Debug, Default)]
pub struct ExactReport {
    /// The original run's best blob size.
    pub original: usize,
    /// The exact canonical run's blob (None: failed its check or no change).
    pub exact: Option<usize>,
    /// After the replay-guided pass on top (ExactThenReplay only).
    pub exact_then_replay: Option<usize>,
    pub sealed: usize,
    pub stats: crate::exact::Stats,
    /// Seconds spent in the exact canonicaliser (passes + check), and in the replay-guided pass.
    pub exact_secs: f64,
    pub replay_secs: f64,
    /// Seconds the plain [`seal_proof_at`] of the original run took (every codec).
    pub plain_secs: f64,
}

/// [`seal_proof_normalised_report`] with the exact canonicaliser: the original run and the exact
/// canonical run (and with [`Method::ExactThenReplay`] the replay-guided pass on top of it), each
/// sealed in every codec; a candidate is used only if its proof decodes to its run and replays
/// valid with the original's end state ([`check_sealed`]); the smallest wins, the original is
/// always a candidate.
pub fn seal_proof_exact_report(start: Start, parent: Option<&sm64_binds::SaveState>, seed: &Seed, p: &Params, played: &[GamePad], method: Method, pick: StickPick) -> Result<(Vec<u8>, ExactReport)> {
    let as_played = played;
    let stripped = without_reserved_bits(start, parent, seed, p, played);
    let played = &stripped[..];
    let t_plain = Stopwatch::start();
    let mut best = seal_proof_at(start, parent, seed, p, played)?;
    let plain_secs = t_plain.secs();
    if played != as_played && !check_sealed(start, parent, seed, p, played, played, &best).unwrap_or(false) {
        let blob = seal_proof_at(start, parent, seed, p, as_played)?;
        let n = blob.len();
        return Ok((blob, ExactReport { original: n, sealed: n, ..Default::default() }));
    }
    let mut rep = ExactReport { original: best.len(), plain_secs, ..Default::default() };
    let t0 = Stopwatch::start();
    let exact = exact_pads(start, parent, seed, p, played, pick);
    rep.exact_secs = t0.secs();
    let Ok((pads, stats)) = exact else { rep.sealed = best.len(); return Ok((best, rep)) };
    rep.stats = stats;
    if pads != played {
        if let Ok(blob) = seal_proof_at(start, parent, seed, p, &pads) {
            if check_sealed(start, parent, seed, p, played, &pads, &blob).unwrap_or(false) {
                rep.exact = Some(blob.len());
                if blob.len() < best.len() {
                    best = blob;
                }
            }
        }
    }
    if method == Method::ExactThenReplay {
        let t1 = Stopwatch::start();
        if let Ok(more) = normalise_played(start, parent, seed, p, &pads, &CONSERVATIVE) {
            if more != played {
                if let Ok(blob) = seal_proof_at(start, parent, seed, p, &more) {
                    if check_sealed(start, parent, seed, p, played, &more, &blob).unwrap_or(false) {
                        rep.exact_then_replay = Some(blob.len());
                        if blob.len() < best.len() {
                            best = blob;
                        }
                    }
                }
            }
        }
        rep.replay_secs = t1.secs();
    }
    rep.sealed = best.len();
    Ok((best, rep))
}

/// Wall-clock seconds for the reports. `std::time::Instant::now()` panics on wasm32-unknown-unknown (the
/// web miner), so there it reads 0.
struct Stopwatch(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);

impl Stopwatch {
    fn start() -> Self {
        Stopwatch(#[cfg(not(target_arch = "wasm32"))] std::time::Instant::now())
    }
    fn secs(&self) -> f64 {
        #[cfg(not(target_arch = "wasm32"))]
        return self.0.elapsed().as_secs_f64();
        #[cfg(target_arch = "wasm32")]
        0.0
    }
}
