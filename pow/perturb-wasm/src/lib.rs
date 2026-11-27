//! The perturbation for the browser miner and replays. JS reads the adapter's
//! memory regions (at the visual build's addresses, [`visual_regions`]) out of
//! the game before and after each frame and passes the bytes here; everything
//! else is `sm64coin-perturb`, the same code native nodes run.

use sm64coin_perturb::sm64::{HEADLESS, VISUAL, event, features, progress, progress_regions, regions, view};
use sm64coin_perturb::{Pad, Perturber, PokeConfig};
use wasm_bindgen::prelude::*;

/// The consensus rules, the very file `sm64pow` compiles (it needs only `sm64coin-perturb`, not the game): the time limit the
/// page computes from a tip's timestamp and difficulty is the node's, not a port of it.
#[path = "../../sm64pow/src/rules.rs"]
#[allow(dead_code)]
pub mod rules;

/// The time limit in frames for a block timestamped `elapsed_seconds` after its parent at retarget difficulty `difficulty`
/// (`Rules::max_frames` of the active rules).
#[wasm_bindgen]
pub fn rules_max_frames(difficulty: u64, elapsed_seconds: u64) -> u32 {
    use rules::Rules;
    rules::active().max_frames(difficulty, elapsed_seconds)
}

/// The first second after the parent at which a run of `frames` fits the limit at retarget difficulty `difficulty` (the limit
/// only grows with time); `u64::MAX`: never. A block carrying that run is valid from `parent timestamp + this`.
#[wasm_bindgen]
pub fn rules_due_elapsed(difficulty: u64, frames: u32) -> u64 {
    use rules::Rules;
    let r = rules::active();
    const FAR: u64 = 1 << 40;
    if r.max_frames(difficulty, FAR) < frames {
        return u64::MAX;
    }
    let (mut lo, mut hi) = (0u64, FAR);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if r.max_frames(difficulty, mid) >= frames { hi = mid } else { lo = mid + 1 }
    }
    lo
}

/// The most frames any block can carry (the limit at difficulty 1 once fully grown).
#[wasm_bindgen]
pub fn rules_max_run_frames() -> u32 {
    use rules::Rules;
    rules::active().max_frames_any()
}

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
/// (concatenated): `[stars, keys, grand_star (0/1), points, caps]` (caps: bit 0 wing, 1 metal, 2 vanish; points counts them). The same code
/// nodes use to judge a block.
#[wasm_bindgen]
pub fn progress_points(bytes: &[u8]) -> Vec<u32> {
    let p = progress(bytes);
    vec![p.stars, p.keys, p.grand_star as u32, p.points(), p.caps as u32]
}

/// The memory regions to read from the headless build (the one native nodes replay with; the
/// browser runs the same build in a worker), as `[addr0, len0, ...]`, in order.
#[wasm_bindgen]
pub fn headless_regions() -> Vec<u32> {
    regions(&HEADLESS).iter().flat_map(|&(a, l)| [a, l]).collect()
}

/// The memory regions [`progress_points`] reads from the headless build.
#[wasm_bindgen]
pub fn headless_progress_regions() -> Vec<u32> {
    progress_regions(&HEADLESS).iter().flat_map(|&(a, l)| [a, l]).collect()
}

/// A state-coded proof's decoder (codecs 3+: the inputs are coded given the game state):
/// the same `sm64coin-statecodec` code native nodes decode with. Per frame of the replay:
/// [`WasmPerturber::pad`] first; on a poked frame call [`WasmStateDecoder::poked`], else
/// [`WasmStateDecoder::pad`] with the same region bytes gives the input to play.
#[wasm_bindgen]
pub struct WasmStateDecoder {
    // Declared first: dropped before the body it borrows.
    dec: Option<Box<dyn sm64coin_statecodec::StateDecoder + 'static>>,
    body: *mut [u8],
}

