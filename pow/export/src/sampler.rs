//! The branching sampler: counterfactual rollouts from states on the chain, for world-model data.
//!
//! The chain gives one trajectory per block (the winner's run) and, from v0.4, the other miners'
//! archived runs of every height. Those are narrow: inputs that reach the next star. This module
//! branches off them: from a state sampled on a chain segment (block boundaries, states along
//! winning runs and archived proofs) it plays K counterfactual rollouts of H frames each in the
//! emulator, with an explicit and recorded **sampler**, and writes `(state features, action,
//! next state features)` plus coverage metrics per sampler, so the bias of each is measured.
//!
//! A sampler is a pair (which states to branch from, which actions to play):
//!
//! | sampler | start states | actions |
//! |---|---|---|
//! | `uniform` | spread evenly over position cells (Scattershot-style: round-robin over cells) | random holds: a random stick direction/magnitude and A/B/Z held 4-24 frames |
//! | `objective` | random among states in the level where their segment ends | head for the segment's end position (where the star/key was taken) by a greedy stick policy, jump when below the target, epsilon-random holds |
//! | `random` | random | pink (1/f) noise on the stick and on A, B, Z |
//! | `jitter` | random | the segment's own next H pads with a growing share of random-hold windows (branch 0 is the unmodified continuation) |
//!
//! Output (`docs/SAMPLER.md`): per sampler `states.bin` (T x (H+1) x 27 u32, the `features.rs`
//! columns), `actions.bin` (T x H x 8 bytes), `trajectories.jsonl`, `coverage.json`; and a
//! `manifest.json` that records the settings, the schemas and a coverage comparison.
//!
//! The action space is what the chain's own miners and perturbation use: the stick and the A, B, Z
//! buttons. Each action carries its exact input class from the input-analysis tables
//! (`data/stick_intended.bin`: the 65,536 raw sticks in 20,129 classes that are indistinguishable to
//! `intendedMag` / `intendedYaw` / `INPUT_NONZERO_ANALOG`), and the buttons masked to the bits the
//! game reads.

