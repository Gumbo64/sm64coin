//! The "reset cycle" policy: when a miner may and should start a block from
//! power-on instead of the parent's end state. Pure decisions, so every miner
//! (native automine, the web miner via wasm) shares them and they are unit-tested.
//!
//! The chain decides what is allowed (`sm64_start` of the template):
//!   "power-on": the block must start from power-on (new cycle), nothing to choose;
//!   "parent":   only the parent's end state; becomes "either" at `sm64_power_on_at`;
//!   "either":   the stall time passed, both starts are valid.

use std::str::FromStr;

/// Stars at which a parent's state has nothing left to give (a power-on restart is the only progress).
pub const ALL_STARS: u32 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    /// Never choose power-on (only when the chain forces it).
    Never,
    /// Search the parent first; power-on when that is stuck (no route, gave up, timed out, 120 stars).
    WhenStuck,
    /// Power-on whenever the chain allows it.
    Always,
}

impl FromStr for ResetMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "never" => Ok(Self::Never),
            "when-stuck" => Ok(Self::WhenStuck),
            "always-when-allowed" | "always" => Ok(Self::Always),
            _ => Err(format!("--reset: expected never|when-stuck|always-when-allowed, got {s:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    PowerOn,
    Parent,
}

/// Where the first attempt on this template starts, and why.
pub fn initial(mode: ResetMode, sm64_start: &str, parent_stars: u32) -> (Pick, &'static str) {
    match sm64_start {
        "power-on" => (Pick::PowerOn, "the chain requires the power-on start"),
        "either" => match mode {
            ResetMode::Always => (Pick::PowerOn, "reset allowed and --reset always-when-allowed"),
            ResetMode::WhenStuck if parent_stars >= ALL_STARS => (Pick::PowerOn, "the parent already has every star: nothing left to do from its state"),
            _ => (Pick::Parent, "parent first"),
        },
        _ => (Pick::Parent, "the chain allows only the parent's start"),
    }
}

/// After the parent-start search failed (no route / gave up / timed out): switch to power-on?
pub fn after_failure(mode: ResetMode, sm64_start: &str) -> bool {
    sm64_start == "either" && mode != ResetMode::Never
}

/// Should the miner restart on a fresh template when `sm64_power_on_at` passes (a "parent" template turns "either")?
pub fn restart_at_power_on_time(mode: ResetMode, sm64_start: &str, power_on_at: u64) -> Option<u64> {
    (sm64_start == "parent" && power_on_at != 0 && mode != ResetMode::Never).then_some(power_on_at)
}

/// Seconds until the power-on start becomes allowed (0: allowed or forced already).
pub fn seconds_until_allowed(sm64_start: &str, power_on_at: u64, now: u64) -> u64 {
    if sm64_start == "parent" { power_on_at.saturating_sub(now) } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResetMode::*;

    #[test]
    fn initial_picks() {
        assert_eq!(initial(Never, "power-on", 0).0, Pick::PowerOn);
        assert_eq!(initial(Never, "either", 0).0, Pick::Parent);
        assert_eq!(initial(WhenStuck, "either", 5).0, Pick::Parent);
        assert_eq!(initial(WhenStuck, "either", 120).0, Pick::PowerOn);
        assert_eq!(initial(Never, "either", 120).0, Pick::Parent);
        assert_eq!(initial(Always, "either", 5).0, Pick::PowerOn);
        assert_eq!(initial(Always, "parent", 5).0, Pick::Parent);
    }

    #[test]
    fn failure_and_wake() {
        assert!(after_failure(WhenStuck, "either"));
        assert!(after_failure(Always, "either"));
        assert!(!after_failure(Never, "either"));
        assert!(!after_failure(WhenStuck, "parent"));
        assert_eq!(restart_at_power_on_time(WhenStuck, "parent", 100), Some(100));
        assert_eq!(restart_at_power_on_time(Never, "parent", 100), None);
        assert_eq!(restart_at_power_on_time(WhenStuck, "either", 100), None);
        assert_eq!(seconds_until_allowed("parent", 100, 40), 60);
        assert_eq!(seconds_until_allowed("either", 100, 40), 0);
    }

    #[test]
    fn parse() {
        assert_eq!("when-stuck".parse::<ResetMode>(), Ok(WhenStuck));
        assert_eq!("always-when-allowed".parse::<ResetMode>(), Ok(Always));
        assert!("x".parse::<ResetMode>().is_err());
    }
}
