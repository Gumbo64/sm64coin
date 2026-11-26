//! The miner's replay-free input canonicaliser (no consensus change; nodes never call it). It
//! applies the exact rules of the input analysis (sm64-binds branch `input-analysis`,
//! docs/INPUTS.md) instead of probing the game with substitute pads:
//!
//! * the stick of a frame becomes the *class representative* of the stick atoms Mario's current
//!   action consumes (neutral when it consumes none),
//! * button levels and press edges the action does not consume are cleared, by an exact dynamic
//!   programme per bit over the whole run (fewest set bits, then fewest transitions),
//! * poked frames (the perturbation overrides the whole pad), the first frame, the last
//!   [`PIN_TAIL`] frames and unknown actions are left alone.
//!
//! What a frame consumes comes from the per-action model ([`crate::exact_table`], generated from
//! the analysis' corpus-and-probe model), keyed by Mario's action at the start of the frame. That
//! is one replay of the run to read the actions, no per-frame probing. The model is a superset
//! built from corpora, not a proof for every state, so the caller must replay the result and
//! compare the end state (see [`crate::normalise::seal_proof_exact_report`]).
//!
//! Stick atoms, the stick pipeline (`adjust_analog_stick`, `atan2s`) and the class representatives
//! are ports of inputs/src/stick.rs, verified there against the game for all 65,536 raw sticks.

use crate::exact_arctan::ARCTAN;
use crate::exact_table::ROWS;
use sm64_binds::GamePad;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

// Stick atoms (partitions of the raw sticks), as inputs/src/trace.rs ATOMS.
pub const AT_NONZERO: u32 = 1 << 0;
pub const AT_IMAG: u32 = 1 << 1;
pub const AT_IYAW: u32 = 1 << 2;
pub const AT_SX: u32 = 1 << 3;
pub const AT_SY: u32 = 1 << 4;
pub const AT_SMAG: u32 = 1 << 5;
pub const AT_SCRX: u32 = 1 << 10;
pub const AT_SCRY: u32 = 1 << 11;
pub const ALL_ATOMS: u32 = (1 << 15) - 1;

pub const BTN_A: u16 = 0x8000;
pub const BTN_B: u16 = 0x4000;
pub const BTN_START: u16 = 0x1000;
/// Bits the game masks off (`VALID_BUTTONS`): never consumed.
pub const VALID_BUTTONS: u16 = 0xFF3F;
/// Frames at the end of the run whose pads are left as played, so that the controller latches in
/// the end state are those of the original run.
pub const PIN_TAIL: usize = 3;

// ---------------------------------------------------------------------------------------------
// The stick pipeline.

#[derive(Clone, Copy)]
struct Processed {
    x: f32,
    y: f32,
    mag: f32,
}

fn process(rx: i8, ry: i8) -> Processed {
    let (rx, ry) = (rx as i32, ry as i32);
    let (mut x, mut y) = (0f32, 0f32);
    if rx <= -8 {
        x = (rx + 6) as f32;
    }
    if rx >= 8 {
        x = (rx - 6) as f32;
    }
    if ry <= -8 {
        y = (ry + 6) as f32;
    }
    if ry >= 8 {
        y = (ry - 6) as f32;
    }
    let mut mag = (x * x + y * y).sqrt();
    if mag > 64.0 {
        let k = 64.0f32 / mag;
        x *= k;
        y *= k;
        mag = 64.0;
    }
    Processed { x, y, mag }
}

fn atan2_lookup(y: f32, x: f32) -> u16 {
    if x == 0.0 { ARCTAN[0] as u16 } else { ARCTAN[(y / x * 1024.0 + 0.5f32) as i32 as usize] as u16 }
}

fn atan2s(mut y: f32, mut x: f32) -> i16 {
    let ret: u16 = if x >= 0.0 {
        if y >= 0.0 {
            if y >= x { atan2_lookup(x, y) } else { 0x4000u16.wrapping_sub(atan2_lookup(y, x)) }
        } else {
            y = -y;
            if y < x { 0x4000u16.wrapping_add(atan2_lookup(y, x)) } else { 0x8000u16.wrapping_sub(atan2_lookup(x, y)) }
        }
    } else {
        x = -x;
        if y < 0.0 {
            y = -y;
            if y >= x { 0x8000u16.wrapping_add(atan2_lookup(x, y)) } else { 0xC000u16.wrapping_sub(atan2_lookup(y, x)) }
        } else if y < x {
            0xC000u16.wrapping_add(atan2_lookup(y, x))
        } else {
            0u16.wrapping_sub(atan2_lookup(x, y))
        }
    };
    ret as i16
}