use crate::export::{fresh_game, hash};
use crate::features::{self, RECORD_BYTES};
use crate::walk::{Seg, SegKind, walk};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sm64_binds::{GamePad, SM64Game, SaveState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "sm64coin-sampler-v1";

// Columns of the feature record (features.rs COLUMNS order).
pub const C_ACTION: usize = 1;
pub const C_CAM_YAW: usize = 10;
pub const C_POS: usize = 11;
pub const C_LEVEL: usize = 21;
pub const C_AREA: usize = 22;
pub const WORDS: usize = RECORD_BYTES / 4;

/// Horizontal cell size and height band (game units) of the coverage cells.
pub const CELL: f32 = 256.0;
pub const YBAND: f32 = 128.0;

pub const A: u16 = 0x8000;
pub const B: u16 = 0x4000;
pub const Z: u16 = 0x2000;
/// The buttons the game reads (the two reserved bits are not).
pub const VALID_BUTTONS: u16 = 0xFF3F;

// ---- the input-analysis stick table ----

/// The `intended` equivalence classes of the 65,536 raw sticks (sm64-binds `input-analysis`,
/// `stick_intended.bin`: `u32 n`, 65,536 `u16` class ids indexed by `(x as u8) << 8 | y as u8`,
/// `n` `u16` representatives). Two sticks of one class are indistinguishable to the game's
/// `intendedMag`, `intendedYaw` and `INPUT_NONZERO_ANALOG` (proven exhaustively there).
pub struct StickClasses {
    class_of: Vec<u16>,
    pub count: usize,
}

impl StickClasses {
    pub fn load() -> StickClasses {
        let b = include_bytes!("../data/stick_intended.bin");
        let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
        let class_of = (0..65536).map(|i| u16::from_le_bytes([b[4 + 2 * i], b[5 + 2 * i]])).collect();
        StickClasses { class_of, count: n }
    }
    pub fn class(&self, x: i8, y: i8) -> u16 {
        self.class_of[((x as u8 as usize) << 8) | y as u8 as usize]
    }
}

// ---- randomness ----

/// A small seeded generator (xorshift64*), so every trajectory is reproducible from its seed.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // splitmix the seed so nearby seeds give unrelated streams
        let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        Rng((z ^ (z >> 31)) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// Uniform in [0, 1).
    pub fn f(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in [0, n).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

fn mix(a: u64, b: u64) -> u64 {
    let mut r = Rng::new(a ^ b.wrapping_mul(0x9E3779B97F4A7C15));
    r.next_u64()
}

fn pad(buttons: u16, sx: f32, sy: f32) -> GamePad {
    GamePad::new(buttons, sx.round().clamp(-128.0, 127.0) as i8, sy.round().clamp(-128.0, 127.0) as i8)
}

// ---- policies ----

/// What a policy sees: the features of the current state (and the frame index).
pub struct Obs<'a> {
    pub t: usize,
    pub f: &'a [u32; WORDS],
}

impl Obs<'_> {
    pub fn pos(&self) -> [f32; 3] {
        [f32::from_bits(self.f[C_POS]), f32::from_bits(self.f[C_POS + 1]), f32::from_bits(self.f[C_POS + 2])]
    }
}

pub trait Policy {
    fn act(&mut self, o: &Obs) -> GamePad;
}

/// Random holds: every 4-24 frames a new random stick (uniform direction, magnitude from a few
/// levels) and a random subset of A, B, Z, held until the next draw.
pub struct HoldRandom {
    rng: Rng,
    left: usize,
    cur: GamePad,
}

impl HoldRandom {
    pub fn new(seed: u64) -> HoldRandom {
        HoldRandom { rng: Rng::new(seed), left: 0, cur: GamePad::default() }
    }
    pub fn draw(rng: &mut Rng) -> (GamePad, usize) {
        let hold = 4 + rng.below(21);
        let ang = rng.f() * std::f32::consts::TAU;
        let mag = [0.0, 0.0, 0.35, 0.65, 1.0, 1.0][rng.below(6)] * 127.0;
        let mut b = 0;
        if rng.f() < 0.25 {
            b |= A;
        }
        if rng.f() < 0.12 {
            b |= B;
        }
        if rng.f() < 0.12 {
            b |= Z;
        }
        (pad(b, mag * ang.sin(), mag * ang.cos()), hold)
    }
}

impl Policy for HoldRandom {
    fn act(&mut self, _: &Obs) -> GamePad {
        if self.left == 0 {
            (self.cur, self.left) = HoldRandom::draw(&mut self.rng);
        }
        self.left -= 1;
        self.cur
    }
}

/// Pink (1/f) noise: eight octave generators, generator k redrawn every 2^k frames, summed. One
/// stream each for stick x, stick y, A, B and Z.
pub struct Pink {
    rng: Rng,
    oct: [[f32; 8]; 5],
}

impl Pink {
    pub fn new(seed: u64) -> Pink {
        let mut rng = Rng::new(seed);
        let mut oct = [[0.0; 8]; 5];
        for s in oct.iter_mut() {
            for o in s.iter_mut() {
                *o = rng.f() * 2.0 - 1.0;
            }
        }
        Pink { rng, oct }
    }
    fn sample(&mut self, t: usize, s: usize) -> f32 {
        for k in 0..8 {
            if t % (1 << k) == 0 {
                self.oct[s][k] = self.rng.f() * 2.0 - 1.0;
            }
        }
        self.oct[s].iter().sum::<f32>() / 8f32.sqrt() / 1.6
    }
}

impl Policy for Pink {
    fn act(&mut self, o: &Obs) -> GamePad {
        let (x, y) = (self.sample(o.t, 0), self.sample(o.t, 1));
        let (a, b, z) = (self.sample(o.t, 2), self.sample(o.t, 3), self.sample(o.t, 4));
        let mut buttons = 0;
        if a > 0.45 {
            buttons |= A;
        }
        if b > 0.8 {
            buttons |= B;
        }
        if z > 0.8 {
            buttons |= Z;
        }
        pad(buttons, x * 127.0 * 1.5, y * 127.0 * 1.5)
    }
}

/// Heads for a target position: the stick that makes Mario's intended yaw point at it (the game's
/// `intendedYaw = atan2s(-stickY, stickX) + cameraYaw`, with `atan2s(y, x)` the angle of `(x, y)`
/// from +y toward +x, and Mario moving by `(sin yaw, cos yaw)`), A when the target is above and he
/// is on the ground or a short while into a jump, and with probability `eps` per hold a random hold
/// instead.
pub struct Objective {
    target: [f32; 3],
    rng: Rng,
    eps: f32,
    left: usize,
    cur: Option<GamePad>,
}

impl Objective {
    pub fn new(target: [f32; 3], eps: f32, seed: u64) -> Objective {
        Objective { target, rng: Rng::new(seed), eps, left: 0, cur: None }
    }
}

impl Policy for Objective {
    fn act(&mut self, o: &Obs) -> GamePad {
        if self.left > 0 {
            self.left -= 1;
            if let Some(p) = self.cur {
                return p;
            }
        }
        let p = o.pos();
        let (dx, dy, dz) = (self.target[0] - p[0], self.target[1] - p[1], self.target[2] - p[2]);
        if self.rng.f() < self.eps {
            let (p, hold) = HoldRandom::draw(&mut self.rng);
            self.cur = Some(p);
            self.left = hold.min(8);
            return p;
        }
        self.cur = None;
        let want = dx.atan2(dz); // yaw of the target, radians from +z toward +x
        let cam = o.f[C_CAM_YAW] as u16 as f32 * std::f32::consts::TAU / 65536.0;
        let phi = want - cam; // angle of the stick: sx = sin phi, -sy = cos phi
        let near = (dx * dx + dz * dz).sqrt() < 60.0;
        let mag = if near { 0.0 } else { 127.0 };
        let mut buttons = 0;
        if dy > 40.0 && o.t % 20 < 10 {
            buttons |= A;
        }
        pad(buttons, mag * phi.sin(), -mag * phi.cos())
    }
}

/// The segment's own continuation with random-hold windows spliced in: every 8-frame window is
/// replaced with probability `rate` by a random hold. Rate 0 is the chain's own continuation.
pub struct Jitter {
    own: Vec<GamePad>,
    rate: f32,
    rng: Rng,
    window: usize,
    replace: Option<GamePad>,
    fallback: HoldRandom,
}

impl Jitter {
    pub fn new(own: Vec<GamePad>, rate: f32, seed: u64) -> Jitter {
        Jitter { own, rate, rng: Rng::new(seed), window: usize::MAX, replace: None, fallback: HoldRandom::new(seed ^ 0xABCD) }
    }
}

impl Policy for Jitter {
    fn act(&mut self, o: &Obs) -> GamePad {
        let Some(&chain) = self.own.get(o.t) else { return self.fallback.act(o) };
        let w = o.t / 8;
        if w != self.window {
            self.window = w;
            self.replace = (self.rng.f() < self.rate).then(|| HoldRandom::draw(&mut self.rng).0);
        }
        self.replace.unwrap_or(chain)
    }
}

// ---- cells and coverage ----

pub type Cell = (u32, u32, i32, i32, i32);

pub fn cell_of(f: &[u32; WORDS]) -> Cell {
    let p = [f32::from_bits(f[C_POS]), f32::from_bits(f[C_POS + 1]), f32::from_bits(f[C_POS + 2])];
    (f[C_LEVEL], f[C_AREA], (p[0] / CELL).floor() as i32, (p[2] / CELL).floor() as i32, (p[1] / YBAND).floor() as i32)
}

fn words(rec: &[u8; RECORD_BYTES]) -> [u32; WORDS] {
    let mut w = [0u32; WORDS];
    for (i, c) in rec.chunks_exact(4).enumerate() {
        w[i] = u32::from_le_bytes(c.try_into().unwrap());
    }
    w
}

fn feats(g: &SM64Game) -> Result<([u32; WORDS], [u8; RECORD_BYTES])> {
    let r = features::record(g)?;
    Ok((words(&r), r))
}

fn entropy_bits<K>(h: &HashMap<K, u64>) -> f64 {
    let n: u64 = h.values().sum();
    if n == 0 {
        return 0.0;
    }
    // summed in a fixed order: the sum must not depend on the hash map's iteration order
    let mut c: Vec<u64> = h.values().copied().collect();
    c.sort_unstable();
    c.iter().map(|&c| -(c as f64 / n as f64) * (c as f64 / n as f64).log2()).sum()
}

/// The names of the stick regions: neutral, or 8 directions x 3 magnitude bins (by the processed stick).
pub fn stick_region(sx: i8, sy: i8) -> String {
    let (x, y) = (sx as f32, sy as f32);
    let m = (x * x + y * y).sqrt();
    if sx.unsigned_abs() < 8 && sy.unsigned_abs() < 8 {
        return "neutral".into();
    }
    let dir = ((y.atan2(x) / std::f32::consts::TAU * 8.0).round() as i32).rem_euclid(8);
    let names = ["E", "NE", "N", "NW", "W", "SW", "S", "SE"];
    format!("{}:{}", names[dir as usize], if m < 45.0 { "low" } else if m < 85.0 { "mid" } else { "high" })
}

pub fn button_combo(b: u16) -> String {
    let mut s = Vec::new();
    for (bit, n) in [(A, "A"), (B, "B"), (Z, "Z"), (0x1000, "Start"), (0x0008, "Cup"), (0x0004, "Cdown"), (0x0002, "Cleft"), (0x0001, "Cright"), (0x0020, "L"), (0x0010, "R")] {
        if b & bit != 0 {
            s.push(n);
        }
    }
    if s.is_empty() { "none".into() } else { s.join("+") }
}

/// The Mario action groups (`ACT_GROUP_MASK` 0x1C0 of the action id).
pub fn action_group(a: u32) -> &'static str {
    ["stationary", "moving", "airborne", "submerged", "cutscene", "automatic", "object", "unknown"][((a >> 6) & 7) as usize]
}

