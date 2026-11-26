//! State-aware proof codecs: the inputs are coded given the game state they
//! are played in, so decoding happens frame by frame during the replay.
//!
//! Everything on the decode path is integer arithmetic (no floats, no ML
//! runtime), so every node decodes bit-identically. Bottom layer first:
//!
//! * [`range`]: the binary range coder, canonical (one byte string per bit
//!   sequence); [`BitIo`] is one code path for encoding and decoding.
//! * [`logistic`]: integer squash / stretch for the mixers.
//! * [`codec`]: the [`StateCodec`] interface (streaming, frame by frame).
//! * [`statectx`]: codec 3, an adaptive context-mixing model.
//! * [`statemix`]: codecs 4 and 5, a stronger model (what miners seal with).
//! * [`mix2`]: codecs 6 (polar) and 7 (square) sticks, the same model with another stick binarisation (v0.4).
//! * this file: the registry ([`STATE_CODECS`], [`SEAL_CODECS`]).
//!
//! This crate is the pure part (no game, no emulator): native nodes use it through
//! `sm64pow::statecodec`, the browser through `sm64coin-perturb-wasm`. Same source, so the
//! two decode identically. Consensus-critical: a change here is a new codec id.

pub mod codec;
pub mod logistic;
pub mod range;
pub mod statectx;
pub mod mix2;
pub mod statemix;
/// The miner-side normaliser's guide model (v0.3.x `mix2.rs` lab fork, renamed: `mix2` is v0.4's
/// frozen codecs 6-7). Not consensus: no codec id uses it.
#[cfg(any(feature = "normalise", feature = "experimental"))]
pub mod mix2lab;
mod trig;

pub use codec::*;
pub use range::*;
pub use statectx::*;

pub use sm64coin_perturb::sm64::Features;
/// A controller pad (buttons and stick).
pub use sm64coin_perturb::Pad;
pub use sm64coin_perturb::Pad as GamePad;

/// Every state codec this build knows (accepted in blocks only when
/// `sm64pow::statecodec::state_codec_enabled`; 6 and 7 only with committed inputs, see
/// `sm64pow::statecodec::state_codec`).
///
/// Ids are never reused and every id is a FROZEN decoder with golden vectors (docs/V0.4-DESIGN.md
/// section 1.5): 3 statectx1; 4, 5 statemix; 6 statemix2-polar, 7 statemix2-square (mix2.rs).
pub static STATE_CODECS: &[&dyn StateCodec] =
    &[&StateCtx1, &crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1], &crate::mix2::VARIANTS[0], &crate::mix2::VARIANTS[1]];

/// The codecs sealing tries (the newest family; 3 stays decodable).
pub static SEAL_CODECS: &[&dyn StateCodec] = &[&crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1]];

/// The codecs a v0.4 (committed inputs) miner tries: all of them, the smallest wins.
pub static SEAL_CODECS_COMMITTED: &[&dyn StateCodec] = STATE_CODECS;

/// Ids that are registered but only valid where the network has committed inputs (v0.4).
pub const COMMITTED_ONLY: &[u8] = &[6, 7];

pub fn state_codec(id: u8) -> Option<&'static dyn StateCodec> {
    STATE_CODECS.iter().copied().find(|c| c.id() == id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn f32_fixed_point_is_exact() {
        use sm64coin_perturb::sm64::f32_fx;
        for (f, want) in [(0.0f32, 0), (1.0, 256), (-1.5, -384), (100.25, 25664), (0.001, 0), (1e30, i32::MAX), (-1e30, -i32::MAX), (f32::NAN, i32::MAX)] {
            assert_eq!(f32_fx(f.to_bits(), 8), want, "{f}");
        }
    }
}