fn intended(p: Processed) -> (u32, i64) {
    let q = p.mag / 64.0;
    let mag = ((q * q) * 64.0) / 2.0;
    if mag > 0.0 { (mag.to_bits(), atan2s(-p.y, p.x) as u16 as i64) } else { (mag.to_bits(), -1) }
}

fn dz(v: i8, lo: i32, hi: i32) -> i64 {
    let v = v as i32;
    if v >= lo && v <= hi { 0 } else { v as i64 }
}

/// The key of one stick atom (indices as the analysis' ATOMS): equal keys = indistinguishable to
/// the code that reads that atom.
fn atom_key(atom: usize, x: i8, y: i8) -> i64 {
    let p = process(x, y);
    match atom {
        0 => (intended(p).0 != 0) as i64,
        1 => intended(p).0 as i64,
        2 => intended(p).1,
        3 => p.x.to_bits() as i64,
        4 => p.y.to_bits() as i64,
        5 => p.mag.to_bits() as i64,
        6 => x as i64,
        7 => y as i64,
        8 => dz(x, -1, 1) / 8,
        9 => dz(y, -1, 1) / 8,
        10 => (x > 60) as i64 | (((x < -60) as i64) << 1),
        11 => (y > 60) as i64 | (((y < -60) as i64) << 1),
        12 => dz(x, -9, 9),
        13 => (p.mag < 30.0) as i64 | (((p.mag > 40.0) as i64) << 1) | (((p.mag > 48.0) as i64) << 2),
        14 => (-16.0f32 < p.y) as i64,
        _ => unreachable!(),
    }
}

fn from_idx(i: usize) -> (i8, i8) {
    ((i >> 8) as u8 as i8, i as u8 as i8)
}

/// The classes of the common refinement of the atoms in `mask`, with the most central member as
/// the representative (smallest |x|+|y|, then max(|x|,|y|), |x|, |y|, then the index).
pub struct Classes {
    class_of: Vec<u32>,
    rep: Vec<u32>,
}

impl Classes {
    fn build(mask: u32) -> Classes {
        let atoms: Vec<usize> = (0..15).filter(|i| mask >> i & 1 == 1).collect();
        let rep_order = |i: usize| {
            let (x, y) = from_idx(i);
            let (ax, ay) = ((x as i32).unsigned_abs(), (y as i32).unsigned_abs());
            (ax + ay, ax.max(ay), ax, ay, i)
        };
        let mut order: Vec<usize> = (0..65536).collect();
        order.sort_by_key(|&i| rep_order(i));
        let mut ids: HashMap<Vec<i64>, u32> = HashMap::new();
        let mut class_of = vec![0u32; 65536];
        let mut rep = vec![];
        for i in order {
            let (x, y) = from_idx(i);
            let key: Vec<i64> = atoms.iter().map(|&a| atom_key(a, x, y)).collect();
            let n = rep.len() as u32;
            let id = *ids.entry(key).or_insert_with(|| {
                rep.push(i as u32);
                n
            });
            class_of[i] = id;
        }
        Classes { class_of, rep }
    }

    pub fn count(&self) -> usize {
        self.rep.len()
    }

    /// The representative of the class of the raw stick `(x, y)`.
    pub fn rep_of(&self, x: i8, y: i8) -> (i8, i8) {
        from_idx(self.rep[self.class_of[((x as u8 as usize) << 8) | y as u8 as usize] as usize] as usize)
    }
}

/// The classes of the atoms in `mask`, built on first use (tens of ms) and shared.
pub fn classes(mask: u32) -> Arc<Classes> {
    static CACHE: OnceLock<Mutex<HashMap<u32, Arc<Classes>>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap();
    c.entry(mask).or_insert_with(|| Arc::new(Classes::build(mask))).clone()
}

// ---------------------------------------------------------------------------------------------
// What an action consumes.

/// What a frame consumes of the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Needs {
    pub atoms: u32,
    pub down: u16,
    pub pressed: u16,
    pub fsa: bool,
    pub fsb: bool,
}

impl Needs {
    /// Everything (an action the model doesn't know).
    pub const ALL: Needs = Needs { atoms: ALL_ATOMS, down: 0xFFFF, pressed: 0xFFFF, fsa: true, fsb: true };
}

