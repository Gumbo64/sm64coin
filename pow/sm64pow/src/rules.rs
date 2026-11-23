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
    /// Simulated frames between mixes of the game state into the timing
    /// (when pokes land) and pad (what they press) streams; 0: never.
    pub timing_mix: u32,
    pub pad_mix: u32,
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
            timing_mix: window_length,
            pad_mix: window_length,
        }
    }

    /// The same with these mix intervals (see [`RngConfig::timing_mix`]).
    pub const fn with_mix(self, timing_mix: u32, pad_mix: u32) -> Self {
        RngConfig { timing_mix, pad_mix, ..self }
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
    /// Whether a block may start from power-on instead of its parent's end
    /// state (the escape from a state nobody can make progress from yet), and
    /// at what price: a restart is judged against the limit as it stood this
    /// many seconds earlier, so it is valid only `delay` seconds after the
    /// parent and then with the limit of `gap - delay`. That bounds the time
    /// any one star can hold the chain up, at every difficulty (a frame cost
    /// would cost nothing once the limit is at its maximum). `parent` is the
    /// parent's end progress. None: not allowed. Not asked when the parent
    /// ends the cycle (then power-on is the only start, at no cost).
    fn restart_delay(&self, parent: &Progress) -> Option<u64> {
        let _ = parent;
        None
    }
    /// Fork choice by the number of blocks (every block weighs the same) in
    /// place of the cumulative difficulty; ties keep the chain seen first.
    /// The difficulty here is only a time limit, and a run of L frames fits
    /// every difficulty whose limit is at least L at the gap the miner
    /// declares: weighing blocks by difficulty would let a private chain
    /// stamped as fast as its runs allow pump its weight (time warp). The
    /// retarget still reads the cumulative difficulty.
    fn weigh_by_blocks(&self) -> bool {
        false
    }
    /// No block but the genesis block may be timestamped before this (unix
    /// seconds): without a floor a fork from the genesis block (timestamp 0)
    /// could declare years of time it never spent (restarts, low limits).
    fn launch_timestamp(&self) -> u64 {
        0
    }
    /// Seconds between blocks the retarget aims for.
    fn target_seconds(&self) -> u64;
    /// The next block's difficulty from the recent blocks' timestamps and
    /// cumulative difficulties (oldest first).
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64;
    /// What a block at retarget `difficulty` adds to the retarget's cumulative
    /// difficulty (and, unless [`Rules::weigh_by_blocks`], to the chain
    /// weight), given how long after its parent it is timestamped. Rules
    /// whose weight reads the timestamp let anyone who restamps a block
    /// change its weight; [`StarCycle`] does not.
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

/// The limit in minutes of play at a few difficulties, by seconds since the
/// parent's timestamp (20 min at most, 20 s at least; it starts
/// `start_halvings` = 20 halvings below the difficulty's and grows one halving
/// a minute; 5 bursts of 2 poked frames per 100 at every difficulty):
///
/// | difficulty | +0 s | +5 min | +10 min | +20 min | +30 min | +40 min |
/// |---|---|---|---|---|---|---|
/// | 1 | 0.8 | 1.8 | 4.0 | 20.0 | 20.0 | 20.0 |
/// | 2^8 | 0.3 | 0.5 | 1.1 | 5.5 | 20.0 | 20.0 |
/// | 2^16 | 0.3 | 0.3 | 0.3 | 1.5 | 7.6 | 20.0 |
/// | 2^24 | 0.3 | 0.3 | 0.3 | 0.4 | 2.1 | 10.5 |
///
/// The 70-star cycle: each block is the game's next bit of progress (a
/// star, a Bowser key, or the Grand Star after 70 stars), played on from the
/// previous block's end state; the Grand Star ends the cycle and the next
/// block starts from power-on. Difficulty is only the time limit; the pokes
/// are the same at every difficulty, so a block's run never changes meaning
/// while it is mined (the limit only grows).
///
/// The frame-length auction: the limit starts at the retarget difficulty's
/// limit the moment the parent is timestamped and grows every second (no
/// grace), so the shortest run anyone holds becomes valid first. A block's
/// weight in fork choice is one block (the chain with more blocks wins,
/// the first seen on a tie), whatever its difficulty or timestamp; the
/// retarget still reads the difficulties. A power-on
/// restart is judged against the limit `restart_base_seconds` plus
/// `restart_point_seconds` per progress point thrown away (capped) earlier:
/// the escape for when no miner can do the next star yet, an upper bound on
/// how long one star can hold the chain up. Set `restart_base_seconds` to
/// `u64::MAX` to turn restarts off. Modelled in pow/tools/auction.
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
    /// Simulated frames between mixes of the game state into the poke
    /// timing and pad streams (0: never). Every frame: a compute-normalised
    /// comparison (pow/perturb/DESIGN.md) found that with per-frame mixing
    /// honest search is already the best use of compute (a dedicated
    /// reroller gains nothing, the GA's own variation rerolls), while
    /// per-window mixing let targeted rerolls beat honest search by ~250
    /// frames and per-game let nonce screening against a route's harm map
    /// erase the pokes (~414 frames) for ~1% of a run's compute.
    pub timing_mix: u32,
    pub pad_mix: u32,
    pub target: u64,
    /// Retarget window and outliers cut at each end (Monero's algorithm).
    pub retarget_window: usize,
    pub retarget_cut: usize,
    /// Decay, independent of the difficulty: how much real time miners get.
    /// For this many seconds after the parent the limit stays put (0: the
    /// auction, it grows from the start)...
    pub decay_grace: u64,
    /// ...then every this many seconds it relaxes by one halving of the
    /// difficulty (continuously, a little every second). The slope: steep
    /// rewards the shortest run near the block time and keeps block times
    /// tight; shallow behaves like a fixed threshold (first run in wins).
    pub decay_halving: u64,
    /// The limit starts this many difficulty halvings below the retarget's
    /// (and grows back at one per `decay_halving` seconds), so even at the
    /// lowest difficulty a run waits for the limit and the shortest run is
    /// valid first: the auction works when miners are slower than the target
    /// (the retarget then sits at 1, whose plain limit is the cap). 20: at
    /// difficulty 1 the limit starts at about 47 s of play and reaches the
    /// 20-minute cap 20 minutes after the parent.
    pub start_halvings: u64,
    /// A power-on restart is judged against the limit this many seconds
    /// earlier...
    pub restart_base_seconds: u64,
    /// ...plus this many per progress point the restart throws away...
    pub restart_point_seconds: u64,
    /// ...at most this many in all.
    pub restart_max_seconds: u64,
    /// See [`Rules::launch_timestamp`]; set it to the new network's launch.
    pub launch_timestamp: u64,
}