#[wasm_bindgen]
impl WasmStateDecoder {
    /// `codec`: the proof's codec id; `body`: the proof bytes after its header.
    #[wasm_bindgen(constructor)]
    pub fn new(codec: u8, body: &[u8]) -> Result<WasmStateDecoder, JsError> {
        let c = sm64coin_statecodec::STATE_CODECS.iter().copied().find(|c| c.id() == codec).ok_or_else(|| JsError::new(&format!("unknown state codec {codec}")))?;
        let body: *mut [u8] = Box::into_raw(body.to_vec().into_boxed_slice());
        // Safety: the box lives until Drop, which drops `dec` first.
        let dec = c.decoder(unsafe { &*body });
        Ok(WasmStateDecoder { dec: Some(dec), body })
    }

    /// The input of a non-poked frame, given the regions' bytes before it: `[button, stick_x, stick_y]`.
    pub fn pad(&mut self, before: &[u8]) -> Result<Vec<i32>, JsError> {
        let d = self.dec.as_mut().ok_or_else(|| JsError::new("decoder finished"))?;
        let p = d.pad(&features(before));
        Ok(vec![p.button as i32, p.stick_x as i32, p.stick_y as i32])
    }

    /// The frame was poked (no input was stored for it).
    pub fn poked(&mut self) -> Result<(), JsError> {
        self.dec.as_mut().ok_or_else(|| JsError::new("decoder finished"))?.poked();
        Ok(())
    }

    /// The canonical encoding of what was decoded: equal to the body for a valid proof.
    pub fn finish(&mut self) -> Result<Vec<u8>, JsError> {
        Ok(self.dec.take().ok_or_else(|| JsError::new("decoder finished"))?.finish())
    }
}

impl Drop for WasmStateDecoder {
    fn drop(&mut self) {
        self.dec = None;
        // Safety: from Box::into_raw in `new`, freed once.
        drop(unsafe { Box::from_raw(self.body) });
    }
}

/// What a block added, from the progress regions' bytes before and after its run: the names
/// nodes report ("c01s6" course 1 star 6, "c00s3" castle, "key1", "grand"); empty if nothing.
#[wasm_bindgen]
pub fn progress_event(before: &[u8], after: &[u8]) -> String {
    event(&progress(before), &progress(after)).map(|e| e.name()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Difficulties and elapsed seconds the vectors cover; `limit_vectors.json` holds the answers (checked by sm64pow's own test
    /// too, so a rule change cannot slip past either side).
    const DIFFS: [u64; 9] = [1, 2, 1000, 12_345_678, 1_000_000_000, 1u64 << 40, 1_000_000_000_000_000, u64::MAX / 3, u64::MAX];
    const ELAPSED: [u64; 14] = [0, 1, 59, 60, 61, 300, 600, 1200, 1800, 3600, 7200, 86_400, 1_000_000, 10_000_000];

    #[test]
    fn limit_vectors_match_the_rules() {
        let rows: Vec<(u64, u64, u32)> = DIFFS.iter().flat_map(|&d| ELAPSED.iter().map(move |&e| (d, e, rules_max_frames(d, e)))).collect();
        let json = serde_json::to_string(&rows.iter().map(|(d, e, f)| serde_json::json!([d.to_string(), e, f])).collect::<Vec<_>>()).unwrap();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../sm64pow/tests/limit_vectors.json");
        if std::env::var("WRITE_LIMIT_VECTORS").is_ok() {
            std::fs::write(path, &json).unwrap();
        }
        let have: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(have, serde_json::from_str::<serde_json::Value>(&json).unwrap(), "pow/sm64pow/tests/limit_vectors.json is stale (WRITE_LIMIT_VECTORS=1 cargo test regenerates it)");
        assert_eq!(rules_max_run_frames(), 36000);
        // due time: the first second the limit reaches the run, for every row
        for (d, e, f) in rows {
            let due = rules_due_elapsed(d, f);
            assert!(due <= e && rules_max_frames(d, due) >= f && (due == 0 || rules_max_frames(d, due - 1) < f), "{d} {e} {f} {due}");
        }
        assert_eq!(rules_due_elapsed(1, 36001), u64::MAX);
    }
}
