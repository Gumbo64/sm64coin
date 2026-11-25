//! The state codec interface.

use sm64coin_perturb::Pad as GamePad;
use sm64coin_perturb::sm64::Features;

/// Streaming encoder: the replay hands it every frame in order.
pub trait StateEncoder {
    /// The wanted input of a non-poked frame, in the state `f`.
    fn pad(&mut self, f: &Features, wanted: GamePad);
    /// The frame was poked (no input is stored).
    fn poked(&mut self);
    fn finish(self: Box<Self>) -> Vec<u8>;
}

/// Streaming decoder, the mirror of [`StateEncoder`].
pub trait StateDecoder {
    fn pad(&mut self, f: &Features) -> GamePad;
    fn poked(&mut self);
    /// The canonical encoding of everything decoded so far (the decoder
    /// re-encodes alongside), to compare with the body: a run has exactly one
    /// valid encoding.
    fn finish(self: Box<Self>) -> Vec<u8>;
}

/// A codec whose body is decoded frame by frame during the replay. Like
/// `sm64pow::proof::Codec`, a new model is a new id.
pub trait StateCodec: Sync {
    fn id(&self) -> u8;
    fn name(&self) -> &'static str;
    fn encoder(&self) -> Box<dyn StateEncoder>;
    fn decoder<'a>(&self, body: &'a [u8]) -> Box<dyn StateDecoder + 'a>;
}