pub const STAR_CYCLE: StarCycle = StarCycle {
    base_frames: 20 * 60 * 30, // 20 min: the longest star of a real 120-star route (t1) is 9.8 min, and pokes lengthen runs
    min_frames: 20 * 30,
    shrink_percent: 98,
    random: 5,
    window: 100,
    burst: 2,
    timing_mix: 1,
    pad_mix: 1,
    target: 300,
    retarget_window: 60,
    retarget_cut: 6,
    decay_grace: 0,
    decay_halving: 60,
    start_halvings: 20,
    restart_base_seconds: 3600,
    restart_point_seconds: 60,
    restart_max_seconds: 2 * 3600,
    launch_timestamp: 1_795_380_000, // 2026-11-22 (the frame-length auction's network)
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
            rng: RngConfig::new(self.window, self.random, self.burst, 0.5, 0.5, 0.2).with_mix(self.timing_mix, self.pad_mix),
            reseed_frames: 0,
        }
    }
    fn is_success(&self, start: &Progress, now: &Progress) -> bool {
        now.points() > start.points()
    }
    fn ends_cycle(&self, end: &Progress) -> bool {
        end.grand_star
    }
    fn weigh_by_blocks(&self) -> bool {
        true
    }
    fn launch_timestamp(&self) -> u64 {
        self.launch_timestamp
    }
    fn restart_delay(&self, parent: &Progress) -> Option<u64> {
        if self.restart_base_seconds == u64::MAX {
            return None;
        }
        let lost = (parent.points() as u64).saturating_mul(self.restart_point_seconds);
        Some(self.restart_base_seconds.saturating_add(lost).min(self.restart_max_seconds.max(self.restart_base_seconds)))
    }
    fn target_seconds(&self) -> u64 {
        self.target
    }
    fn next_difficulty(&self, timestamps: &[u64], cumulative: &[u128]) -> u64 {
        monero_retarget(timestamps, cumulative, self.target, self.retarget_window, self.retarget_cut)
    }
    /// The weight is the retarget difficulty: the timestamp only decides
    /// when a run fits the limit, never what the block is worth.
    fn effective_difficulty(&self, difficulty: u64, _elapsed: u64) -> u64 {
        difficulty.max(1)
    }
    /// Continuous in time: after the grace period the limit grows every
    /// second (one difficulty halving, 8 levels, per target), so runs waiting
    /// for the limit become valid one by one, the shortest first. Integer
    /// arithmetic: every node computes the same limit.
    fn max_frames(&self, difficulty: u64, elapsed: u64) -> u32 {
        let t = self.decay_halving.max(1); // level units per level: one level is t/8 seconds of decay
        let grace = self.decay_grace;
        let decay = elapsed.saturating_sub(grace).saturating_mul(8); // in 1/t levels
        let level = ((level8(difficulty) as u64 + 8 * self.start_halvings) * t).saturating_sub(decay); // in 1/t levels
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
        assert_eq!(last.max_frames, 36000);
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
    fn weight_is_the_retarget_difficulty_whatever_the_timestamp() {
        let r = &STAR_CYCLE;
        for e in [0, 1, 600, 900, 1_000_000] {
            assert_eq!(r.effective_difficulty(64, e), 64);
        }
        assert_eq!(r.effective_difficulty(0, 0), 1);
    }

    #[test]
    fn time_limit_grows_every_second_from_the_parent() {
        let r = &STAR_CYCLE;
        assert_eq!(r.decay_grace, 0, "the auction: no grace");
        let d = 1 << 10; // level 80
        let h = r.decay_halving;
        let k = r.start_halvings * h; // the start offset, in seconds of growth
        assert_eq!(r.max_frames(d, k), r.params(d).max_frames, "start_halvings later: the plain limit");
        assert!(r.max_frames(1, 0) < r.params(1).max_frames / 10, "even at difficulty 1 the limit starts low");
        assert_eq!(r.max_frames(1, k), r.params(1).max_frames, "and reaches the cap start_halvings later");
        let mut last = r.max_frames(d, k);
        let mut grew = 0;
        for s in k + 1..k + h * 10 {
            let m = r.max_frames(d, s);
            assert!(m >= last, "never shrinks ({s} s)");
            grew += (m > last) as u32;
            last = m;
        }
        assert!(grew as u64 > h * 10 * 9 / 10, "grows (almost) every second: {grew}");
        // One halving of decay is one halving of the difficulty.
        assert_eq!(r.max_frames(d, k + h), r.params(d / 2).max_frames);
        assert_eq!(r.max_frames(d, 10_000_000), r.params(1).max_frames);
        assert_eq!(r.max_frames(u64::MAX, 0), r.min_frames);
    }

    #[test]
    fn restart_waits_a_base_plus_the_progress_thrown_away() {
        let r = &STAR_CYCLE;
        let p = |stars| Progress { stars, keys: 0, grand_star: false, star_flags: [0; 26] };
        assert_eq!(r.restart_delay(&p(0)), Some(r.restart_base_seconds));
        assert_eq!(r.restart_delay(&p(10)), Some(r.restart_base_seconds + 10 * r.restart_point_seconds));
        assert_eq!(r.restart_delay(&p(69)), Some(r.restart_max_seconds), "capped");
        let off = StarCycle { restart_base_seconds: u64::MAX, ..STAR_CYCLE };
        assert_eq!(off.restart_delay(&p(3)), None, "restarts can be turned off");
    }

    #[test]
    fn success_is_one_more_point_and_the_grand_star_ends_the_cycle() {
        let r = &STAR_CYCLE;
        let s = Progress { stars: 12, keys: 1, ..Default::default() };
        assert!(!r.is_success(&s, &s));
        assert!(r.is_success(&s, &Progress { stars: 13, ..s }));
        assert!(r.is_success(&s, &Progress { keys: 2, ..s }));
        assert!(!r.ends_cycle(&s));
        assert!(r.ends_cycle(&Progress { stars: 70, keys: 2, grand_star: true, ..Default::default() }));
    }
}