#[derive(Default)]
pub struct Coverage {
    pub trajectories: u64,
    pub frames: u64,
    /// Visits per cell by rollout states (frame 1..=H).
    pub cells: HashMap<Cell, u64>,
    /// Cells of the start states.
    pub start_cells: HashSet<Cell>,
    pub stick_classes: HashSet<u16>,
    pub buttons: HashMap<String, u64>,
    pub stick_regions: HashMap<String, u64>,
    pub groups: HashMap<String, u64>,
    pub mario_actions: HashMap<u32, u64>,
    pub objective: Vec<(f32, f32, f32)>, // (distance at start, at end, minimum)
    pub levels: HashMap<u32, u64>,
}

impl Coverage {
    fn json(&self, chain_cells: &HashSet<Cell>, stick: &StickClasses) -> Value {
        let cells: HashSet<Cell> = self.cells.keys().copied().collect();
        let floors: HashSet<(u32, u32, i32)> = cells.iter().map(|c| (c.0, c.1, c.4)).collect();
        let new_cells = cells.difference(chain_cells).count();
        let mut acts: Vec<_> = self.mario_actions.iter().collect();
        acts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let sorted = |h: &HashMap<String, u64>| h.iter().map(|(k, v)| (k.clone(), *v)).collect::<BTreeMap<_, _>>();
        let obj = if self.objective.is_empty() {
            Value::Null
        } else {
            let n = self.objective.len() as f32;
            let closing = self.objective.iter().filter(|o| o.1 < o.0).count() as f32;
            json!({
                "trajectories": self.objective.len(),
                "mean_distance_start": self.objective.iter().map(|o| o.0).sum::<f32>() / n,
                "mean_distance_end": self.objective.iter().map(|o| o.1).sum::<f32>() / n,
                "mean_distance_min": self.objective.iter().map(|o| o.2).sum::<f32>() / n,
                "fraction_closing": closing / n,
            })
        };
        let level_hist: BTreeMap<String, u64> = self.levels.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        json!({
            "trajectories": self.trajectories,
            "frames": self.frames,
            "cell_size": CELL, "height_band": YBAND,
            "cells_visited": cells.len(),
            "cells_new_vs_chain": new_cells,
            "cells_start_states": self.start_cells.len(),
            "floors_visited": floors.len(),
            "levels": level_hist,
            "stick_classes_used": self.stick_classes.len(),
            "stick_classes_total": stick.count,
            "buttons_hist": sorted(&self.buttons),
            "stick_region_hist": sorted(&self.stick_regions),
            "mario_action_group_hist": sorted(&self.groups),
            "mario_actions_distinct": self.mario_actions.len(),
            "mario_actions_top": acts.iter().take(16).map(|(a, n)| json!({"action": format!("{:#010x}", a), "group": action_group(**a), "count": n})).collect::<Vec<_>>(),
            "entropy_bits": {
                "cells": entropy_bits(&self.cells),
                "buttons": entropy_bits(&self.buttons),
                "stick_regions": entropy_bits(&self.stick_regions),
                "mario_action_groups": entropy_bits(&self.groups),
            },
            "objective": obj,
        })
    }
}

