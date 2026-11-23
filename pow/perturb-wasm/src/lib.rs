//! The perturbation for the browser miner and replays. JS reads the adapter's
//! memory regions (at the visual build's addresses, [`visual_regions`]) out of
//! the game before and after each frame and passes the bytes here; everything
//! else is `sm64coin-perturb`, the same code native nodes run.

use sm64coin_perturb::sm64::{VISUAL, progress, progress_regions, regions, view};
use sm64coin_perturb::{Pad, Perturber, PokeConfig};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmPerturber(Perturber);

#[wasm_bindgen]
impl WasmPerturber {
    #[wasm_bindgen(constructor)]
    pub fn new(seed_hex: &str, window_length: u32, random_amount: u32, burst_length: u32, a_prob: f32, b_prob: f32, z_prob: f32, timing_mix: u32, pad_mix: u32) -> Result<WasmPerturber, JsError> {
        if seed_hex.len() != 64 {
            return Err(JsError::new("seed hash must be 64 hex characters"));
        }
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = u8::from_str_radix(&seed_hex[2 * i..2 * i + 2], 16).map_err(|e| JsError::new(&e.to_string()))?;
        }
        let cfg = PokeConfig { window_length, random_amount, burst_length, a_prob, b_prob, z_prob, timing_mix, pad_mix };
        Ok(WasmPerturber(Perturber::new(&seed, &cfg)))
    }

    /// The pad to play this frame given the regions' bytes before it:
    /// `[button, stick_x, stick_y, poked]`.
    pub fn pad(&mut self, before: &[u8], button: u16, stick_x: i8, stick_y: i8) -> Vec<i32> {
        let (p, poked) = self.0.pad(view(before).clock, Pad { button, stick_x, stick_y });
        vec![p.button as i32, p.stick_x as i32, p.stick_y as i32, poked as i32]
    }

    /// The perturber's state (for save states), as bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, JsError> {
        serde_json::to_vec(&self.0).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Becomes the perturber saved by [`WasmPerturber::to_bytes`].
    pub fn load_bytes(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.0 = serde_json::from_slice(bytes).map_err(|e| JsError::new(&e.to_string()))?;
        Ok(())
    }

    /// A perturber from [`WasmPerturber::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<WasmPerturber, JsError> {
        Ok(WasmPerturber(serde_json::from_slice(bytes).map_err(|e| JsError::new(&e.to_string()))?))
    }

    /// After stepping: mixes in the state digest if the game simulated,
    /// refunds the frame's poke if it didn't.
    pub fn after(&mut self, before: &[u8], after: &[u8]) {
        let (b, a) = (view(before), view(after));
        self.0.settle(a.clock != b.clock, &a.digest);
    }
}

/// The memory regions to read from the visual (browser) build, as
/// `[addr0, len0, addr1, len1, ...]`, in order.
#[wasm_bindgen]
pub fn visual_regions() -> Vec<u32> {
    regions(&VISUAL).iter().flat_map(|&(a, l)| [a, l]).collect()
}

/// The memory regions [`progress_points`] reads from the visual build, as
/// `[addr0, len0, ...]`, in order.
#[wasm_bindgen]
pub fn visual_progress_regions() -> Vec<u32> {
    progress_regions(&VISUAL).iter().flat_map(|&(a, l)| [a, l]).collect()
}

/// The game's progress from the bytes of [`visual_progress_regions`]
/// (concatenated): `[stars, keys, grand_star (0/1), points]`. The same code
/// nodes use to judge a block.
#[wasm_bindgen]
pub fn progress_points(bytes: &[u8]) -> Vec<u32> {
    let p = progress(bytes);
    vec![p.stars, p.keys, p.grand_star as u32, p.points()]
}
