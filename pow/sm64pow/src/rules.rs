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

pub use sm64coin_perturb::sm64::Progress;

/// The perturbation's tuning (was sm64-binds' `RngConfig`, which the stock
/// game build no longer carries).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RngConfig {
    pub window_length: u32,
    pub random_amount: u32,
    pub random_burst_length: u32,
    pub a_prob: f32,
    pub b_prob: f32,
    pub z_prob: f32,
}

impl RngConfig {
    pub const fn new(
        window_length: u32,
        random_amount: u32,
        random_burst_length: u32,
        a_prob: f32,
        b_prob: f32,
        z_prob: f32,
    ) -> Self {
        RngConfig {
            window_length,
            random_amount,
            random_burst_length,
            a_prob,
            b_prob,
            z_prob,
        }
    }
}

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
    fn name(&self) -> &'static str;
    /// One line for humans.
    fn goal(&self) -> &'static str;
    /// The task at (effective) `difficulty`: the time limit and the pokes.
    fn params(&self, difficulty: u64) -> Params;
    /// The most frames any difficulty allows (bounds decoding).
    fn max_frames_any(&self) -> u32 {
        self.params(1).max_frames
    }
    /// Whether `now` (the state after a frame) completes a block that started
    /// at `start`: the success condition. It must first hold on a run's last
    /// frame.
    fn is_success(&self, start: &Progress, now: &Progress) -> bool;
    /// Whether a block ending at `end` ends the cycle: the next block starts
    /// from power-on.
    fn ends_cycle(&self, end: &Progress) -> bool;
    /// After this many seconds without a block, a block may (instead of
    /// continuing from its parent's end state) start from power-on: the
    /// escape from a state nobody can make progress from.
    fn stall_seconds(&self) -> u64;
    /// Seconds between blocks the retarget aims for.
    fn target_seconds(&self) -> u64;
    /// The next block's difficulty from the recent blocks' timestamps and
    /// cumulative difficulties (oldest first).
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64;
    /// The difficulty a block actually needs, given how long after its parent
    /// it is timestamped (`retarget` is [`Rules::next_difficulty`]): what
    /// counts as the block's work (chain weight).
    fn effective_difficulty(&self, difficulty: u64, elapsed_seconds: u64) -> u64;
    /// The time limit of a block at retarget `difficulty` timestamped
    /// `elapsed_seconds` after its parent. Defaults to the limit at the
    /// effective difficulty; rules may make it continuous in time.
    fn max_frames(&self, difficulty: u64, elapsed_seconds: u64) -> u32 {
        self.params(self.effective_difficulty(difficulty, elapsed_seconds)).max_frames
    }
}

/// The rules this build runs.
pub fn active() -> &'static dyn Rules {
    &STAR_CYCLE
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
/// The 70-star cycle: each block is the game's next bit of progress (a
/// star, a Bowser key, or the Grand Star after 70 stars), played on from the
/// previous block's end state; the Grand Star ends the cycle and the next
/// block starts from power-on. Difficulty is only the time limit; the pokes
/// are the same at every difficulty, so a block's run never changes meaning
/// while it is mined (decay only gives it more time).
pub struct StarCycle {
    /// Time limit at difficulty 1 (frames, 30 per second).
    pub base_frames: u32,
    /// The shortest time limit any difficulty sets.
    pub min_frames: u32,
    /// Each difficulty level (1/8 of a doubling) multiplies the limit by this / 100.
    pub shrink_percent: u64,
    /// Pokes: bursts per window, window length (game-clock ticks), burst length.
    pub random: u32,
    pub window: u32,
    pub burst: u32,
    pub target: u64,
    /// Retarget window and outliers cut at each end (Monero's algorithm).
    pub retarget_window: usize,
    pub retarget_cut: usize,
    /// Decay, independent of the difficulty: how much real time miners get.
    /// For this many seconds after the parent the limit stays put...
    pub decay_grace: u64,
    /// ...then every this many seconds it relaxes by one halving of the
    /// difficulty (continuously, a little every second).
    pub decay_halving: u64,
    pub stall: u64,
}

pub const STAR_CYCLE: StarCycle = StarCycle {
    base_frames: 10 * 60 * 30,
    min_frames: 20 * 30,
    shrink_percent: 98,
    random: 5,
    window: 100,
    burst: 2,
    target: 300,
    retarget_window: 60,
    retarget_cut: 6,
    decay_grace: 600,
    decay_halving: 300,
    stall: 3600,
};

