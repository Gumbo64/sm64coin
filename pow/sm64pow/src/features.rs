//! The v0.4 consensus features, each behind its own switch (layer 1), so a test network can run
//! any subset and each change can be staged on its own (docs/V0.4-DESIGN.md section 10):
//!
//! - [`Features::committed_inputs`] (stage A): proofs carry the committed input hash
//!   (commit.rs); encodings are transport. Off: v0.3's proof layout and proof_id.
//! - [`Features::included_proofs`] (stage B): blocks may carry proof records (records.rs).
//!   Off: a block's record set must be empty.
//! - [`Features::proof_weight`] (stage C): fork choice is "longer AND heavier". Off: proofs weigh
//!   nothing (v0.3's longest chain).
//! - [`Features::proof_rewards`] (stage C): included proofs are paid. Off: they are not.
//!
//! Weight and rewards need included proofs; included proofs need committed inputs (records
//! commit input hashes). [`Features::valid`] checks that. The switches are process-wide, set once
//! at start-up for the network (`set`); the defaults are the full v0.4 design. Everything that
//! can be node policy (tie-breaks, the codec registry, the windows' widths) is not a switch here.
//!
//! Read next: commit.rs.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    pub committed_inputs: bool,
    pub included_proofs: bool,
    pub proof_weight: bool,
    pub proof_rewards: bool,
}

impl Features {
    /// The full v0.4 design.
    pub const ALL: Features = Features { committed_inputs: true, included_proofs: true, proof_weight: true, proof_rewards: true };
    /// v0.3's consensus (nothing of v0.4).
    pub const NONE: Features = Features { committed_inputs: false, included_proofs: false, proof_weight: false, proof_rewards: false };
    /// Stage A: committed inputs only.
    pub const STAGE_A: Features = Features { committed_inputs: true, ..Features::NONE };
    /// Stage B: and included proofs as data (no weight, no rewards).
    pub const STAGE_B: Features = Features { included_proofs: true, ..Features::STAGE_A };

    /// Whether the dependencies hold.
    pub fn valid(&self) -> bool {
        (!self.included_proofs || self.committed_inputs) && (!(self.proof_weight || self.proof_rewards) || self.included_proofs)
    }

    fn bits(&self) -> u8 {
        self.committed_inputs as u8 | (self.included_proofs as u8) << 1 | (self.proof_weight as u8) << 2 | (self.proof_rewards as u8) << 3
    }

    fn from_bits(b: u8) -> Features {
        Features { committed_inputs: b & 1 != 0, included_proofs: b & 2 != 0, proof_weight: b & 4 != 0, proof_rewards: b & 8 != 0 }
    }
}

static ACTIVE: AtomicU8 = AtomicU8::new(0b1111);

/// The features this process runs.
pub fn active() -> Features {
    Features::from_bits(ACTIVE.load(Ordering::Relaxed))
}

/// Sets the features (start-up, or a test). Refuses a set whose dependencies don't hold.
pub fn set(f: Features) -> Result<(), &'static str> {
    if !f.valid() {
        return Err("v0.4 features: included proofs need committed inputs; weight and rewards need included proofs");
    }
    ACTIVE.store(f.bits(), Ordering::Relaxed);
    Ok(())
}

pub fn committed() -> bool {
    active().committed_inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependencies_and_round_trip() {
        for b in 0..16u8 {
            let f = Features::from_bits(b);
            assert_eq!(f.bits(), b);
        }
        assert!(Features::ALL.valid() && Features::NONE.valid() && Features::STAGE_A.valid() && Features::STAGE_B.valid());
        assert!(!Features { committed_inputs: false, ..Features::STAGE_B }.valid());
        assert!(!Features { proof_weight: true, ..Features::STAGE_A }.valid());
        assert!(!Features { proof_rewards: true, ..Features::STAGE_A }.valid());
        assert!(Features { proof_weight: true, ..Features::STAGE_B }.valid());
        assert_eq!(active(), Features::ALL, "the default is the full design");
    }
}
