//! The C ABI the daemon calls (layer 6), declared in `sm64pow.h`.
//!
//! The Monero fork wraps it in monero/src/sm64coin/sm64coin.{h,cpp}; every
//! function catches panics and reports errors through
//! [`sm64pow_last_error`] (per thread). Changing a signature here means
//! changing the header and the wrapper too.

use crate::proof::Codec;
use crate::proof_id::proof_id;
use crate::rules::{active as rules, params};
use crate::seed::{Seed, seed_from_hash};
use std::ffi::{CStr, c_char};
use std::cell::RefCell;

// ---- Setup and errors ----

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
                .and_then(crate::emulator::init)
                .map(|_| ()),
            None => crate::emulator::init_default().map(|_| ()),
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

/// A 32-byte argument (block id, seed hash), None for a null pointer.
unsafe fn id_arg(p: *const u8) -> Option<[u8; 32]> {
    if p.is_null() { None } else { Some(unsafe { *(p as *const [u8; 32]) }) }
}

/// The block's parent: `parent32` null means the genesis block.
unsafe fn parent_arg(parent32: *const u8, gap_seconds: u64) -> crate::chain::Parent {
    match unsafe { id_arg(parent32) } {
        None => crate::chain::Parent::Genesis,
        Some(id) => crate::chain::Parent::Block { id, gap_seconds },
    }
}

fn blob_arg<'a>(blob: *const u8, len: usize) -> &'a [u8] {
    if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } }
}

fn copy_out(bytes: &[u8], out: *mut u8, cap: usize) -> usize {
    if !out.is_null() && cap >= bytes.len() {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    bytes.len()
}

// ---- Verifying blocks: the chain of game states (chain.rs) ----

/// Verifies block `id32` (seed `hash32`, proof) on top of `parent32` (null:
/// the genesis block) timestamped `gap_seconds` after it, at effective
/// `difficulty`; 0 difficulty replays an already-accepted block without the
/// time limit (rebuilding its end state). `difficulty` is the retarget
/// difficulty: the time limit grows with `gap_seconds`. Caches the end state when valid.
/// Returns frames (>0) if valid, 0 invalid, -2 when the parent's end state
/// isn't cached (replay the parent first), -1 internal error; reason in
/// sm64pow_last_error.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_verify_block(id32: *const u8, parent32: *const u8, gap_seconds: u64, hash32: *const u8, difficulty: u64, proof: *const u8, len: usize) -> i64 {
    let (Some(id), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(hash32) }) else {
        set_error("null block id or seed hash".into());
        return -1;
    };
    let parent = unsafe { parent_arg(parent32, gap_seconds) };
    let bytes = blob_arg(proof, len);
    let seed = Seed(seed);
    let res = std::panic::catch_unwind(|| {
        if difficulty == 0 {
            crate::chain::rebuild_block(&id, parent, &seed, bytes)
        } else {
            crate::chain::verify_block(&id, parent, &seed, difficulty, bytes)
        }
    });
    use crate::chain::BlockVerdict;
    match res {
        Ok(Ok(BlockVerdict::Valid { frames, .. })) => frames as i64,
        Ok(Ok(BlockVerdict::Invalid(why))) => {
            set_error(why);
            0
        }
        Ok(Ok(BlockVerdict::NeedParent)) => {
            set_error("the parent's end state isn't cached".into());
            -2
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

/// The parallel pre-pass of `n` blocks in chain order (a sync batch, see
/// chain::preverify): `ids`, `parents` (`n` x 32 bytes; a parent of all zero
/// bytes is the genesis block), `seeds` (`n` x 32), the proofs concatenated in
/// `blobs` with `offsets` (`n + 1` entries). Returns how many runs it replayed.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_preverify(n: usize, ids: *const u8, parents: *const u8, seeds: *const u8, blobs: *const u8, offsets: *const u64, threads: u32) -> u64 {
    if n == 0 || ids.is_null() || parents.is_null() || seeds.is_null() || offsets.is_null() {
        return 0;
    }
    let (ids, parents, seeds, offsets) = unsafe {
        (
            std::slice::from_raw_parts(ids, n * 32),
            std::slice::from_raw_parts(parents, n * 32),
            std::slice::from_raw_parts(seeds, n * 32),
            std::slice::from_raw_parts(offsets, n + 1),
        )
    };
    let total = offsets[n] as usize;
    let blobs: &[u8] = if total == 0 || blobs.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(blobs, total) } };
    let mut batch = Vec::with_capacity(n);
    for i in 0..n {
        let id: [u8; 32] = ids[32 * i..32 * i + 32].try_into().unwrap();
        let p: [u8; 32] = parents[32 * i..32 * i + 32].try_into().unwrap();
        let seed: [u8; 32] = seeds[32 * i..32 * i + 32].try_into().unwrap();
        let (a, b) = (offsets[i] as usize, offsets[i + 1] as usize);
        if a > b || b > blobs.len() {
            return 0;
        }
        let parent = if p == [0u8; 32] { crate::chain::Parent::Genesis } else { crate::chain::Parent::Block { id: p, gap_seconds: 0 } };
        batch.push(crate::chain::BatchBlock { id, parent, seed: Seed(seed), blob: blobs[a..b].to_vec() });
    }
    std::panic::catch_unwind(|| crate::chain::preverify(&batch, threads as usize)).unwrap_or(0) as u64
}

