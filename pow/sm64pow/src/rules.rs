//! The consensus rules, in one place.
//!
//! Everything the daemon decides about proof-of-play goes through [`Rules`]:
//! the success condition, what a difficulty asks of the player, how
//! difficulty retargets and how it decays when the network is stuck. To change
//! the game, write another `impl Rules` and return it from [`active`]. The
//! daemon, the automated miner, the RPC and the explorer all pick it up; the
//! C++ side has no rules of its own.
//!
//! Changing the rules is a hard fork: every node must run the same ones.

use sm64_binds::{GameState, RngConfig, SM64Game};

/// What the network asks of a miner at some difficulty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Difficulty level in eighths of a doubling ([`level8`]).
    pub level8: u32,
    /// The success condition must hold on or before this frame.
    pub max_frames: u32,
    /// Controller perturbation.
    pub rng: RngConfig,
    /// The perturbation is reseeded from the next 32 bits of the block hash
    /// every this many frames (0: never; see `Seed`).
    pub reseed_frames: u32,
}

pub trait Rules: Sync {
    /// Shown by the RPC and the explorer.
    fn name(&self) -> &'static str;
    /// One line for humans: what you have to do.
    fn goal(&self) -> &'static str;

    /// What `difficulty` asks of the player.
    fn params(&self, difficulty: u64) -> Params;

    /// The longest any task can be (bounds proof decoding everywhere).
    fn max_frames_any(&self) -> u32 {
        self.params(1).max_frames
    }

    /// Checked after every frame, in order, on a fresh game. The first frame
    /// where it holds must be the solution's last.
    fn is_success(&self, game: &SM64Game, state: &GameState) -> bool;

    /// Wanted time between blocks, in seconds.
    fn target_seconds(&self) -> u64;

    /// The difficulty for the next block from the last blocks' timestamps and
    /// cumulative difficulties (oldest first, at most
    /// `DIFFICULTY_BLOCKS_COUNT` of them, as gathered by the daemon).
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64;

    /// The difficulty a block actually has to meet, from the retargeted one
    /// and the seconds between its timestamp and its parent's. Keeps the
    /// chain alive if nobody can finish the task.
    fn effective_difficulty(&self, difficulty: u64, elapsed_seconds: u64) -> u64;
}

/// The rules this build runs.
pub fn active() -> &'static dyn Rules {
    &ANY_STAR
}

/// About `8 * log2(d)` in integer arithmetic: 8 x the exponent plus the next
/// three mantissa bits (0 for `d <= 1`). Within 1.5 of the exact value.
pub fn level8(d: u64) -> u32 {
    if d <= 1 {
        return 0;
    }
    let msb = 63 - d.leading_zeros();
    let frac = if msb >= 3 { (d >> (msb - 3)) & 7 } else { (d << (3 - msb)) & 7 };
    msb * 8 + frac as u32
}

/// Monero's retarget (`next_difficulty` in `src/cryptonote_basic/difficulty.cpp`):
/// sort the window's timestamps, drop `cut` outliers from each end, and scale
/// the work done in the rest to `target` seconds.
pub fn monero_retarget(timestamps: &[u64], cumulative: &[u128], target: u64, window: usize, cut: usize) -> u64 {
    let n = timestamps.len().min(cumulative.len()).min(window);
    if n <= 1 {
        return 1;
    }
    let mut ts: Vec<u64> = timestamps[timestamps.len() - n..].to_vec();
    let cd = &cumulative[cumulative.len() - n..];
    ts.sort_unstable();
    let kept = window - 2 * cut;
    let (begin, end) = if n <= kept { (0, n) } else {
        let b = (n - kept + 1) / 2;
        (b, b + kept)
    };
    let span = (ts[end - 1] - ts[begin]).max(1) as u128;
    let work = cd[end - 1] - cd[begin];
    let d = (work * target as u128).div_ceil(span);
    d.clamp(1, u64::MAX as u128) as u64
}

/// The original sm64-crypto game: from power-on, get any star while the
/// controller is poked. Difficulty is hardness: each doubling cuts the time
/// limit by about 15% and adds one poke per 100 frames.
///
/// | difficulty | time limit | random frames / 100 |
/// |---|---|---|
/// | 1 | 10:00 | 10 (5 bursts of 2) |
/// | 4 | 7:13 | 14 |
/// | 16 | 5:13 | 18 |
/// | 64 | 3:46 | 22 |
/// | 256 | 2:43 | 26 |
pub struct AnyStar {
    pub base_frames: u32,
    pub min_frames: u32,
    /// Per eighth of a doubling, the limit is multiplied by this / 100.
    pub shrink_percent: u64,
    pub base_random: u32,
    pub max_random: u32,
    pub window: u32,
    pub burst: u32,
    pub target: u64,
    /// Blocks in the retarget window and outliers cut from each end.
    pub reseed_frames: u32,
    pub retarget_window: usize,
    pub retarget_cut: usize,
    /// No decay until this many targets have passed since the parent; then
    /// the difficulty halves every further target.
    pub decay_grace_targets: u64,
}