// ---- the sampler run ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Uniform,
    Objective,
    Random,
    Jitter,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Uniform => "uniform",
            Kind::Objective => "objective",
            Kind::Random => "random",
            Kind::Jitter => "jitter",
        }
    }
    pub fn parse(s: &str) -> Result<Kind> {
        Ok(match s {
            "uniform" => Kind::Uniform,
            "objective" => Kind::Objective,
            "random" => Kind::Random,
            "jitter" => Kind::Jitter,
            x => bail!("unknown sampler {x:?} (uniform, objective, random, jitter)"),
        })
    }
    pub const ALL: [Kind; 4] = [Kind::Uniform, Kind::Objective, Kind::Random, Kind::Jitter];
}

#[derive(Clone, Debug)]
pub struct Options {
    pub out: PathBuf,
    pub samplers: Vec<Kind>,
    /// Start states per sampler.
    pub states: usize,
    /// Branches per start state, frames per branch.
    pub branches: usize,
    pub horizon: usize,
    /// Spacing of the candidate start states along each segment (frames).
    pub stride: usize,
    pub seed: u64,
    /// Only segments of heights within this range.
    pub heights: Option<(u64, u64)>,
    /// Epsilon of the objective sampler (share of random holds).
    pub eps: f32,
    /// Do not branch from states in a Mario cutscene action (group 0x100: the game ignores the pad there,
    /// intros and star cutscenes), which carry no action information.
    pub skip_cutscenes: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { out: PathBuf::from("sample"), samplers: Kind::ALL.to_vec(), states: 200, branches: 4, horizon: 90, stride: 240, seed: 1, heights: None, eps: 0.15, skip_cutscenes: true }
    }
}