/// The model's row for Mario's action `a` at the start of a frame: what the frame consumes.
/// Unknown actions, and 0 (before Mario exists: the title and the menus), consume everything.
/// Cutscene-group actions (dialogs, signs) also consume what the pause/dialog menu row does (the
/// raw stick to scroll, A and B to advance), since the model attributes those frames to the menu,
/// not to the action.
pub fn needs_of(action: u32) -> Needs {
    if action == 0 {
        return Needs::ALL;
    }
    let Ok(i) = ROWS.binary_search_by_key(&action, |r| r.0) else { return Needs::ALL };
    let r = ROWS[i];
    let mut n = Needs { atoms: r.1, down: r.2, pressed: r.3, fsa: r.4, fsb: r.5 };
    if action & 0x1C0 == 0x100 {
        n.atoms |= AT_SCRX | AT_SCRY;
        n.down |= 0x0F1F;
        n.pressed |= 0xD01F; // A, B, Start (advance and close a dialog)
    }
    n
}

// ---------------------------------------------------------------------------------------------
// The canonicaliser.

/// Chooses new values for one button bit over time. `d` the original level, `nd[t]`: the level at
/// `t` is consumed, `np[t]`: the edge (down at t, not at t-1) at `t` is consumed; `prev0` the level
/// before the first frame. Exact DP: fewest set bits, then fewest transitions.
pub fn solve_bit(d: &[bool], nd: &[bool], np: &[bool], prev0: bool) -> Vec<bool> {
    let n = d.len();
    let inf = u64::MAX / 4;
    let mut cost = vec![[inf; 2]; n];
    let mut from = vec![[0u8; 2]; n];
    for t in 0..n {
        for s in 0..2usize {
            if nd[t] && (s == 1) != d[t] {
                continue;
            }
            let orig_prev = if t == 0 { prev0 } else { d[t - 1] };
            let orig_edge = d[t] && !orig_prev;
            for p in 0..2usize {
                if t == 0 && (p == 1) != prev0 {
                    continue;
                }
                if np[t] && ((s == 1 && p == 0) != orig_edge) {
                    continue;
                }
                let base = if t == 0 { 0 } else { cost[t - 1][p] };
                if base >= inf {
                    continue;
                }
                let flip = if (t == 0 && (s == 1) != prev0) || (t > 0 && s != p) { 1 } else { 0 };
                let c = base + if s == 1 { 1000 } else { 0 } + flip;
                if c < cost[t][s] {
                    cost[t][s] = c;
                    from[t][s] = p as u8;
                }
            }
        }
    }
    let mut s = if cost[n - 1][0] <= cost[n - 1][1] { 0 } else { 1 };
    let mut out = vec![false; n];
    for t in (0..n).rev() {
        out[t] = s == 1;
        s = from[t][s] as usize;
    }
    out
}

/// What [`canonicalise`] changed.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub frames: usize,
    /// frames left alone (poked, first, tail, unknown action)
    pub pinned: usize,
    pub sticks_changed: usize,
    pub sticks_neutral: usize,
    pub button_bits_cleared: usize,
    pub button_bits_set: usize,
}

/// What the exact rules allow for a run: per frame the stick atoms that must be kept (0: any stick,
/// [`ALL_ATOMS`]: leave it), the new buttons, and which frames are pinned.
pub struct Plan {
    pub atoms: Vec<u32>,
    pub buttons: Vec<u16>,
    pub fixed: Vec<bool>,
    pub stats: Stats,
}

impl Plan {
    /// Whether sticks `a` and `b` are interchangeable on frame `i`.
    pub fn same_class(&self, i: usize, a: (i8, i8), b: (i8, i8)) -> bool {
        let m = self.atoms[i];
        if self.fixed[i] || m == ALL_ATOMS {
            return a == b;
        }
        m == 0 || classes(m).rep_of(a.0, a.1) == classes(m).rep_of(b.0, b.1)
    }
}

