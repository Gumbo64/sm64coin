//! State-aware proof codecs: the inputs are coded given the game state they
//! are played in, so decoding happens frame by frame during the replay.
//!
//! Everything on the decode path is integer arithmetic (no floats, no ML
//! runtime), so every node decodes bit-identically:
//!
//! * [`RangeEncoder`] / [`RangeDecoder`]: a binary range coder (LZMA style,
//!   12-bit probabilities) with a canonical minimal flush and an implicit
//!   all-zero tail, so each bit sequence has exactly one byte string.
//! * [`Model`]: predicts every binary decision of a pad (same as before?,
//!   which buttons, which stick values) from several small contexts of the
//!   game state ([`Features`]) and the previous pads; the predictions are
//!   mixed by a logistic mixer and every counter adapts while coding
//!   (prequential, nothing is transmitted). One code path ([`BitIo`]) both
//!   encodes and decodes, so the two cannot disagree.
//! * [`StateCodec`]: the codec ids built from it. A model change is a new id.
//!
//! This crate is the pure part (no game, no emulator): native nodes use it through
//! `sm64pow::statecodec`, the browser through `sm64coin-perturb-wasm`. Same source, so the
//! two decode identically. Consensus-critical: a change here is a new codec id.

pub mod codec;
pub mod logistic;
pub mod range;
pub mod statectx;
pub mod statemix;
mod trig;

pub use codec::*;
pub use range::*;
pub use statectx::*;
pub(crate) use logistic::{squash, stretch_table};

pub use sm64coin_perturb::sm64::Features;
/// A controller pad (buttons and stick).
pub use sm64coin_perturb::Pad;
pub use sm64coin_perturb::Pad as GamePad;

/// Every state codec this build knows (accepted in blocks only when
/// [`state_codec_enabled`]).
pub static STATE_CODECS: &[&dyn StateCodec] = &[&StateCtx1, &crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1]];

/// The codecs sealing tries (the newest family; 3 stays decodable).
pub static SEAL_CODECS: &[&dyn StateCodec] = &[&crate::statemix::VARIANTS[0], &crate::statemix::VARIANTS[1]];

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