type SegKey = (u8, u64, String);

fn key(s: &Seg) -> SegKey {
    (s.kind as u8, s.height, s.name.clone())
}

#[derive(Clone, Debug)]
struct Cand {
    seg: SegKey,
    offset: usize,
    cell: Cell,
    /// The segment's end position (the objective) and whether Mario is in the level it ends in.
    target: Option<[f32; 3]>,
}

fn end_target(s: &Seg) -> Option<([f32; 3], u32)> {
    let p = s.meta["end_pos"].as_array()?;
    let t = [p[0].as_f64()? as f32, p[1].as_f64()? as f32, p[2].as_f64()? as f32];
    Some((t, s.meta["end_level_num"].as_u64()? as u32))
}

/// Steps `game` through the segment from its start, calling `f(offset, game)` at every frame
/// offset that is a multiple of `stride` (including 0).
fn along(game: &mut SM64Game, seg: &Seg, stride: usize, f: &mut dyn FnMut(usize, &mut SM64Game) -> Result<()>) -> Result<()> {
    game.load_state(&seg.start)?;
    for (o, p) in seg.pads.iter().enumerate() {
        if o % stride == 0 {
            f(o, game)?;
        }
        game.step_game(*p)?;
    }
    Ok(())
}

struct Writers {
    states: std::io::BufWriter<std::fs::File>,
    actions: std::io::BufWriter<std::fs::File>,
    traj: std::io::BufWriter<std::fs::File>,
    cov: Coverage,
    n: u64,
    dir: PathBuf,
}

pub struct Summary {
    pub samplers: Vec<(Kind, u64, u64, usize)>, // kind, trajectories, frames, cells
    pub candidates: usize,
    pub chain_cells: usize,
}

