//! The controller perturbation ("pokes"), outside the game.
//!
//! Game-agnostic: it only needs, from the game, a *clock* (a counter that
//! moves when the game simulates and stands still otherwise: paused, menus)
//! and a *digest* of the game state (a few words), both supplied by a game
//! adapter (see [`sm64`]). The game build is stock; this state lives next to
//! it, and save states bundle the two (sm64pow's `SM64MiningGame`). This crate has
//! no dependencies, so the browser runs the very same code (perturb-wasm).
//!
//! * A 256-bit generator (xoshiro256**) seeded once from the whole block hash.
//! * Pokes are scheduled per tick of the game's clock: a frame can only be
//!   poked if the game simulated on the previous frame. While the clock stands
//!   still (pause menu, menus) nothing is drawn, so pausing neither spends
//!   pokes nor rerolls them. Implicit: no rule about pausing, just the clock.
//! * A poke is only spent on a frame the game simulated: if the clock didn't
//!   move on a poked frame (e.g. the frame after START, already in the pause
//!   menu), the poke is refunded and drawn again on the next simulated frame,
//!   so a menu can't absorb it. Still just the clock, no rule about menus.
//! * Two generators: one decides *when* pokes land (the timing stream), the
//!   other *what* they press (the pad stream), each seeded from the whole
//!   block hash (the pad stream under a domain tag). The digest of the game
//!   state is mixed into each stream once every `timing_mix` / `pad_mix`
//!   simulated frames (0: never, a pure function of the block hash). The
//!   consensus rules mix after every frame: freezing a window's (or the
//!   game's) schedule only turns dodging into a separate strategy for some
//!   miners (targeted rerolls at window starts, or screening nonces against
//!   a route's harm map), while with a per-frame mix honest search already
//!   rerolls as well as any dodger can (pow/perturb/DESIGN.md). The digest holds only
//!   state that stands still while the clock does, so pausing for longer
//!   doesn't reroll the pokes either.
//! * Within each window of `window_length` ticks, `random_amount` bursts of
//!   `random_burst_length` poked ticks, as in the original sm64-crypto.

use serde::{Deserialize, Serialize};

pub mod sm64;

pub const A_BUTTON: u16 = 0x8000;
pub const B_BUTTON: u16 = 0x4000;
pub const Z_TRIG: u16 = 0x2000;

/// A controller pad: buttons and stick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    pub button: u16,
    pub stick_x: i8,
    pub stick_y: i8,
}

/// How hard the controller is poked (from the consensus rules).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PokeConfig {
    pub window_length: u32,
    pub random_amount: u32,
    pub burst_length: u32,
    pub a_prob: f32,
    pub b_prob: f32,
    pub z_prob: f32,
    /// Simulated frames between mixes of the state digest into the timing
    /// stream (0: never).
    pub timing_mix: u32,
    /// The same for the pad stream.
    pub pad_mix: u32,
}