impl Rules for StarCycle {
    fn name(&self) -> &'static str {
        "star-cycle-70"
    }
    fn goal(&self) -> &'static str {
        "Make the game's next progress (a star, a Bowser key, or the Grand Star after 70 stars) from the previous block's end state, while the controller is randomly poked. The Grand Star ends the cycle; the next block starts from power-on."
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
            rng: RngConfig::new(self.window, self.random, self.burst, 0.5, 0.5, 0.2),
            reseed_frames: 0,
        }
    }
    fn is_success(&self, start: &Progress, now: &Progress) -> bool {
        now.points() > start.points()
    }
    fn ends_cycle(&self, end: &Progress) -> bool {
        end.grand_star
    }
    fn stall_seconds(&self) -> u64 {
        self.stall
    }
    fn target_seconds(&self) -> u64 {
        self.target
    }
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64 {
        monero_retarget(timestamps, cumulative, self.target, self.retarget_window, self.retarget_cut)
    }
    fn effective_difficulty(&self, difficulty: u64, elapsed: u64) -> u64 {
        let grace = self.decay_grace;
        if elapsed <= grace {
            return difficulty.max(1);
        }
        let halvings = (elapsed - grace) / self.decay_halving.max(1);
        if halvings >= 64 { 1 } else { (difficulty >> halvings).max(1) }
    }
    /// Continuous in time: after the grace period the limit grows every
    /// second (one difficulty halving, 8 levels, per target), so runs waiting
    /// for the limit become valid one by one, the shortest first. Integer
    /// arithmetic: every node computes the same limit.
    fn max_frames(&self, difficulty: u64, elapsed: u64) -> u32 {
        let t = self.decay_halving.max(1); // level units per level: one level is t/8 seconds of decay
        let grace = self.decay_grace;
        let decay = elapsed.saturating_sub(grace).saturating_mul(8); // in 1/t levels
        let level = (level8(difficulty) as u64 * t).saturating_sub(decay); // in 1/t levels
        let (whole, frac) = (level / t, level % t);
        let mut frames = self.base_frames as u64;
        for _ in 0..whole {
            frames = frames * self.shrink_percent / 100;
            if frames <= self.min_frames as u64 {
                return self.min_frames;
            }
        }
        // The fraction of one more level, linearly.
        let step = 100 - self.shrink_percent; // percent per level
        frames = frames * (100 * t - step * frac) / (100 * t);
        (frames as u32).max(self.min_frames)
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
    fn only_the_time_limit_gets_harder() {
        let r = &STAR_CYCLE;
        let mut last = r.params(1);
        assert_eq!(last.max_frames, 18000);
        assert_eq!(last.rng.random_amount, 5);
        for d in 2..5000u64 {
            let p = r.params(d);
            assert!(p.max_frames <= last.max_frames);
            assert_eq!(p.rng, last.rng, "pokes don't depend on difficulty");
            last = p;
        }
        assert_eq!(r.params(u64::MAX).max_frames, 600);
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
        let r = &STAR_CYCLE;
        assert_eq!(r.effective_difficulty(64, 0), 64);
        assert_eq!(r.effective_difficulty(64, 600), 64);
        assert_eq!(r.effective_difficulty(64, 900), 32);
        assert_eq!(r.effective_difficulty(64, 1200), 16);
        assert_eq!(r.effective_difficulty(64, 1_000_000), 1);
        assert_eq!(r.effective_difficulty(0, 0), 1);
    }

    #[test]
    fn time_limit_grows_every_second_after_the_grace() {
        let r = &STAR_CYCLE;
        let d = 1 << 10; // level 80
        assert_eq!(r.max_frames(d, 0), r.params(d).max_frames, "no decay: the plain limit");
        assert_eq!(r.max_frames(d, 600), r.max_frames(d, 0), "grace");
        let mut last = r.max_frames(d, 600);
        let mut grew = 0;
        for s in 601..600 + 300 * 10 {
            let m = r.max_frames(d, s);
            assert!(m >= last, "never shrinks ({s} s)");
            grew += (m > last) as u32;
            last = m;
        }
        assert!(grew > 2000, "grows (almost) every second: {grew}");
        // One target of decay is one halving of the difficulty.
        assert_eq!(r.max_frames(d, 900), r.params(d / 2).max_frames);
        assert_eq!(r.max_frames(d, 10_000_000), r.params(1).max_frames);
        assert_eq!(r.max_frames(u64::MAX, 0), r.min_frames);
    }

    #[test]
    fn success_is_one_more_point_and_the_grand_star_ends_the_cycle() {
        let r = &STAR_CYCLE;
        let s = Progress { stars: 12, keys: 1, grand_star: false };
        assert!(!r.is_success(&s, &s));
        assert!(r.is_success(&s, &Progress { stars: 13, ..s }));
        assert!(r.is_success(&s, &Progress { keys: 2, ..s }));
        assert!(!r.ends_cycle(&s));
        assert!(r.ends_cycle(&Progress { stars: 70, keys: 2, grand_star: true }));
    }
}