/// Runs the samplers over the export in `dir`.
pub fn sample(dir: &Path, o: &Options, log: &mut dyn FnMut(&str)) -> Result<Summary> {
    ensure!(!o.samplers.is_empty() && o.branches > 0 && o.horizon > 0 && o.stride > 0 && o.states > 0, "bad sampler options");
    std::fs::create_dir_all(&o.out)?;
    let stick = StickClasses::load();
    let in_range = |h: u64| o.heights.is_none_or(|(a, b)| h >= a && h <= b);
    let mut game = fresh_game()?;

    // Pass 1: candidates along every segment, and the cells the chain itself visits.
    let mut cands: Vec<Cand> = Vec::new();
    let mut chain_cells: HashSet<Cell> = HashSet::new();
    let mut seg_len: HashMap<SegKey, usize> = HashMap::new();
    walk(dir, None, &mut |m| log(m), &mut |seg| {
        if !in_range(seg.height) {
            return Ok(());
        }
        let k = key(seg);
        seg_len.insert(k.clone(), seg.pads.len());
        let tgt = end_target(seg);
        game.load_state(&seg.start)?;
        for (o_, p) in seg.pads.iter().enumerate() {
            if o_ % 8 == 0 || o_ % o.stride == 0 {
                let (w, _) = feats(&game)?;
                chain_cells.insert(cell_of(&w));
                if o_ % o.stride == 0 && !(o.skip_cutscenes && action_group(w[C_ACTION]) == "cutscene") {
                    let target = tgt.filter(|t| t.1 == w[C_LEVEL]).map(|t| t.0);
                    cands.push(Cand { seg: k.clone(), offset: o_, cell: cell_of(&w), target });
                }
            }
            game.step_game(*p)?;
        }
        Ok(())
    })?;
    ensure!(!cands.is_empty(), "no candidate states (empty export or height range)");
    log(&format!("{} candidate start states, {} chain cells", cands.len(), chain_cells.len()));

    // Selection per sampler.
    let mut chosen: HashMap<Kind, HashSet<(SegKey, usize)>> = HashMap::new();
    for &kind in &o.samplers {
        let mut rng = Rng::new(mix(o.seed, kind as u64 + 100));
        let mut idx: Vec<usize> = (0..cands.len()).collect();
        if kind == Kind::Objective {
            // Only states with a reachable objective (in the level the segment ends in).
            let with: Vec<usize> = idx.iter().copied().filter(|&i| cands[i].target.is_some()).collect();
            if !with.is_empty() {
                idx = with;
            }
        }
        for i in (1..idx.len()).rev() {
            idx.swap(i, rng.below(i + 1));
        }
        let want = o.states.min(idx.len());
        let sel: Vec<usize> = if kind == Kind::Uniform {
            // Round-robin over cells (random cell order, random candidate within a cell) until `want`.
            let mut by_cell: BTreeMap<Cell, Vec<usize>> = BTreeMap::new();
            for &i in &idx {
                by_cell.entry(cands[i].cell).or_default().push(i);
            }
            let mut buckets: Vec<Vec<usize>> = by_cell.into_values().collect();
            let mut out = Vec::new();
            while out.len() < want {
                for i in (1..buckets.len()).rev() {
                    buckets.swap(i, rng.below(i + 1));
                }
                for b in buckets.iter_mut() {
                    if out.len() < want {
                        if let Some(c) = b.pop() {
                            out.push(c);
                        }
                    }
                }
                buckets.retain(|b| !b.is_empty());
                if buckets.is_empty() {
                    break;
                }
            }
            out
        } else {
            idx.into_iter().take(want).collect()
        };
        chosen.insert(kind, sel.iter().map(|&i| (cands[i].seg.clone(), cands[i].offset)).collect());
    }
    let cand_by: HashMap<(SegKey, usize), Cand> = cands.iter().map(|c| ((c.seg.clone(), c.offset), c.clone())).collect();

    // Writers.
    let mut ws: BTreeMap<Kind, Writers> = BTreeMap::new();
    for &kind in &o.samplers {
        let d = o.out.join(kind.name());
        std::fs::create_dir_all(&d)?;
        let mk = |n: &str| -> Result<_> { Ok(std::io::BufWriter::new(std::fs::File::create(d.join(n))?)) };
        ws.insert(kind, Writers { states: mk("states.bin")?, actions: mk("actions.bin")?, traj: mk("trajectories.jsonl")?, cov: Coverage::default(), n: 0, dir: d });
    }

    // Pass 2: at every chosen (segment, offset), snapshot and branch.
    let mut roll = fresh_game()?;
    let n_states: usize = chosen.values().map(|s| s.len()).sum();
    let mut done = 0usize;
    walk(dir, None, &mut |_| {}, &mut |seg| {
        if !in_range(seg.height) {
            return Ok(());
        }
        let k = key(seg);
        if !o.samplers.iter().any(|kind| chosen[kind].iter().any(|(s, _)| *s == k)) {
            return Ok(());
        }
        along(&mut game, seg, o.stride, &mut |off, g| {
            let want: Vec<Kind> = o.samplers.iter().copied().filter(|kind| chosen[kind].contains(&(k.clone(), off))).collect();
            if want.is_empty() {
                return Ok(());
            }
            let snap = g.save_state();
            let start_hash = hash(&snap);
            let cand = &cand_by[&(k.clone(), off)];
            for kind in want {
                branch(&mut roll, &snap, &start_hash, seg, off, kind, cand, o, &stick, ws.get_mut(&kind).unwrap())?;
                done += 1;
            }
            if done % 25 == 0 {
                log(&format!("{done}/{n_states} start states"));
            }
            Ok(())
        })
    })?;

    // Coverage and manifest.
    let mut summary = Summary { samplers: vec![], candidates: cands.len(), chain_cells: chain_cells.len() };
    let mut table = Vec::new();
    for (kind, mut w) in ws {
        w.states.flush()?;
        w.actions.flush()?;
        w.traj.flush()?;
        let cj = w.cov.json(&chain_cells, &stick);
        std::fs::write(w.dir.join("coverage.json"), serde_json::to_string_pretty(&cj)?)?;
        summary.samplers.push((kind, w.n, w.cov.frames, w.cov.cells.len()));
        table.push((kind.name(), cj));
    }
    let man = json!({
        "format": FORMAT,
        "source_export": dir.file_name().map(|n| n.to_string_lossy().into_owned()),
        "source_manifest_sha256": crate::snapshot::sha256_hex(&std::fs::read(dir.join("manifest.json"))?),
        "game_build": sm64pow::GAME_BUILD,
        "settings": {"samplers": o.samplers.iter().map(|k| k.name()).collect::<Vec<_>>(), "states_per_sampler": o.states, "branches": o.branches, "horizon": o.horizon, "stride": o.stride, "seed": o.seed, "objective_eps": o.eps, "skip_cutscenes": o.skip_cutscenes, "heights": o.heights},
        "candidates": cands.len(),
        "chain_cells": chain_cells.len(),
        "arrays": {
            "states.bin": {"dtype": "little-endian u32 x 27 per record (features.schema.json columns of the export; f32 columns are bit patterns)", "shape": ["trajectories", o.horizon + 1, WORDS], "numpy": features::schema()["numpy"], "doc": "record 0 is the state before the first action, record t+1 the state after action t"},
            "actions.bin": {"dtype": "8 bytes per action", "shape": ["trajectories", o.horizon], "numpy": "np.dtype([('button','<u2'),('stick_x','i1'),('stick_y','i1'),('stick_class','<u2'),('button_class','<u2')])",
                "doc": "button = the pad's 16 button bits (A 0x8000, B 0x4000, Z 0x2000); stick_class = the exact class of the stick among 20,129 (input-analysis intended table); button_class = button & 0xFF3F (the bits the game reads)"},
            "trajectories.jsonl": {"doc": "one line per trajectory, in array order: sampler, source segment, frame offset, branch, seed, start state hash, policy parameters, objective"},
        },
        "coverage": table.iter().map(|(n, c)| (n.to_string(), c.clone())).collect::<BTreeMap<_, _>>(),
    });
    std::fs::write(o.out.join("manifest.json"), serde_json::to_string_pretty(&man)?)?;
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn branch(g: &mut SM64Game, snap: &SaveState, start_hash: &str, seg: &Seg, off: usize, kind: Kind, cand: &Cand, o: &Options, stick: &StickClasses, w: &mut Writers) -> Result<()> {
    for b in 0..o.branches {
        let seed = mix(mix(o.seed, kind as u64), mix(seg.height * 1000 + off as u64, b as u64 + 7));
        let own: Vec<GamePad> = seg.pads.get(off..).unwrap_or(&[]).iter().take(o.horizon).copied().collect();
        let mut jitter_rate = None;
        let mut policy: Box<dyn Policy> = match kind {
            Kind::Uniform => Box::new(HoldRandom::new(seed)),
            Kind::Random => Box::new(Pink::new(seed)),
            Kind::Objective => match cand.target {
                Some(t) => Box::new(Objective::new(t, o.eps, seed)),
                None => Box::new(HoldRandom::new(seed)),
            },
            Kind::Jitter => {
                let r = if o.branches > 1 { 0.6 * b as f32 / (o.branches - 1) as f32 } else { 0.3 };
                jitter_rate = Some(r);
                Box::new(Jitter::new(own, r, seed))
            }
        };
        g.load_state(snap)?;
        let (mut cur, rec0) = feats(g)?;
        let start_f = cur;
        w.states.write_all(&rec0)?;
        w.cov.start_cells.insert(cell_of(&cur));
        let mut dmin = f32::INFINITY;
        let dist = |f: &[u32; WORDS], t: &[f32; 3]| {
            let p = Obs { t: 0, f }.pos();
            ((p[0] - t[0]).powi(2) + (p[1] - t[1]).powi(2) + (p[2] - t[2]).powi(2)).sqrt()
        };
        // the distance to the segment's end position is measured for every sampler, so the objective bias is a comparison
        let obj_target = cand.target;
        for t in 0..o.horizon {
            let p = policy.act(&Obs { t, f: &cur });
            let class = stick.class(p.stick_x, p.stick_y);
            let mut a = [0u8; 8];
            a[..2].copy_from_slice(&p.button.to_le_bytes());
            a[2] = p.stick_x as u8;
            a[3] = p.stick_y as u8;
            a[4..6].copy_from_slice(&class.to_le_bytes());
            a[6..8].copy_from_slice(&(p.button & VALID_BUTTONS).to_le_bytes());
            w.actions.write_all(&a)?;
            g.step_game(p)?;
            let (next, rec) = feats(g)?;
            w.states.write_all(&rec)?;
            let c = &mut w.cov;
            *c.cells.entry(cell_of(&next)).or_default() += 1;
            c.stick_classes.insert(class);
            *c.buttons.entry(button_combo(p.button & VALID_BUTTONS)).or_default() += 1;
            *c.stick_regions.entry(stick_region(p.stick_x, p.stick_y)).or_default() += 1;
            *c.groups.entry(action_group(next[C_ACTION]).into()).or_default() += 1;
            *c.mario_actions.entry(next[C_ACTION]).or_default() += 1;
            *c.levels.entry(next[C_LEVEL]).or_default() += 1;
            if let Some(tg) = &obj_target {
                dmin = dmin.min(dist(&next, tg));
            }
            cur = next;
        }
        w.cov.trajectories += 1;
        w.cov.frames += o.horizon as u64;
        let objective = obj_target.map(|tg| {
            let (d0, d1) = (dist(&start_f, &tg), dist(&cur, &tg));
            w.cov.objective.push((d0, d1, dmin.min(d0)));
            json!({"target": tg, "distance_start": d0, "distance_end": d1, "distance_min": dmin.min(d0)})
        });
        let mut line = json!({
            "trajectory": w.n, "sampler": kind.name(),
            "segment": {"kind": match seg.kind { SegKind::Block => "block", SegKind::Record => "record" }, "height": seg.height, "name": seg.name, "cycle": seg.cycle},
            "frame_offset": off, "branch": b, "seed": seed, "start_state_hash": start_hash,
            "jitter_rate": jitter_rate, "objective": objective,
        });
        let sc = cell_of(&start_f);
        line["start_cell"] = json!({"level": sc.0, "area": sc.1, "x": sc.2, "z": sc.3, "y": sc.4});
        writeln!(w.traj, "{}", line)?;
        w.n += 1;
    }
    Ok(())
}

/// Reads a sampler's arrays back: (states [T][H+1][27] as u32, actions [T][H] as (button, sx, sy, stick_class, button_class)).
pub fn read_arrays(dir: &Path, horizon: usize) -> Result<(Vec<u32>, Vec<(u16, i8, i8, u16, u16)>)> {
    let s = std::fs::read(dir.join("states.bin")).context("states.bin")?;
    let a = std::fs::read(dir.join("actions.bin")).context("actions.bin")?;
    ensure!(s.len() % ((horizon + 1) * RECORD_BYTES) == 0 && a.len() % (horizon * 8) == 0, "array sizes do not match the horizon");
    let states = s.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let actions = a.chunks_exact(8).map(|c| (u16::from_le_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8, u16::from_le_bytes([c[4], c[5]]), u16::from_le_bytes([c[6], c[7]]))).collect();
    Ok((states, actions))
}
