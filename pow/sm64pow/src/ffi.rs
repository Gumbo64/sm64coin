//! C ABI used by the daemon (monero fork). See `sm64pow.h`.

use crate::proof::Codec;
use crate::{Verdict, params, proof_id, rules, seed_from_hash, verify};
use std::ffi::{CStr, c_char};
use std::cell::RefCell;

thread_local! {
    /// Per thread, so concurrent verifications don't clobber each other.
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg);
}

/// Loads the ROM. `rom_path` may be NULL or empty to use the default search
/// (`$SM64COIN_ROM`, `./baserom.us.z64`, `~/.sm64coin/baserom.us.z64`).
/// Returns 1 on success, 0 on failure (see `sm64pow_last_error`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_init(rom_path: *const c_char) -> i32 {
    let res = std::panic::catch_unwind(|| {
        let path = if rom_path.is_null() {
            None
        } else {
            let s = unsafe { CStr::from_ptr(rom_path) }.to_string_lossy().into_owned();
            if s.is_empty() { None } else { Some(s) }
        };
        match path {
            Some(p) => std::fs::read(&p)
                .map_err(|e| anyhow::anyhow!("reading ROM {p}: {e}"))
                .and_then(crate::init)
                .map(|_| ()),
            None => crate::init_default().map(|_| ()),
        }
    });
    match res {
        Ok(Ok(())) => 1,
        Ok(Err(e)) => {
            set_error(e.to_string());
            0
        }
        Err(_) => {
            set_error("panic while loading the ROM".into());
            0
        }
    }
}

/// Verifies a proof for the block whose hashing blob hashes to `hash32`
/// (32 bytes, the perturbation's seed). Returns the number of frames (> 0) when valid, 0 when
/// invalid, -1 on an internal error. The reason is in `sm64pow_last_error`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_verify(hash32: *const u8, difficulty: u64, solution: *const u8, len: usize) -> i64 {
    if hash32.is_null() {
        set_error("null seed hash".into());
        return -1;
    }
    let seed = crate::Seed(unsafe { *(hash32 as *const [u8; 32]) });
    let bytes: &[u8] = if solution.is_null() || len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(solution, len) }
    };
    match std::panic::catch_unwind(|| verify(&seed, difficulty, bytes)) {
        Ok(Ok(Verdict::Valid { frames })) => frames as i64,
        Ok(Ok(Verdict::Invalid(why))) => {
            set_error(why);
            0
        }
        Ok(Err(e)) => {
            set_error(e.to_string());
            -1
        }
        Err(_) => {
            set_error("panic during verification".into());
            -1
        }
    }
}

/// The task at `difficulty`: frame limit, random frames per window, window
/// length and burst length.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_params(
    difficulty: u64,
    max_frames: *mut u32,
    random_amount: *mut u32,
    window_length: *mut u32,
    burst_length: *mut u32,
) {
    let p = params(difficulty);
    unsafe {
        if !max_frames.is_null() {
            *max_frames = p.max_frames;
        }
        if !random_amount.is_null() {
            *random_amount = p.rng.random_amount;
        }
        if !window_length.is_null() {
            *window_length = p.rng.window_length;
        }
        if !burst_length.is_null() {
            *burst_length = p.rng.random_burst_length;
        }
    }
}

/// Copies this thread's last error message (NUL terminated, truncated to `cap`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_last_error(buf: *mut c_char, cap: usize) -> usize {
    let msg = LAST_ERROR.with(|e| e.borrow().clone());
    if buf.is_null() || cap == 0 {
        return msg.len();
    }
    let n = msg.len().min(cap - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(msg.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    n
}

/// The RNG seed for a block from `cn_fast_hash(hashing blob)` (32 bytes).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_seed(hash32: *const u8) -> u32 {
    let h: &[u8; 32] = unsafe { &*(hash32 as *const [u8; 32]) };
    seed_from_hash(h)
}