// ---- Admission policy: which blocks earn a replay (admit.rs, docs/DOS.md) ----

fn str_arg(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// Sets this thread's source of blocks until [`sm64pow_source_leave`] with the
/// returned handle: `host` (the peer's or RPC client's address), `source` 1 a
/// block relayed unasked, 2 a sync span whose last block is at height `top`,
/// 3 a block submitted over restricted RPC. Blocks handled without a source
/// are never refused.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_source_enter(host: *const c_char, source: i32, top: u64) -> *mut std::ffi::c_void {
    use crate::admit::{Context, Source};
    let src = match source {
        1 => Some(Source::Relay),
        2 => Some(Source::Sync { top }),
        3 => Some(Source::RpcSubmit),
        _ => None,
    };
    let ctx = src.map(|source| Context { host: str_arg(host), source });
    Box::into_raw(Box::new(crate::admit::enter(ctx))) as *mut std::ffi::c_void
}

#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_source_leave(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut crate::admit::ContextGuard) });
    }
}

/// Whether to replay an alternative block (`id32`, `parent32`, seed `hash32`,
/// claiming `frames`) at `height` when our tip is at `top`; `main_seed32` (or
/// null) is the seed of our main-chain block at that height. 1 replay it, 0
/// skip it (not stored, not relayed, its sender not punished; the reason in
/// sm64pow_last_error).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_admit_alt(id32: *const u8, parent32: *const u8, hash32: *const u8, frames: u32, height: u64, top: u64, main_seed32: *const u8) -> i32 {
    let (Some(id), Some(parent), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(parent32) }, unsafe { id_arg(hash32) }) else { return 1 };
    let main_seed = unsafe { id_arg(main_seed32) }.map(Seed);
    let b = crate::admit::AltBlock { id, parent, seed: Seed(seed), frames, height, top, main_seed };
    match std::panic::catch_unwind(|| crate::admit::admit_alt(&b)) {
        Ok(crate::admit::Decision::Skip(why)) => {
            set_error(why.into());
            0
        }
        _ => 1,
    }
}

/// Whether to replay a block extending the tip that claims `frames`, in this
/// thread's source (see sm64pow_source_enter): 1 yes, 0 skip it.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_admit_relay(frames: u32) -> i32 {
    match std::panic::catch_unwind(|| crate::admit::admit_relay(frames)) {
        Ok(crate::admit::Decision::Skip(why)) => {
            set_error(why.into());
            0
        }
        _ => 1,
    }
}

/// Whether restricted-RPC client `host` may submit a block now (0: it sent an
/// invalid proof recently).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_submit_allowed(host: *const c_char) -> i32 {
    let h = str_arg(host);
    std::panic::catch_unwind(|| crate::admit::rpc_submit_allowed(&h)).unwrap_or(true) as i32
}

/// Marks this thread as serving an RPC call until [`sm64pow_rpc_leave`]:
/// replays it causes draw on the RPC replay budget (and fail when it is used up).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_enter() -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(crate::admit::RpcScope::enter())) as *mut std::ffi::c_void
}

#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_leave(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut crate::admit::RpcScope) });
    }
}

/// Keeps the last few blocks' end states in directory `path` (NUL-terminated)
/// and loads those already there. Returns how many were loaded, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_set_state_dir(path: *const c_char) -> i32 {
    if path.is_null() {
        return -1;
    }
    let Ok(p) = (unsafe { std::ffi::CStr::from_ptr(path) }).to_str() else { return -1 };
    std::panic::catch_unwind(|| crate::chain::set_state_dir(std::path::Path::new(p)) as i32).unwrap_or(-1)
}

/// Whether block `id32`'s end state is cached (1) or not (0).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_has_state(id32: *const u8) -> i32 {
    match unsafe { id_arg(id32) } {
        Some(id) => crate::chain::cached(&id).is_some() as i32,
        None => 0,
    }
}