pub const ANY_STAR: AnyStar = AnyStar {
    base_frames: 10 * 60 * 30,
    min_frames: 60 * 30,
    shrink_percent: 98,
    base_random: 5,
    max_random: 40,
    window: 100,
    burst: 2,
    target: 300,
    reseed_frames: 0, // one 256-bit seed per run (the game's generator holds 256 bits)
    retarget_window: 60,
    retarget_cut: 6,
    decay_grace_targets: 2,
};

impl Rules for AnyStar {
    fn name(&self) -> &'static str {
        "any-star"
    }

    fn goal(&self) -> &'static str {
        "From power-on, collect any star while the controller is randomly poked."
    }

    fn params(&self, difficulty: u64) -> Params {
        let l8 = level8(difficulty);
        let mut frames = self.base_frames as u64;
        for _ in 0..l8 {
            frames = frames * self.shrink_percent / 100;
            if frames <= self.min_frames as u64 {
                break;
            }
        }
        Params {
            level8: l8,
            max_frames: (frames as u32).max(self.min_frames),
            rng: RngConfig::new(
                self.window,
                (self.base_random + l8 / 8).min(self.max_random),
                self.burst,
                0.5,
                0.5,
                0.2,
            ),
            reseed_frames: self.reseed_frames,
        }
    }

    fn is_success(&self, _game: &SM64Game, state: &GameState) -> bool {
        state.num_stars > 0
    }

    fn target_seconds(&self) -> u64 {
        self.target
    }

    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64 {
        monero_retarget(timestamps, cumulative, self.target, self.retarget_window, self.retarget_cut)
    }

    fn effective_difficulty(&self, difficulty: u64, elapsed: u64) -> u64 {
        let grace = self.decay_grace_targets * self.target;
        if elapsed <= grace {
            return difficulty.max(1);
        }
        let halvings = (elapsed - grace) / self.target;
        if halvings >= 64 { 1 } else { (difficulty >> halvings).max(1) }
    }
}

/// An example of another success condition: N stars instead of one (say,
/// for a longer task). Use it by returning it from [`active`].
pub struct Stars(pub i32);

impl Rules for Stars {
    fn name(&self) -> &'static str {
        "n-stars"
    }
    fn goal(&self) -> &'static str {
        "From power-on, collect N stars while the controller is randomly poked."
    }
    fn params(&self, difficulty: u64) -> Params {
        let mut p = ANY_STAR.params(difficulty);
        p.max_frames = p.max_frames.saturating_mul(self.0.max(1) as u32);
        p
    }
    fn is_success(&self, _game: &SM64Game, state: &GameState) -> bool {
        state.num_stars >= self.0
    }
    fn target_seconds(&self) -> u64 {
        ANY_STAR.target_seconds() * self.0.max(1) as u64
    }
    fn next_difficulty(&self, t: &[u64], c: &[u128]) -> u64 {
        monero_retarget(t, c, self.target_seconds(), 60, 6)
    }
    fn effective_difficulty(&self, d: u64, e: u64) -> u64 {
        ANY_STAR.effective_difficulty(d, e / self.0.max(1) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level8_matches_log2() {
        assert_eq!(level8(0), 0);
        assert_eq!(level8(1), 0);
        assert_eq!(level8(2), 8);
        assert_eq!(level8(3), 12);
        assert_eq!(level8(4), 16);
        assert_eq!(level8(256), 64);
        for d in [5u64, 77, 1000, 123456789, u64::MAX] {
            let exact = (d as f64).log2() * 8.0;
            assert!((level8(d) as f64 - exact).abs() < 1.5, "{d}");
        }
    }

    #[test]
    fn params_get_harder_monotonically() {
        let r = &ANY_STAR;
        let mut last = r.params(1);
        assert_eq!(last.max_frames, 18000);
        assert_eq!(last.rng.random_amount, 5);
        for d in 2..5000u64 {
            let p = r.params(d);
            assert!(p.max_frames <= last.max_frames);
            assert!(p.rng.random_amount >= last.rng.random_amount);
            last = p;
        }
        assert_eq!(r.params(u64::MAX).max_frames, 1800);
    }

    #[test]
    fn retarget_scales_work_to_target() {
        // Ten blocks at difficulty 10, one every 150 s: twice too fast.
        let ts: Vec<u64> = (0..10).map(|i| 1000 + i * 150).collect();
        let cd: Vec<u128> = (0..10).map(|i| 10 * (i + 1) as u128).collect();
        assert_eq!(monero_retarget(&ts, &cd, 300, 60, 6), 20);
        assert_eq!(monero_retarget(&ts[..1], &cd[..1], 300, 60, 6), 1);
    }

    #[test]
    fn decay_halves_after_grace() {
        let r = &ANY_STAR;
        assert_eq!(r.effective_difficulty(64, 0), 64);
        assert_eq!(r.effective_difficulty(64, 600), 64);
        assert_eq!(r.effective_difficulty(64, 900), 32);
        assert_eq!(r.effective_difficulty(64, 1200), 16);
        assert_eq!(r.effective_difficulty(64, 1_000_000), 1);
        assert_eq!(r.effective_difficulty(0, 0), 1);
    }
}