/// What a block id commits to for this proof blob (32 bytes into `out32`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_id(blob: *const u8, len: usize, out32: *mut u8) {
    let bytes: &[u8] = if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } };
    let id = std::panic::catch_unwind(|| proof_id(bytes)).unwrap_or([0xff; 32]);
    unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), out32, 32) };
}

/// The retargeted difficulty. `timestamps` and `cumulative` (as low/high
/// 64-bit halves) are `n` blocks, oldest first.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_next_difficulty(
    timestamps: *const u64,
    cumulative_lo: *const u64,
    cumulative_hi: *const u64,
    n: usize,
) -> u64 {
    if n == 0 || timestamps.is_null() || cumulative_lo.is_null() || cumulative_hi.is_null() {
        return 1;
    }
    let (ts, lo, hi) = unsafe {
        (
            std::slice::from_raw_parts(timestamps, n),
            std::slice::from_raw_parts(cumulative_lo, n),
            std::slice::from_raw_parts(cumulative_hi, n),
        )
    };
    let cd: Vec<u128> = lo.iter().zip(hi).map(|(&l, &h)| ((h as u128) << 64) | l as u128).collect();
    std::panic::catch_unwind(|| rules().next_difficulty(ts, &cd)).unwrap_or(1).max(1)
}

/// The difficulty a block must meet, given the retargeted one and the
/// seconds between the block's timestamp and its parent's.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_effective_difficulty(difficulty: u64, elapsed_seconds: u64) -> u64 {
    rules().effective_difficulty(difficulty, elapsed_seconds).max(1)
}

/// Wanted seconds between blocks.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_target_seconds() -> u64 {
    rules().target_seconds()
}

/// Copies the active rules' name and goal (NUL terminated) for the RPC.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rules_info(name: *mut c_char, name_cap: usize, goal: *mut c_char, goal_cap: usize) {
    fn put(s: &str, buf: *mut c_char, cap: usize) {
        if buf.is_null() || cap == 0 {
            return;
        }
        let n = s.len().min(cap - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
            *buf.add(n) = 0;
        }
    }
    put(rules().name(), name, name_cap);
    put(rules().goal(), goal, goal_cap);
}

fn copy_out(bytes: &[u8], out: *mut u8, cap: usize) -> usize {
    if !out.is_null() && cap >= bytes.len() {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    bytes.len()
}

/// Decodes a proof blob to raw pads (4 bytes each, codec-free), for the RPC
/// and explorer. Returns the byte length (call with `out` NULL to size the
/// buffer; nothing is written unless `cap` is big enough), or `usize::MAX`
/// if the blob does not decode.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_decode_raw(blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let bytes: &[u8] = if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } };
    match crate::proof::decode(bytes, rules().max_frames_any() as usize) {
        Ok(pads) => copy_out(&crate::proof::Raw.encode(&pads), out, cap),
        Err(e) => {
            set_error(e.to_string());
            usize::MAX
        }
    }
}

/// Re-encodes a proof blob with the smallest codec (the block id does not
/// change). Same return convention as `sm64pow_decode_raw`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_reencode(blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let bytes: &[u8] = if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } };
    match crate::proof::decode(bytes, rules().max_frames_any() as usize) {
        Ok(pads) => copy_out(&crate::proof::encode_best(&pads), out, cap),
        Err(e) => {
            set_error(e.to_string());
            usize::MAX
        }
    }
}

/// The perturbation's button probabilities at `difficulty`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_probs(difficulty: u64, a: *mut f32, b: *mut f32, z: *mut f32) {
    let p = params(difficulty);
    unsafe {
        if !a.is_null() {
            *a = p.rng.a_prob;
        }
        if !b.is_null() {
            *b = p.rng.b_prob;
        }
        if !z.is_null() {
            *z = p.rng.z_prob;
        }
    }
}

/// Frames between reseeds of the perturbation at `difficulty` (0: never).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_reseed_frames(difficulty: u64) -> u32 {
    params(difficulty).reseed_frames
}
