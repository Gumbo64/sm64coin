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
//! * After every frame on which the game simulated, the digest of its state
//!   is mixed in, so pokes can't be predicted without playing the run.
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
        Perturber {
            s,
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
        }
    }

    fn next64(&mut self) -> u64 {
        let s = &mut self.s;
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

    fn next32(&mut self) -> u32 {
        (self.next64() >> 32) as u32
    }

    /// Uniform in [0, 1) from 24 bits (exact in f32 and f64, so every platform
    /// and the browser agree).
    fn prob(&mut self) -> f64 {
        (self.next32() >> 8) as f64 / (1u64 << 24) as f64
    }

    fn stick(&mut self) -> i8 {
        ((self.next32() % 161) as i32 - 80) as i8
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
        if !ticking || !self.is_poke() {
            return (wanted, false);
        }
        let mut b = 0u16;
        if self.prob() < self.a_prob as f64 {
            b |= A_BUTTON;
        }
        if self.prob() < self.b_prob as f64 {
            b |= B_BUTTON;
        }
        if self.prob() < self.z_prob as f64 {
            b |= Z_TRIG;
        }
        let x = self.stick();
        let y = self.stick();
        (Pad { button: b, stick_x: x, stick_y: y }, true)
    }

    /// Folds the game state's digest in; call after a frame on which the game
    /// simulated (its clock moved).
    pub fn mix(&mut self, digest: &[u64]) {
        for &w in digest {
            self.s[0] ^= splitmix64(w ^ self.s[3]);
            self.next64();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PokeConfig {
        PokeConfig { window_length: 100, random_amount: 5, burst_length: 2, a_prob: 0.5, b_prob: 0.5, z_prob: 0.2 }
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