/// The 8 little-endian 32-bit words of a 32-byte seed hash.
pub fn seed_words(h: &[u8; 32]) -> [u32; 8] {
    let mut w = [0u32; 8];
    for (k, x) in w.iter_mut().enumerate() {
        *x = u32::from_le_bytes([h[4 * k], h[4 * k + 1], h[4 * k + 2], h[4 * k + 3]]);
    }
    w
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Perturber {
    s: [u64; 4],
    window_length: u32,
    random_amount: u32,
    burst_length: u32,
    a_prob: f32,
    b_prob: f32,
    z_prob: f32,
    cur_window: u32,
    cur_amount: u32,
    cur_burst: u32,
    last_clock: Option<u64>,
    /// The draw state before this frame's poke, to refund it if the game
    /// doesn't simulate the frame ([`Perturber::settle`]).
    #[serde(default)]
    undo: Option<Undo>,
    /// The pad stream (what pokes press); `s` is the timing stream.
    #[serde(default)]
    ps: [u64; 4],
    #[serde(default)]
    timing_mix: u32,
    #[serde(default)]
    pad_mix: u32,
    /// Simulated frames so far (when the streams are mixed).
    #[serde(default)]
    ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Undo {
    s: [u64; 4],
    ps: [u64; 4],
    cur_window: u32,
    cur_amount: u32,
    cur_burst: u32,
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Perturber {
    pub fn new(seed_hash: &[u8; 32], cfg: &PokeConfig) -> Perturber {
        let w = seed_words(seed_hash);
        let mut s = [
            w[0] as u64 | (w[1] as u64) << 32,
            w[2] as u64 | (w[3] as u64) << 32,
            w[4] as u64 | (w[5] as u64) << 32,
            w[6] as u64 | (w[7] as u64) << 32,
        ];
        if s == [0; 4] {
            s[0] = 1; // xoshiro can't leave the all-zero state
        }
        // The pad stream: the same seed words under a domain tag.
        let mut ps = [0u64; 4];
        for (k, x) in ps.iter_mut().enumerate() {
            *x = splitmix64(s[k] ^ 0x7061_645f_7374_7265 ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)); // "pad_stre"
        }
        if ps == [0; 4] {
            ps[0] = 1;
        }
        Perturber {
            s,
            ps,
            timing_mix: cfg.timing_mix,
            pad_mix: cfg.pad_mix,
            ticks: 0,
            window_length: cfg.window_length.max(1),
            random_amount: cfg.random_amount,
            burst_length: cfg.burst_length.max(1),
            a_prob: cfg.a_prob,
            b_prob: cfg.b_prob,
            z_prob: cfg.z_prob,
            cur_window: 0,
            cur_amount: 0,
            cur_burst: 0,
            last_clock: None,
            undo: None,
        }
    }

    fn next64(&mut self) -> u64 {
        xoshiro(&mut self.s)
    }

    /// Uniform in [0, 1) from the pad stream.
    fn pad_prob(&mut self) -> f64 {
        ((xoshiro(&mut self.ps) >> 32) as u32 >> 8) as f64 / (1u64 << 24) as f64
    }

    fn stick(&mut self) -> i8 {
        (((xoshiro(&mut self.ps) >> 32) as u32 % 161) as i32 - 80) as i8
    }
}

fn xoshiro(s: &mut [u64; 4]) -> u64 {
    let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    let t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = s[3].rotate_left(45);
    result
}

impl Perturber {
    fn next32(&mut self) -> u32 {
        (self.next64() >> 32) as u32
    }

    /// Uniform in [0, 1) from 24 bits (exact in f32 and f64, so every platform
    /// and the browser agree).
    fn prob(&mut self) -> f64 {
        (self.next32() >> 8) as f64 / (1u64 << 24) as f64
    }

    fn is_poke(&mut self) -> bool {
        if self.cur_burst > 0 {
            self.cur_burst -= 1;
            return true;
        }
        if self.cur_window == 0 {
            self.cur_window = self.window_length;
            self.cur_amount = self.random_amount;
        }
        let p = self.cur_amount as f64 / self.cur_window as f64;
        let poke = self.prob() < p;
        if poke {
            self.cur_amount -= 1;
            self.cur_burst = self.burst_length - 1;
        }
        self.cur_window -= 1;
        poke
    }

    /// The pad to play this frame, given the game's clock before the frame,
    /// and whether it was poked (then `wanted` is ignored).
    pub fn pad(&mut self, clock: u64, wanted: Pad) -> (Pad, bool) {
        let ticking = self.last_clock.is_some_and(|c| c != clock);
        self.last_clock = Some(clock);
        self.undo = None;
        if !ticking {
            return (wanted, false);
        }
        let undo = Undo { s: self.s, ps: self.ps, cur_window: self.cur_window, cur_amount: self.cur_amount, cur_burst: self.cur_burst };
        if !self.is_poke() {
            return (wanted, false);
        }
        self.undo = Some(undo);
        let mut b = 0u16;
        if self.pad_prob() < self.a_prob as f64 {
            b |= A_BUTTON;
        }
        if self.pad_prob() < self.b_prob as f64 {
            b |= B_BUTTON;
        }
        if self.pad_prob() < self.z_prob as f64 {
            b |= Z_TRIG;
        }
        let x = self.stick();
        let y = self.stick();
        (Pad { button: b, stick_x: x, stick_y: y }, true)
    }

    /// After the frame: `simulated` is whether the game's clock moved during
    /// it. Mixes the digest in when it did; refunds a poke when it didn't
    /// (the poke comes again on the next simulated frame).
    pub fn settle(&mut self, simulated: bool, digest: &[u64]) {
        let undo = self.undo.take();
        if simulated {
            self.ticks += 1;
            if self.timing_mix > 0 && self.ticks % self.timing_mix as u64 == 0 {
                mix_into(&mut self.s, digest);
            }
            if self.pad_mix > 0 && self.ticks % self.pad_mix as u64 == 0 {
                mix_into(&mut self.ps, digest);
            }
        } else if let Some(u) = undo {
            self.s = u.s;
            self.ps = u.ps;
            self.cur_window = u.cur_window;
            self.cur_amount = u.cur_amount;
            self.cur_burst = u.cur_burst;
        }
    }

    /// Folds a digest into both streams now, whatever the mix intervals
    /// (experiments; consensus goes through [`Perturber::settle`]).
    pub fn mix(&mut self, digest: &[u64]) {
        mix_into(&mut self.s, digest);
        mix_into(&mut self.ps, digest);
    }
}

fn mix_into(s: &mut [u64; 4], digest: &[u64]) {
    for &w in digest {
        s[0] ^= splitmix64(w ^ s[3]);
        xoshiro(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PokeConfig {
        PokeConfig { window_length: 100, random_amount: 5, burst_length: 2, a_prob: 0.5, b_prob: 0.5, z_prob: 0.2, timing_mix: 100, pad_mix: 100 }
    }

    #[test]
    fn standing_clock_means_no_pokes_and_no_draws() {
        let mut p = Perturber::new(&[7; 32], &cfg());
        let before = p.clone();
        for _ in 0..500 {
            assert_eq!(p.pad(42, Pad::default()), (Pad::default(), false));
        }
        assert_eq!(p.s, before.s, "nothing drawn while the clock stands still");
    }

    #[test]
    fn a_poke_on_a_frame_the_game_did_not_simulate_is_refunded() {
        // Clock 0, 1, 2, ... ticking; find the first poked frame.
        let mut p = Perturber::new(&[3; 32], &cfg());
        let mut t = 0u64;
        let first = loop {
            let snapshot = p.clone();
            let r = p.pad(t, Pad::default());
            if r.1 {
                break (t, snapshot, r.0);
            }
            p.settle(true, &[t]);
            t += 1;
        };
        let (t0, before, pad0) = first;
        // The game didn't simulate that frame (e.g. the pause menu opened):
        // the clock stands still for a while, then moves again.
        p.settle(false, &[999]);
        for _ in 0..10 {
            assert_eq!(p.pad(t0, Pad::default()), (Pad::default(), false), "no pokes while paused");
            p.settle(false, &[999]);
        }
        // The same poke comes back on the next simulated frame, as if the
        // paused frames never happened.
        let mut q = before.clone();
        q.last_clock = Some(t0 - 1);
        let mut p2 = p.clone();
        p2.last_clock = Some(t0 - 1);
        assert_eq!(p2.pad(t0, Pad::default()), (pad0, true));
        assert_eq!(q.pad(t0, Pad::default()), (pad0, true));
        assert_eq!((p2.s, p2.cur_burst, p2.cur_amount, p2.cur_window), (q.s, q.cur_burst, q.cur_amount, q.cur_window));
    }

    /// Plays `n` ticking frames, mixing digest `d(t)` after each, and returns
    /// the pokes (frame, pad).
    fn schedule(cfg: &PokeConfig, n: u64, d: impl Fn(u64) -> u64) -> Vec<(u64, Pad)> {
        let mut p = Perturber::new(&[5; 32], cfg);
        let mut out = vec![];
        for t in 0..n {
            let (pad, poked) = p.pad(t, Pad::default());
            if poked {
                out.push((t, pad));
            }
            p.settle(true, &[d(t)]);
        }
        out
    }

    #[test]
    fn a_window_cant_be_rerolled_from_inside_it() {
        let c = cfg(); // both streams mixed once per 100 simulated frames
        let base = schedule(&c, 400, |t| t);
        // A different state anywhere inside window 2 (frames 200..299, not on
        // its closing tick) leaves that window's pokes as they were...
        let nudged = schedule(&c, 400, |t| if (200..299).contains(&t) { t ^ 0xdead } else { t });
        let w = |s: &Vec<(u64, Pad)>, a, b| s.iter().filter(|x| (a..b).contains(&x.0)).cloned().collect::<Vec<_>>();
        assert_eq!(w(&base, 0, 300), w(&nudged, 0, 300));
        // ...but a different state on the tick that closes a window changes the next one.
        let at_close = schedule(&c, 400, |t| if t == 299 { t ^ 0xdead } else { t }); // frame 299 closes window 2 (the 300th simulated frame)
        assert_eq!(w(&base, 0, 300), w(&at_close, 0, 300));
        assert_ne!(w(&base, 300, 400), w(&at_close, 300, 400));
        // With a per-frame mix any nudge rerolls what follows.
        let per_frame = PokeConfig { timing_mix: 1, pad_mix: 1, ..c };
        assert_ne!(schedule(&per_frame, 400, |t| t), schedule(&per_frame, 400, |t| if t == 150 { 7 } else { t }));
        // Never mixed: the schedule is a function of the block hash alone.
        let never = PokeConfig { timing_mix: 0, pad_mix: 0, ..c };
        assert_eq!(schedule(&never, 400, |t| t), schedule(&never, 400, |t| t * 31 + 7));
    }

    #[test]
    fn timing_and_pads_are_separate_streams() {
        let c = cfg();
        // Pads mixed per frame, timing never: the same frames are poked, with other pads.
        let pads_only = PokeConfig { timing_mix: 0, pad_mix: 1, ..c };
        let (a, b) = (schedule(&pads_only, 400, |t| t), schedule(&pads_only, 400, |t| t + 1));
        assert_eq!(a.iter().map(|x| x.0).collect::<Vec<_>>(), b.iter().map(|x| x.0).collect::<Vec<_>>());
        assert_ne!(a, b);
    }

    #[test]
    fn pokes_follow_the_budget_per_window() {
        let mut p = Perturber::new(&[9; 32], &cfg());
        let mut poked = 0;
        for t in 0..1000u64 {
            if p.pad(t, Pad::default()).1 {
                poked += 1;
            }
            p.mix(&[t]);
        }
        // 5 bursts of 2 per 100 ticks.
        assert!((90..=110).contains(&poked), "{poked}");
    }

    #[test]
    fn seed_and_digest_both_matter() {
        let run = |seed: u8, salt: u64| {
            let mut p = Perturber::new(&[seed; 32], &cfg());
            (0..300u64).map(|t| { let r = p.pad(t, Pad::default()); p.mix(&[t ^ salt]); r }).collect::<Vec<_>>()
        };
        assert_eq!(run(1, 0), run(1, 0));
        assert_ne!(run(1, 0), run(2, 0));
        assert_ne!(run(1, 0), run(1, 5));
    }
}