/// Block `id32`'s cached end state (the game's save state), gzip; for get_sm64_state. Same
/// return convention as `sm64pow_decode_raw`; SIZE_MAX when it isn't cached.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_end_state_gz(id32: *const u8, out: *mut u8, cap: usize) -> usize {
    match unsafe { id_arg(id32) }.and_then(|id| crate::chain::end_state_gz(&id)) {
        Some(gz) => copy_out(&gz, out, cap),
        None => {
            set_error("the block's end state isn't cached".into());
            usize::MAX
        }
    }
}

/// A cached block's end progress: stars, keys, whether it has the Grand
/// Star and whether it ends the cycle. 1 if cached, 0 if not.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_block_progress(id32: *const u8, stars: *mut u32, keys: *mut u32, grand_star: *mut i32, ends_cycle: *mut i32) -> i32 {
    // From the end state, or the run's summary (the parallel pre-pass keeps only those).
    let Some((progress, ends, _)) = (unsafe { id_arg(id32) }).and_then(|id| crate::chain::info(&id)) else { return 0 };
    unsafe {
        if !stars.is_null() { *stars = progress.stars; }
        if !keys.is_null() { *keys = progress.keys; }
        if !grand_star.is_null() { *grand_star = progress.grand_star as i32; }
        if !ends_cycle.is_null() { *ends_cycle = ends as i32; }
    }
    1
}

/// What a cached block's run added, as a short name ("c09s1": course 9
/// star 1, "c00s3": castle secret star, "key1", "grand"); bookkeeping only.
/// Returns the name's length (0 if not cached or unknown); writes at most `cap`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_block_event(id32: *const u8, out: *mut c_char, cap: usize) -> usize {
    let Some((_, _, event)) = (unsafe { id_arg(id32) }).and_then(|id| crate::chain::info(&id)) else { return 0 };
    let name = event.map(|e| e.name()).unwrap_or_default();
    if !out.is_null() && cap > 0 {
        let n = name.len().min(cap - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(name.as_ptr(), out as *mut u8, n);
            *out.add(n) = 0;
        }
    }
    name.len()
}

/// The pads block `id32`'s run played on every frame, pokes regenerated
/// (for replays and .m64 export); parent arguments as for
/// sm64pow_verify_block. Same return convention as `sm64pow_decode_raw`,
/// plus SIZE_MAX-1 when the parent's end state isn't cached.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_played_block(id32: *const u8, parent32: *const u8, gap_seconds: u64, hash32: *const u8, proof: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let (Some(id), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(hash32) }) else {
        set_error("null block id or seed hash".into());
        return usize::MAX;
    };
    let parent = unsafe { parent_arg(parent32, gap_seconds) };
    let bytes = blob_arg(proof, len);
    let res = std::panic::catch_unwind(|| crate::chain::played_block(&id, parent, &Seed(seed), bytes));
    match res {
        Ok(Ok(Ok(pads))) => copy_out(&crate::proof::Raw.encode(&pads), out, cap),
        Ok(Ok(Err(crate::chain::BlockVerdict::NeedParent))) => {
            set_error("the parent's end state isn't cached".into());
            usize::MAX - 1
        }
        Ok(Ok(Err(v))) => {
            set_error(format!("{v:?}"));
            usize::MAX
        }
        Ok(Err(e)) => {
            set_error(e.to_string());
            usize::MAX
        }
        Err(_) => {
            set_error("panic while replaying".into());
            usize::MAX
        }
    }
}

/// The inputs of block `id32`'s proof, codec-free (the non-poked frames' pads;
/// replays the run, so state-coded proofs work); parent arguments as for
/// sm64pow_verify_block. Same return convention as `sm64pow_decode_raw`,
/// plus SIZE_MAX-1 when the parent's end state isn't cached.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_wanted_block(id32: *const u8, parent32: *const u8, gap_seconds: u64, hash32: *const u8, proof: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    let (Some(id), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(hash32) }) else {
        set_error("null block id or seed hash".into());
        return usize::MAX;
    };
    let parent = unsafe { parent_arg(parent32, gap_seconds) };
    let bytes = blob_arg(proof, len);
    let res = std::panic::catch_unwind(|| crate::chain::played_wanted_block(&id, parent, &Seed(seed), bytes));
    match res {
        Ok(Ok(Ok(pl))) => copy_out(&crate::proof::Raw.encode(&pl.wanted()), out, cap),
        Ok(Ok(Err(crate::chain::BlockVerdict::NeedParent))) => {
            set_error("the parent's end state isn't cached".into());
            usize::MAX - 1
        }
        Ok(Ok(Err(v))) => {
            set_error(format!("{v:?}"));
            usize::MAX
        }
        Ok(Err(e)) => {
            set_error(e.to_string());
            usize::MAX
        }
        Err(_) => {
            set_error("panic while replaying".into());
            usize::MAX
        }
    }
}