/// The exact rules for the run `pads`. `actions[i]` is Mario's action at the start of frame `i`;
/// `poked[i]` whether the perturbation played frame `i`'s pad (those are left alone). No game.
pub fn plan(pads: &[GamePad], actions: &[u32], poked: &[bool]) -> Plan {
    let n = pads.len();
    let mut st = Stats { frames: n, ..Default::default() };
    if n == 0 {
        return Plan { atoms: vec![], buttons: vec![], fixed: vec![], stats: st };
    }
    let fixed: Vec<bool> = (0..n).map(|i| poked[i] || i == 0 || i + PIN_TAIL >= n).collect();
    let mut needs: Vec<Needs> = actions.iter().map(|&a| needs_of(a)).collect();
    // The pause menu (Start) reads the raw stick to scroll: after the first Start press
    // nothing is changed (a miner's run has none).
    let mut started = false;
    for i in 0..n {
        started |= pads[i].button & BTN_START != 0;
        if started {
            needs[i] = Needs::ALL;
        }
    }
    for i in 0..n {
        if fixed[i] {
            st.pinned += 1;
            needs[i] = Needs::ALL;
        }
    }
    // Buttons: per bit, the exact DP. framesSinceA/B readers keep the presses in [t, next press).
    let latent = |mask: u16, read: &dyn Fn(usize) -> bool| -> Vec<bool> {
        let mut r = vec![false; n];
        for t in (0..n).rev() {
            let next_pressed = t + 1 < n && pads[t + 1].button & mask != 0 && pads[t].button & mask == 0;
            r[t] = read(t) || (t + 1 < n && !next_pressed && r[t + 1]);
        }
        r
    };
    let lat_a = latent(BTN_A, &|t| needs[t].fsa);
    let lat_b = latent(BTN_B, &|t| needs[t].fsb);
    let mut btn = vec![0u16; n];
    for bit in 0..16 {
        let m = 1u16 << bit;
        if m & VALID_BUTTONS == 0 {
            continue;
        }
        let d: Vec<bool> = pads.iter().map(|p| p.button & m != 0).collect();
        let nd: Vec<bool> = (0..n).map(|t| fixed[t] || needs[t].down & m != 0).collect();
        let mut np: Vec<bool> = (0..n).map(|t| fixed[t] || needs[t].pressed & m != 0).collect();
        for t in 0..n {
            if (m == BTN_A && lat_a[t]) || (m == BTN_B && lat_b[t]) {
                np[t] = true;
            }
        }
        let s = solve_bit(&d, &nd, &np, false);
        for t in 0..n {
            if s[t] {
                btn[t] |= m;
            }
        }
    }
    for t in 0..n {
        if fixed[t] {
            btn[t] = pads[t].button;
            continue;
        }
        st.button_bits_cleared += (pads[t].button & !btn[t]).count_ones() as usize;
        st.button_bits_set += (btn[t] & !pads[t].button).count_ones() as usize;
    }
    Plan { atoms: needs.iter().map(|x| x.atoms).collect(), buttons: btn, fixed, stats: st }
}

/// The run `pads` with every stick replaced by its class representative (neutral where the action
/// reads none) and unconsumed button levels/edges cleared. Fully deterministic, no game involved.
pub fn canonicalise(pads: &[GamePad], actions: &[u32], poked: &[bool]) -> (Vec<GamePad>, Stats) {
    let mut pl = plan(pads, actions, poked);
    let mut out = pads.to_vec();
    for i in 0..pads.len() {
        out[i].button = pl.buttons[i];
        if pl.fixed[i] || pl.atoms[i] == ALL_ATOMS {
            continue;
        }
        let (x, y) = (pads[i].stick_x, pads[i].stick_y);
        let (nx, ny) = if pl.atoms[i] == 0 { (0, 0) } else { classes(pl.atoms[i]).rep_of(x, y) };
        if (nx, ny) != (x, y) {
            pl.stats.sticks_changed += 1;
            if pl.atoms[i] == 0 {
                pl.stats.sticks_neutral += 1;
            }
        }
        out[i].stick_x = nx;
        out[i].stick_y = ny;
    }
    (out, pl.stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_counts() {
        // docs/INPUTS.md 2.2: the processed stick and Mario's (intendedMag, intendedYaw) pair.
        assert_eq!(classes(AT_SX | AT_SY | AT_SMAG).count(), 51_549);
        assert_eq!(classes(AT_IMAG | AT_IYAW).count(), 20_129);
        assert_eq!(classes(0).count(), 1);
        assert_eq!(classes(AT_IMAG | AT_IYAW).rep_of(5, 3), (0, 0));
        assert_eq!(classes(AT_IMAG | AT_IYAW).rep_of(127, 127), (52, 52));
        assert_eq!(classes(AT_IMAG | AT_IYAW).rep_of(0, -100), (0, -70));
    }

    #[test]
    fn solve_bit_keeps_what_is_consumed() {
        let d = [false, true, true, false, true, false];
        // nothing consumed: all clear
        assert_eq!(solve_bit(&d, &[false; 6], &[false; 6], false), vec![false; 6]);
        // levels consumed at 1 and 4
        let nd = [false, true, false, false, true, false];
        let s = solve_bit(&d, &nd, &[false; 6], false);
        assert!(s[1] && s[4]);
        // the edge at 4 is consumed: level 3 must be low and 4 high
        let np = [false, false, false, false, true, false];
        let s = solve_bit(&d, &[false; 6], &np, false);
        assert!(s[4] && !s[3]);
    }
}