// ---- Proof blobs (proof.rs, proof_id.rs) and the seed ----

/// What a block id commits to for this proof blob (32 bytes into `out32`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_id(blob: *const u8, len: usize, out32: *mut u8) {
    let bytes: &[u8] = if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } };
    let id = std::panic::catch_unwind(|| proof_id(bytes)).unwrap_or([0xff; 32]);
    unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), out32, 32) };
}

/// A proof's start: 0 from the parent's end state, 1 from power-on, -1 if it
/// doesn't decode.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_start(proof: *const u8, len: usize) -> i32 {
    crate::proof::proof_start(blob_arg(proof, len)).map_or(-1, |s| s as i32)
}

/// How many frames a proof's run lasted (0 if it doesn't decode).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_frames(blob: *const u8, len: usize) -> u32 {
    let b = blob_arg(blob, len);
    crate::proof::decode_proof(b, rules().max_frames_any() as usize).map_or_else(|_| crate::proof::split_blob(b).map_or(0, |x| if crate::statecodec::state_codec(x.2).is_some() { x.0 } else { 0 }), |pr| pr.frames)
}

/// Decodes a proof blob to its inputs (the non-poked frames' pads, 4 bytes
/// each, codec-free). Returns the byte length (call with `out` NULL to size
/// the buffer; nothing is written unless `cap` is big enough), or
/// `usize::MAX` if the blob does not decode.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_decode_raw(blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    match crate::proof::decode_proof(blob_arg(blob, len), rules().max_frames_any() as usize) {
        Ok(pr) => copy_out(&crate::proof::Raw.encode(&pr.inputs), out, cap),
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
    match crate::proof::decode_proof(blob_arg(blob, len), rules().max_frames_any() as usize) {
        Ok(pr) => copy_out(&crate::proof::encode_proof(&pr), out, cap),
        Err(e) => {
            set_error(e.to_string());
            usize::MAX
        }
    }
}

/// The first word of a seed hash (32 bytes), for display (see `seed_from_hash`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_seed(hash32: *const u8) -> u32 {
    let h: &[u8; 32] = unsafe { &*(hash32 as *const [u8; 32]) };
    seed_from_hash(h)
}

// ---- The rules (rules.rs) and the hold policy (hold.rs) ----

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

/// The time limit (frames) of a block at retarget `difficulty` timestamped
/// `elapsed_seconds` after its parent (continuous in time).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_max_frames(difficulty: u64, elapsed_seconds: u64) -> u32 {
    rules().max_frames(difficulty, elapsed_seconds)
}

/// The hold policy (`chain::hold_until`): 0 to process the block now, else
/// the unix second until which to hold it. `restart_delay` 0 for no restart.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_hold_until(difficulty: u64, parent_ts: u64, block_ts: u64, now: u64, frames: u32, restart_delay: u64) -> u64 {
    crate::chain::hold_until(difficulty, parent_ts, block_ts, now, frames, restart_delay).unwrap_or(0)
}

/// Whether fork choice counts blocks (`Rules::weigh_by_blocks`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_weigh_by_blocks() -> bool {
    rules().weigh_by_blocks()
}

/// No block but the genesis block may be timestamped before this.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_launch_timestamp() -> u64 {
    rules().launch_timestamp()
}

/// How many seconds earlier a power-on restart on top of a parent with this
/// progress takes its limit (`Rules::restart_delay`); -1: not allowed.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_restart_delay(stars: u32, keys: u32, grand_star: bool) -> i64 {
    let p = crate::rules::Progress { stars, keys, grand_star, star_flags: [0; 26] };
    rules().restart_delay(&p).map_or(-1, |c| c.min(i64::MAX as u64) as i64)
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

/// The poke streams' mix intervals at `difficulty` (simulated frames between
/// mixes of the game state into the timing and pad streams; 0: never).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_mix(difficulty: u64, timing_mix: *mut u32, pad_mix: *mut u32) {
    let p = params(difficulty);
    unsafe {
        if !timing_mix.is_null() {
            *timing_mix = p.rng.timing_mix;
        }
        if !pad_mix.is_null() {
            *pad_mix = p.rng.pad_mix;
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

/// Frames between reseeds of the perturbation at `difficulty`: always 0 (vestigial, see
/// `Params::reseed_frames`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_reseed_frames(difficulty: u64) -> u32 {
    params(difficulty).reseed_frames
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
