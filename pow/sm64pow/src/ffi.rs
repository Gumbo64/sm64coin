//! The C ABI the daemon calls (layer 6), declared in `sm64pow.h`.
//!
//! The Monero fork wraps it in monero/src/sm64coin/sm64coin.{h,cpp}; every
//! function catches panics and reports errors through
//! [`sm64pow_last_error`] (per thread). Changing a signature here means
//! changing the header and the wrapper too.

use crate::proof::Codec;
use crate::proof_id::active as proof_id; // v0.4: the committed input hash when on (features.rs)
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
/// The proof is a committed blob (v0.4, commit.rs). Returns frames (>0) if valid, 0 invalid,
/// -2 when the parent's end state isn't cached (replay the parent first), -3 a bad encoding (the
/// blob doesn't decode to its committed inputs: drop the blob and its sender, never mark the
/// block id invalid), -1 internal error; reason in sm64pow_last_error.
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
            crate::chain::rebuild_any(&id, parent, &seed, bytes)
        } else {
            crate::chain::verify_any(&id, parent, &seed, difficulty, bytes)
        }
    });
    use crate::chain::{BlockVerdict, CommittedVerdict};
    match res {
        Ok(Ok(CommittedVerdict::Run(BlockVerdict::Valid { frames, .. }))) => frames as i64,
        Ok(Ok(CommittedVerdict::Run(BlockVerdict::Invalid(why)))) => {
            set_error(why);
            0
        }
        Ok(Ok(CommittedVerdict::BadEncoding(why))) => {
            set_error(format!("bad encoding: {why}"));
            -3
        }
        Ok(Ok(CommittedVerdict::Run(BlockVerdict::NeedParent))) => {
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

fn sig_arg(p: *const u8) -> Option<[u8; 64]> {
    if p.is_null() { None } else { Some(unsafe { *(p as *const [u8; 64]) }) }
}

// ---- Signed proofs (sign.rs, equivocation.rs) ----

/// [`sm64pow_verify_block`] for a signed block: first checks that the
/// coinbase extra nonce (`nonce`, `nonce_len`: the miner tx's first extra
/// nonce) commits a miner key and that `sig64` signs (seed, proof_id, frames)
/// with it, so a block without a valid signature is invalid without a replay.
/// `difficulty` 0 (rebuilding an accepted block) skips the signature.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_verify_signed_block(id32: *const u8, parent32: *const u8, gap_seconds: u64, hash32: *const u8, difficulty: u64, proof: *const u8, len: usize, nonce: *const u8, nonce_len: usize, sig64: *const u8) -> i64 {
    if difficulty != 0 {
        let (Some(seed), Some(sig)) = (unsafe { id_arg(hash32) }, sig_arg(sig64)) else {
            set_error("null seed hash or signature".into());
            return -1;
        };
        let (bytes, n) = (blob_arg(proof, len), blob_arg(nonce, nonce_len));
        match std::panic::catch_unwind(|| crate::chain::claim_signed_run(&Seed(seed), bytes, n, &sig)) {
            Ok(Ok(_)) => {}
            Ok(Err(why)) => {
                crate::admit::note_invalid();
                set_error(why);
                return 0;
            }
            Err(_) => {
                set_error("panic while checking the signature".into());
                return -1;
            }
        }
    }
    sm64pow_verify_block(id32, parent32, gap_seconds, hash32, difficulty, proof, len)
}

/// Whether a block's signature is valid (1) or not (0; reason in
/// sm64pow_last_error). No replay: the sync pre-pass and the hold check it
/// before spending one.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_check_signature(hash32: *const u8, proof: *const u8, len: usize, nonce: *const u8, nonce_len: usize, sig64: *const u8) -> i32 {
    let (Some(seed), Some(sig)) = (unsafe { id_arg(hash32) }, sig_arg(sig64)) else { return 0 };
    let (bytes, n) = (blob_arg(proof, len), blob_arg(nonce, nonce_len));
    match std::panic::catch_unwind(|| crate::chain::signed_run(&Seed(seed), bytes, n, &sig)) {
        Ok(Ok(_)) => 1,
        Ok(Err(why)) => {
            set_error(why);
            0
        }
        Err(_) => 0,
    }
}

/// The one-run-per-key rule (equivocation.rs) for a block whose signature
/// is checked here: the coinbase extra nonce `nonce`, `proof`, `sig64`, for
/// the template with seed `hash32`. Returns 0 for a bad signature (the block
/// is invalid; reason in sm64pow_last_error), 1 the first run seen for the
/// template (its claim), 2 the same run again (a duplicate), 3 a different
/// run: equivocation. Then the evidence is kept and queued for relay
/// ([`sm64pow_take_evidence`]); the caller does not replay this block.
/// -1 on an internal error. No replay.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_note_claim(hash32: *const u8, proof: *const u8, len: usize, nonce: *const u8, nonce_len: usize, sig64: *const u8) -> i32 {
    let (Some(seed), Some(sig)) = (unsafe { id_arg(hash32) }, sig_arg(sig64)) else { return -1 };
    let (bytes, n) = (blob_arg(proof, len), blob_arg(nonce, nonce_len));
    match std::panic::catch_unwind(|| crate::chain::claim_signed_run(&Seed(seed), bytes, n, &sig)) {
        Ok(Ok(crate::equivocation::Claim::First)) => 1,
        Ok(Ok(crate::equivocation::Claim::Same)) => 2,
        Ok(Ok(crate::equivocation::Claim::Equivocation(_))) => 3,
        Ok(Err(why)) => {
            crate::admit::note_invalid();
            set_error(why);
            0
        }
        Err(_) => -1,
    }
}

/// Another run for the template (seed `hash32`, coinbase extra nonce
/// `nonce`) of a block already in our chain (`main_proof`, `main_sig64`),
/// arriving as an alternative block (`alt_proof`, `alt_sig64`): the fallback
/// of [`sm64pow_note_claim`] after a restart, when the claims are forgotten.
/// Returns 0 the same run, 3 a bad signature, 4 equivocation (any other
/// signed run, whatever its length): the evidence (EVIDENCE_LEN = 272 bytes)
/// is written to `out` if `cap` allows and kept (relay it; templates then
/// avoid the block); 5 the same evidence again; -1 our block's own signature
/// doesn't check (should not happen).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_judge_sibling(hash32: *const u8, nonce: *const u8, nonce_len: usize, main_proof: *const u8, main_len: usize, main_sig64: *const u8, alt_proof: *const u8, alt_len: usize, alt_sig64: *const u8, out: *mut u8, cap: usize) -> i32 {
    let (Some(seed), Some(ms), Some(asig)) = (unsafe { id_arg(hash32) }, sig_arg(main_sig64), sig_arg(alt_sig64)) else { return -1 };
    let n = blob_arg(nonce, nonce_len);
    let (mp, ap) = (blob_arg(main_proof, main_len), blob_arg(alt_proof, alt_len));
    let r = std::panic::catch_unwind(|| {
        use crate::equivocation::{Sibling, judge_sibling};
        let seed = Seed(seed);
        let Ok((key, main)) = crate::chain::signed_run(&seed, mp, n, &ms) else { return (-1, None) };
        let Ok(frames) = crate::proof_id::claimed_frames(ap) else { return (3, None) };
        let alt = crate::equivocation::SignedRun { proof_id: proof_id(ap), frames, sig: asig };
        match judge_sibling(&seed.0, &key, &main, &alt) {
            Sibling::SameRun => (0, None),
            Sibling::BadSignature => (3, None),
            Sibling::Equivocation(e) => {
                let new = crate::equivocation::policy().lock().unwrap().add(e);
                (if new { 4 } else { 5 }, Some(e.encode()))
            }
        }
    });
    match r {
        Ok((code, ev)) => {
            if let Some(ev) = ev {
                copy_out(&ev, out, cap);
            }
            code
        }
        Err(_) => -1,
    }
}

/// Evidence received from a peer (`NOTIFY_SM64_EVIDENCE`, self-contained:
/// equivocation.rs `offer_wire`) from `host`, our top block at height `top`:
/// 1 new and valid (relay it), 0 not (known, or over the peer's rate), -1
/// forged or malformed (an honest node never relays that: drop the peer).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_evidence_received(bytes: *const u8, len: usize, host: *const c_char, top: u64) -> i32 {
    let b = blob_arg(bytes, len);
    let h = str_arg(host);
    std::panic::catch_unwind(|| {
        // Per peer: a burst of 16 pieces, then 8 a minute (evidence is rare: one per equivocating template).
        static RATE: std::sync::OnceLock<std::sync::Mutex<crate::admit::Bounded<String, crate::admit::Bucket>>> = std::sync::OnceLock::new();
        let t = crate::admit::now();
        let ok = RATE.get_or_init(|| std::sync::Mutex::new(crate::admit::Bounded::new(4096))).lock().unwrap().entry_or(h, || crate::admit::Bucket::new(16.0, 8.0, t)).take(1.0, t);
        if !ok {
            return 0;
        }
        match crate::equivocation::policy().lock().unwrap().offer_wire(b, top) {
            crate::equivocation::Offer::New => 1,
            crate::equivocation::Offer::Known | crate::equivocation::Offer::Pending => 0,
            crate::equivocation::Offer::Invalid => -1,
        }
    })
    .unwrap_or(0)
}

/// The next new evidence (found here or received) to relay to peers, on the
/// wire, into `out` (if `cap` allows; at most sm64pow_evidence_max() bytes);
/// returns its length, 0 when there is none.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_take_evidence(out: *mut u8, cap: usize) -> usize {
    match std::panic::catch_unwind(|| crate::equivocation::policy().lock().unwrap().take_outbox()) {
        Ok(Some(e)) => copy_out(&e, out, cap),
        _ => 0,
    }
}

/// The largest evidence message.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_evidence_max() -> usize {
    crate::equivocation::MAX_WIRE
}

/// The evidence for template `hash32` (sent along with its block), its length; 0 none.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_evidence_for(hash32: *const u8, out: *mut u8, cap: usize) -> usize {
    let Some(seed) = (unsafe { id_arg(hash32) }) else { return 0 };
    match std::panic::catch_unwind(|| crate::equivocation::policy().lock().unwrap().wire_for(&seed)) {
        Ok(Some(e)) => copy_out(&e, out, cap),
        _ => 0,
    }
}

/// The `index`-th piece of recent evidence (accused block within
/// equivocation::RECENT_HEIGHTS of `top`), for a newly connected peer; 0: no more.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_evidence_recent(top: u64, index: usize, out: *mut u8, cap: usize) -> usize {
    match std::panic::catch_unwind(|| crate::equivocation::policy().lock().unwrap().recent_nth(top, index)) {
        Ok(Some(e)) => copy_out(&e, out, cap),
        _ => 0,
    }
}

/// Keeps recent evidence in the file `path` across restarts (loads it now).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_evidence_persist(path: *const c_char) {
    let p = str_arg(path);
    let _ = std::panic::catch_unwind(|| crate::equivocation::policy().lock().unwrap().persist(p.into()));
}

/// Until sm64pow_block_scope_leave: the block this thread handles (its blob;
/// our top block's height), so evidence formed about its template carries it.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_block_scope_enter(blob: *const u8, len: usize, hash32: *const u8, height: u64, top: u64) -> *mut std::ffi::c_void {
    let b = blob_arg(blob, len);
    let Some(seed) = (unsafe { id_arg(hash32) }) else { return std::ptr::null_mut() };
    match std::panic::catch_unwind(|| crate::equivocation::block_scope(b, seed, height, top)) {
        Ok(g) => Box::into_raw(Box::new(g)) as *mut std::ffi::c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_block_scope_leave(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut crate::equivocation::BlockScope) });
    }
}

/// Fork choice with the equivocation penalty (equivocation.rs `fork_choice`):
/// the chains above their fork point, our main chain (`main_len` blocks, the
/// seeds of up to `n_main` of them) and the alternative (`alt_len`, `n_alt`
/// seeds). 1: switch to the alternative.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_fork_choice(main_len: u64, main_seeds: *const u8, n_main: usize, alt_len: u64, alt_seeds: *const u8, n_alt: usize) -> i32 {
    let seeds = |p: *const u8, n: usize| -> Vec<[u8; 32]> {
        if p.is_null() || n == 0 { vec![] } else { unsafe { std::slice::from_raw_parts(p, 32 * n) }.chunks(32).map(|c| c.try_into().unwrap()).collect() }
    };
    let (m, a) = (seeds(main_seeds, n_main), seeds(alt_seeds, n_alt));
    std::panic::catch_unwind(|| crate::equivocation::policy().lock().unwrap().fork_choice(main_len, &m, alt_len, &a)).unwrap_or(alt_len > main_len) as i32
}

/// A block of our chain at startup (restart state): it counts as eligible at
/// its own stamp for the displacement window (pof.rs), so a sibling of the
/// tip found within the window is still judged after a restart.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_note_history(id32: *const u8, parent32: *const u8, hash32: *const u8, frames: u32, timestamp: u64, sig64: *const u8, proof: *const u8, len: usize) {
    let (Some(id), Some(parent), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(parent32) }, unsafe { id_arg(hash32) }) else { return };
    let tie = match (unsafe { id_arg(hash32) }, sig_arg(sig64)) {
        (Some(sd), Some(sg)) => std::panic::catch_unwind(|| crate::pof::tie_of(blob_arg(proof, len), &sd, &sg)).unwrap_or(crate::pof::NO_TIE),
        _ => crate::pof::NO_TIE,
    };
    let at = timestamp.min(unix_now());
    let _ = std::panic::catch_unwind(|| crate::pof::policy().lock().unwrap().note_eligible_tie(id, parent, seed, frames, timestamp, at, tie));
}

// ---- Publish-on-find (pof.rs): the displacement window, the freshness bound ----

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Block `id32` (parent `parent32`, seed `hash32`, `frames`, stamped
/// `timestamp`) is eligible now: valid and fitting by this node's clock.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_note_eligible(id32: *const u8, parent32: *const u8, hash32: *const u8, frames: u32, timestamp: u64, sig64: *const u8, proof: *const u8, len: usize) {
    let tie = match (unsafe { id_arg(hash32) }, sig_arg(sig64)) {
        (Some(sd), Some(sg)) => std::panic::catch_unwind(|| crate::pof::tie_of(blob_arg(proof, len), &sd, &sg)).unwrap_or(crate::pof::NO_TIE),
        _ => crate::pof::NO_TIE,
    };
    let (Some(id), Some(parent), Some(seed)) = (unsafe { id_arg(id32) }, unsafe { id_arg(parent32) }, unsafe { id_arg(hash32) }) else { return };
    // A block that came in a sync span (we asked for it: it is history) counts as eligible at its own
    // stamp: neither stale nor opening a window now. Otherwise a node that just synced (or restarted)
    // would treat its tip as stale and let any fresh sibling a peer sends it displace the tip.
    let synced = matches!(crate::admit::context().map(|c| c.source), Some(crate::admit::Source::Sync { .. }));
    let at = if synced { timestamp.min(unix_now()) } else { eligible_at(&id) };
    let _ = std::panic::catch_unwind(|| crate::pof::policy().lock().unwrap().note_eligible_tie(id, parent, seed, frames, timestamp, at, tie));
}

/// Whether eligible alternative block `alt32` replaces our childless block
/// `tip32` at the same height (1) or not (0).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_displaces(tip32: *const u8, alt32: *const u8) -> i32 {
    let (Some(t), Some(a)) = (unsafe { id_arg(tip32) }, unsafe { id_arg(alt32) }) else { return 0 };
    std::panic::catch_unwind(|| crate::pof::policy().lock().unwrap().displaces(&t, &a)).unwrap_or(false) as i32
}

/// Whether a sibling of our childless tip `tip32` claiming `frames`, stamped
/// `timestamp`, could displace it (1: worth a replay).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pof_candidate(tip32: *const u8, id32: *const u8, hash32: *const u8, frames: u32, timestamp: u64, sig64: *const u8, proof: *const u8, len: usize) -> i32 {
    let tie = match (unsafe { id_arg(hash32) }, sig_arg(sig64)) {
        (Some(sd), Some(sg)) => std::panic::catch_unwind(|| crate::pof::tie_of(blob_arg(proof, len), &sd, &sg)).unwrap_or(crate::pof::NO_TIE),
        _ => crate::pof::NO_TIE,
    };
    let (Some(t), Some(id), Some(seed)) = (unsafe { id_arg(tip32) }, unsafe { id_arg(id32) }, unsafe { id_arg(hash32) }) else { return 0 };
    // Judged at the time block `id` became eligible here (a claim the queue replayed late still counts).
    let at = eligible_at(&id);
    std::panic::catch_unwind(|| crate::pof::sibling_could_matter(&t, &seed, frames, &tie, timestamp, at)).unwrap_or(false) as i32
}

/// Whether a block stamped `timestamp` arriving now is stale (the freshness
/// bound: not relayed).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_stale(timestamp: u64) -> i32 {
    crate::pof::stale(timestamp, unix_now()) as i32
}

/// When block `id` became eligible at this node: for a claim that went
/// through the replay queue, `max(arrival, due)` (not the later moment a
/// worker got to it: a slow node must not judge honest blocks stale, or open
/// the window late), else now.
fn eligible_at(id: &[u8; 32]) -> u64 {
    let now = unix_now();
    crate::queue::global().eligible_at(id).map_or(now, |t| t.min(now))
}

/// Whether block `id`, a copy of our block `tip`'s run (same seed, another
/// stamp), is the preferred copy (earliest stamp, then lowest id; inside the
/// window: pof.rs `restamp_preferred`): worth a (cached) replay.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_restamp_preferred(tip32: *const u8, hash32: *const u8, timestamp: u64, id32: *const u8) -> i32 {
    let (Some(t), Some(seed), Some(id)) = (unsafe { id_arg(tip32) }, unsafe { id_arg(hash32) }, unsafe { id_arg(id32) }) else { return 0 };
    let at = eligible_at(&id);
    std::panic::catch_unwind(|| crate::pof::restamp_preferred(&t, &seed, timestamp, &id, at)).unwrap_or(false) as i32
}

/// Restricted RPC: how many seconds ahead of its arrival a submitted run may
/// be due (rules.rs `RPC_SUBMIT_AHEAD_SECONDS`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_submit_ahead() -> u64 {
    crate::rules::RPC_SUBMIT_AHEAD_SECONDS
}

/// The freshness bound for block `id` (pof.rs), judged at the time it became eligible here.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_stale_block(id32: *const u8, timestamp: u64) -> i32 {
    let Some(id) = (unsafe { id_arg(id32) }) else { return 0 };
    if matches!(crate::admit::context().map(|c| c.source), Some(crate::admit::Source::Sync { .. })) {
        return 0; // history we asked for: never "stale" (and not relayed anyway: sync is not relayed)
    }
    std::panic::catch_unwind(|| crate::pof::stale(timestamp, eligible_at(&id))).unwrap_or(false) as i32
}

/// Where a new template should build when our tip (seed `tip_seed32`) is
/// childless: -1 on the tip; `i` on the `i`-th of the `n` blocks at the tip's
/// height (their seeds in `sibling_seeds`, 32 bytes each) because the tip is
/// equivocated and that one is clean; `n` on the tip's parent.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_choose_parent(tip_seed32: *const u8, sibling_seeds: *const u8, n: usize) -> i64 {
    let Some(tip) = (unsafe { id_arg(tip_seed32) }) else { return -1 };
    let sib: Vec<[u8; 32]> = if n == 0 || sibling_seeds.is_null() {
        vec![]
    } else {
        unsafe { std::slice::from_raw_parts(sibling_seeds, 32 * n) }.chunks(32).map(|c| c.try_into().unwrap()).collect()
    };
    let p = crate::equivocation::policy().lock().unwrap();
    match crate::equivocation::choose_parent(&p, &tip, &sib) {
        None => -1,
        Some(usize::MAX) => n as i64,
        Some(i) => i as i64,
    }
}

/// The parallel pre-pass of `n` blocks in chain order (a sync batch, see
/// chain::preverify): `ids`, `parents` (`n` x 32 bytes; a parent of all zero
/// bytes is the genesis block), `seeds` (`n` x 32), the proofs concatenated in
/// `blobs` with `offsets` (`n + 1` entries). Returns how many runs it replayed.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_preverify(n: usize, ids: *const u8, parents: *const u8, seeds: *const u8, blobs: *const u8, offsets: *const u64, gaps: *const u64, threads: u32) -> u64 {
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
        // `gaps` (may be null): each block's timestamp gap to its parent, u64::MAX unknown; it bounds the frames worth a replay.
        let gap = if gaps.is_null() { u64::MAX } else { unsafe { *gaps.add(i) } };
        let parent = if p == [0u8; 32] { crate::chain::Parent::Genesis } else { crate::chain::Parent::Block { id: p, gap_seconds: gap } };
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

/// The kind of source this thread is in (1 relay, 2 sync, 3 restricted RPC), 0 none.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_source_kind() -> i32 {
    use crate::admit::Source;
    match crate::admit::context().map(|c| c.source) {
        Some(Source::Relay) => 1,
        Some(Source::Sync { .. }) => 2,
        Some(Source::RpcSubmit) => 3,
        None => 0,
    }
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

// ---- The replay queue (queue.rs, docs/DOS.md) ----

/// What the daemon says about the blocks at a job's height (queue.rs
/// `Chain`): given a parent id, 0 the parent is not in our main chain, 1 it is
/// our tip, 2 it has a main-chain child, whose id and frames go to the out
/// parameters. Called from the queue's worker threads, never under the
/// queue's lock.
pub type FactsCb = extern "C" fn(parent32: *const u8, child_id32: *mut u8, child_frames: *mut u32) -> i32;

/// Sets the callback the queue asks what is at a job's height. Call once at start-up.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_set_facts(cb: FactsCb) {
    use crate::queue::Chain;
    let f = move |p: &[u8; 32]| -> Chain {
        let (mut id, mut frames) = ([0u8; 32], 0u32);
        match cb(p.as_ptr(), id.as_mut_ptr(), &mut frames) {
            1 => Chain::Tip,
            2 => Chain::Child { id, frames },
            _ => Chain::Unknown,
        }
    };
    crate::queue::global().set_facts(std::sync::Arc::new(f));
}

/// How many replay workers (default: every core). Before the first job.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_set_threads(n: u32) {
    crate::queue::global().set_threads(n as usize);
}

#[allow(clippy::too_many_arguments)]
fn queue_job(id32: *const u8, parent32: *const u8, genesis_parent: i32, gap_seconds: u64, hash32: *const u8, difficulty: u64, proof: *const u8, len: usize, frames: u32, stamp: u64, due: u64, sig64: *const u8) -> Option<crate::queue::Job> {
    let tie = match (unsafe { id_arg(hash32) }, sig_arg(sig64)) {
        (Some(sd), Some(sg)) => std::panic::catch_unwind(|| crate::pof::tie_of(blob_arg(proof, len), &sd, &sg)).unwrap_or(crate::pof::NO_TIE),
        _ => crate::pof::NO_TIE,
    };
    let (id, parent_id, seed) = unsafe { (id_arg(id32)?, id_arg(parent32)?, id_arg(hash32)?) };
    let parent = if genesis_parent != 0 { crate::chain::Parent::Genesis } else { crate::chain::Parent::Block { id: parent_id, gap_seconds } };
    Some(crate::queue::Job { id, parent, parent_id, seed: Seed(seed), difficulty, blob: blob_arg(proof, len).to_vec(), frames, tie, stamp, due, arrived: unix_now(), ctx: crate::admit::context() })
}

fn verdict_code(v: &crate::queue::Verdict) -> i32 {
    use crate::queue::Verdict::*;
    match v {
        Valid(_) => {
            1
        }
        Invalid(why) => {
            set_error(why.clone());
            0
        }
        Skipped(why) => {
            set_error((*why).into());
            2
        }
        Process => 3,
        BadEncoding(why) => {
            set_error(why.clone());
            4
        }
    }
}

/// Queues a claim: block `id32` on `parent32` (`genesis_parent` 1 when that
/// is the genesis block), `gap_seconds` after it, seed `hash32`, retarget
/// `difficulty`, the proof, its claimed `frames`, the block's `stamp`, and the
/// unix second it is `due`. The source is this thread's (sm64pow_source_enter).
/// 0 queued, 1 already known, 2 the queue (or this source's share) is full, 3
/// this source sent an invalid claim recently (reason in sm64pow_last_error).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_submit(id32: *const u8, parent32: *const u8, genesis_parent: i32, gap_seconds: u64, hash32: *const u8, difficulty: u64, proof: *const u8, len: usize, frames: u32, stamp: u64, due: u64, sig64: *const u8) -> i32 {
    use crate::queue::Refused;
    let Some(job) = queue_job(id32, parent32, genesis_parent, gap_seconds, hash32, difficulty, proof, len, frames, stamp, due, sig64) else { return 2 };
    match crate::queue::global().submit(job) {
        Ok(()) => 0,
        Err(Refused::Known) => 1,
        Err(Refused::Full) => {
            set_error("the replay queue is full".into());
            2
        }
        Err(Refused::Penalty) => {
            set_error("this source sent an invalid claim recently".into());
            3
        }
        Err(Refused::Budget(why)) => {
            set_error(why.into());
            2
        }
        Err(Refused::Hopeless(why)) => {
            set_error(why.into());
            2
        }
    }
}

/// Replays a claim now on the calling thread (the local miner's
/// `submit_block`, outside any lock): the decision and replay of a queued
/// job. 1 valid (its end state is cached; [`sm64pow_queue_take_cleared`] then
/// says so), 0 invalid, 2 skipped (it cannot matter), 3 not decided here
/// (process it the normal way).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_replay_now(id32: *const u8, parent32: *const u8, genesis_parent: i32, gap_seconds: u64, hash32: *const u8, difficulty: u64, proof: *const u8, len: usize, frames: u32, stamp: u64, sig64: *const u8) -> i32 {
    let Some(job) = queue_job(id32, parent32, genesis_parent, gap_seconds, hash32, difficulty, proof, len, frames, stamp, 0, sig64) else { return 3 };
    match std::panic::catch_unwind(|| crate::queue::global().replay_now(job)) {
        Ok(v) => verdict_code(&v),
        Err(_) => 3,
    }
}

/// The next finished claim, waiting up to `timeout_ms`: writes its id to
/// `out_id32` and returns its verdict (1 valid, 0 invalid, 2 skipped, 3 not
/// decided here: process it normally; reason in sm64pow_last_error), or -1
/// when none finished.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_wait(timeout_ms: u32, out_id32: *mut u8) -> i32 {
    match crate::queue::global().wait_ready(std::time::Duration::from_millis(timeout_ms as u64)) {
        Some((id, v)) => {
            unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), out_id32, 32) };
            verdict_code(&v)
        }
        None => -1,
    }
}

/// Whether block `id32` was replayed valid by the queue and not yet
/// consumed: then the daemon's normal path finds the cached result. Consumes it.
// ---- Reorganisation cap (reorg.rs), restricted RPC's bulk budget (admit.rs) ----

/// The operator's reorganisation cap in blocks (0: unlimited; default 36).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_set_max_reorg(n: u64) {
    crate::reorg::set_max(n);
}

/// The operator allows one reorganisation onto a chain holding block `id32`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_allow_reorg(id32: *const u8) {
    if let Some(id) = unsafe { id_arg(id32) } {
        crate::reorg::allow(id);
    }
}

/// May the node pop `depth` blocks to switch to an alternative chain with the `n` block ids `alt_ids`?
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_reorg_allowed(depth: u64, alt_ids: *const u8, n: usize) -> i32 {
    let ids: Vec<[u8; 32]> = if alt_ids.is_null() || n == 0 { vec![] } else { unsafe { std::slice::from_raw_parts(alt_ids, 32 * n) }.chunks(32).map(|c| c.try_into().unwrap()).collect() };
    std::panic::catch_unwind(|| crate::reorg::allowed_reorg(depth, &ids)).unwrap_or(false) as i32
}

/// Restricted RPC's bulk answers (get_sm64_state / get_sm64_cycle): 1 may answer now; charge the bytes after.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_bulk_ok() -> i32 {
    std::panic::catch_unwind(crate::admit::rpc_bulk_ok).unwrap_or(true) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_rpc_bulk_charge(bytes: u64) {
    let _ = std::panic::catch_unwind(|| crate::admit::rpc_bulk_charge(bytes));
}

/// Claim `id`, already queued, was sent again by this thread's source: a fallback source for it.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_also(id32: *const u8) {
    let Some(id) = (unsafe { id_arg(id32) }) else { return };
    let _ = std::panic::catch_unwind(|| crate::queue::global().also(&id, crate::admit::context()));
}

#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_queue_take_cleared(id32: *const u8) -> i32 {
    let Some(id) = (unsafe { id_arg(id32) }) else { return 0 };
    crate::queue::global().take_cleared(&id) as i32
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
    let res = std::panic::catch_unwind(|| crate::chain::played_any(&id, parent, &Seed(seed), bytes));
    match res {
        Ok(Ok(Ok(pl))) => copy_out(&crate::proof::Raw.encode(&pl.pads), out, cap),
        Ok(Ok(Err(crate::chain::CommittedVerdict::Run(crate::chain::BlockVerdict::NeedParent)))) => {
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
    let res = std::panic::catch_unwind(|| crate::chain::played_any(&id, parent, &Seed(seed), bytes));
    match res {
        Ok(Ok(Ok(pl))) => copy_out(&crate::proof::Raw.encode(&pl.wanted()), out, cap),
        Ok(Ok(Err(crate::chain::CommittedVerdict::Run(crate::chain::BlockVerdict::NeedParent)))) => {
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

// ---- v0.4 feature switches (features.rs) ----

/// Sets the v0.4 consensus features for this process (start-up, once per network): bit 0
/// committed inputs, 1 included proofs, 2 proof weight, 3 proof rewards. Returns 0, or -1 when
/// the dependencies don't hold (nothing changes).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_set_features(bits: u32) -> i32 {
    let f = crate::features::Features { committed_inputs: bits & 1 != 0, included_proofs: bits & 2 != 0, proof_weight: bits & 4 != 0, proof_rewards: bits & 8 != 0 };
    match crate::features::set(f) {
        Ok(()) => 0,
        Err(e) => {
            set_error(e.into());
            -1
        }
    }
}

/// The final key of the equal-length tie-break (pof::TieKey, node policy): 0 the signature's
/// hash (v0.3, the default), 1 the seed's hash, 2 none (first seen). Unknown values: the default.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_set_tie_key(k: u32) {
    crate::pof::set_tie_key(match k {
        1 => crate::pof::TieKey::SeedHash,
        2 => crate::pof::TieKey::FirstSeen,
        _ => crate::pof::TieKey::SigHash,
    });
}

// ---- Proof blobs (proof.rs, proof_id.rs) and the seed ----

/// What a block id commits to for this proof blob (32 bytes into `out32`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_id(blob: *const u8, len: usize, out32: *mut u8) {
    let bytes: &[u8] = if blob.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(blob, len) } };
    let id = std::panic::catch_unwind(|| proof_id(bytes)).unwrap_or([0xff; 32]);
    unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), out32, 32) };
}

/// A proof's start (its committed header): 0 from the parent's end state, 1 from power-on, -1 if
/// the header doesn't parse.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_start(proof: *const u8, len: usize) -> i32 {
    let b = blob_arg(proof, len);
    if crate::features::committed() { crate::commit::header(b).map_or(-1, |h| h.start as i32) } else { crate::proof::proof_start(b).map_or(-1, |s| s as i32) }
}

/// How many frames a proof's run lasted: its committed header (0 if it doesn't parse).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_frames(blob: *const u8, len: usize) -> u32 {
    let b = blob_arg(blob, len);
    if crate::features::committed() {
        return crate::commit::header(b).map_or(0, |h| h.frames);
    }
    crate::proof::decode_proof(b, rules().max_frames_any() as usize).map_or_else(|_| crate::proof::split_blob(b).map_or(0, |x| if crate::statecodec::state_codec(x.2).is_some() { x.0 } else { 0 }), |pr| pr.frames)
}

fn stateless_run(blob: &[u8]) -> Result<crate::proof::Proof, String> {
    if !crate::features::committed() {
        return crate::proof::decode_proof(blob, rules().max_frames_any() as usize).map_err(|e| e.to_string());
    }
    match crate::commit::open(blob, rules().max_frames_any(), false) {
        Ok(crate::commit::Opened::Run(_, pr)) => Ok(pr),
        Ok(crate::commit::Opened::State(..)) => Err("state-coded".into()),
        Err(crate::commit::Fault::Invalid(e) | crate::commit::Fault::BadEncoding(e)) => Err(e),
    }
}

/// Decodes a committed proof blob in a stateless codec to its inputs (the non-poked frames'
/// pads, 4 bytes each), checked against the committed hash. Returns the byte length (call with
/// `out` NULL to size the buffer; nothing is written unless `cap` is big enough), or `usize::MAX`
/// if the blob does not decode (or is state-coded: that needs the replay, sm64pow_wanted_block).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_decode_raw(blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    match stateless_run(blob_arg(blob, len)) {
        Ok(pr) => copy_out(&crate::proof::Raw.encode(&pr.inputs), out, cap),
        Err(e) => {
            set_error(e);
            usize::MAX
        }
    }
}

/// Re-encodes a committed proof blob in a stateless codec with the smallest stateless codec (the
/// header, hence the block id, does not change). Same return convention as `sm64pow_decode_raw`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_reencode(blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    match stateless_run(blob_arg(blob, len)) {
        Ok(pr) => copy_out(&if crate::features::committed() { crate::commit::commit(&pr) } else { crate::proof::encode_proof(&pr) }, out, cap),
        Err(e) => {
            set_error(e);
            usize::MAX
        }
    }
}

// ---- Codec negotiation (codecs.rs, v0.4 node policy) ----

fn mask_arg(mask32: *const u8) -> crate::codecs::Mask {
    if mask32.is_null() {
        return crate::codecs::Mask::baseline();
    }
    crate::codecs::Mask(unsafe { *(mask32 as *const [u8; 32]) })
}

/// This node's codec advert (NOTIFY_SM64_CODECS: `(id, version)` pairs). Same return convention
/// as `sm64pow_decode_raw`.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_codecs_advert(out: *mut u8, cap: usize) -> usize {
    copy_out(&crate::codecs::advert(), out, cap)
}

/// A peer's codec set (32 bytes, one bit per id) from its advert (`len` 0: the baseline).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_codecs_mask(advert: *const u8, len: usize, out32: *mut u8) {
    let m = crate::codecs::peer_mask(blob_arg(advert, len));
    unsafe { std::ptr::copy_nonoverlapping(m.0.as_ptr(), out32, 32) };
}

/// What sending `blob` to a peer with set `mask32` (NULL: the baseline) needs: 0 nothing, 1 a
/// stateless transcode, 2 the run's inputs first (state-coded; `sm64pow_wanted_block`).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_codecs_need(blob: *const u8, len: usize, mask32: *const u8) -> i32 {
    crate::codecs::need(blob_arg(blob, len), &mask_arg(mask32)) as i32
}

/// The codec id of a committed proof blob, -1 if it has none.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_proof_codec(blob: *const u8, len: usize) -> i32 {
    let b = blob_arg(blob, len);
    if crate::features::committed() {
        crate::codecs::codec_of(b).map_or(-1, |c| c as i32)
    } else {
        crate::proof::split_blob(b).map_or(-1, |x| x.2 as i32)
    }
}

/// `blob` re-encoded for a peer with set `mask32` (the same header, so the same block id), in the
/// smallest stateless codec it decodes; `inputs` (4 bytes per non-poked frame, may be NULL) for a
/// state-coded blob. Same return convention as `sm64pow_decode_raw`; 0 bytes: send it as is.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_transcode(blob: *const u8, len: usize, inputs: *const u8, inputs_len: usize, mask32: *const u8, out: *mut u8, cap: usize) -> usize {
    let ins = if inputs.is_null() { None } else { Some(blob_arg(inputs, inputs_len)) };
    match crate::codecs::transcode(blob_arg(blob, len), ins, &mask_arg(mask32)) {
        Ok(None) => 0,
        Ok(Some(b)) => copy_out(&b, out, cap),
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

// ---- Included proofs (v0.4 stage B: included.rs, pool.rs) ----

/// Judges a block's `sm64_proofs` field against its parent block h (docs/V0.4-DESIGN.md section
/// 2.4). 1 valid, 0 invalid (reason in sm64pow_last_error), -3 bad witness (never an invalid
/// block), -2 B_{h-1}'s end state isn't cached, -1 internal error. `counted`: records that count
/// for weight (0 while proof_weight is off). `normalized` (may be NULL): the field with every
/// stateless proof in its smallest encoding (the length is always written; the bytes only when
/// `cap` is enough).
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_verify_records(
    field: *const u8, field_len: usize, nonce: *const u8, nonce_len: usize, h_height: u64, h_prev32: *const u8, h_prev_is_genesis: i32, h_nonce: *const u8, h_nonce_len: usize,
    h_proof: *const u8, h_proof_len: usize, h_difficulty: u64, threads: u32, counted: *mut u32, normalized: *mut u8, cap: usize, normalized_len: *mut usize,
) -> i32 {
    use crate::included::{BlockRecords, Parent1, Real, verify_block_records};
    let Some(prev_id) = (unsafe { id_arg(h_prev32) }) else {
        set_error("null parent id".into());
        return -1;
    };
    let h = Parent1 { height: h_height, prev_id, prev_is_genesis: h_prev_is_genesis != 0, nonce: blob_arg(h_nonce, h_nonce_len), proof: blob_arg(h_proof, h_proof_len), difficulty: h_difficulty };
    let (field, nonce) = (blob_arg(field, field_len), blob_arg(nonce, nonce_len));
    match std::panic::catch_unwind(|| verify_block_records(field, nonce, &h, threads as usize, &Real)) {
        Ok(Ok(BlockRecords::Valid { counted: c, normalized: n })) => {
            if !counted.is_null() {
                unsafe { *counted = c };
            }
            if !normalized_len.is_null() {
                unsafe { *normalized_len = n.len() };
            }
            if !normalized.is_null() {
                copy_out(&n, normalized, cap);
            }
            1
        }
        Ok(Ok(BlockRecords::Invalid(why))) => {
            set_error(why);
            0
        }
        Ok(Ok(BlockRecords::BadWitness(why))) => {
            set_error(format!("bad witness: {why}"));
            -3
        }
        Ok(Ok(BlockRecords::NeedParent)) => {
            set_error("the parent's end state isn't cached".into());
            -2
        }
        Ok(Err(e)) => {
            set_error(format!("{e:#}"));
            -1
        }
        Err(_) => {
            set_error("panic while verifying proof records".into());
            -1
        }
    }
}

/// Our new tip is block h (id, height, its parent B_{h-1}, its key nonce and committed proof, its
/// retarget difficulty): the pool collects proofs for height h. The last few tips' pools are kept.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pool_tip(h_id32: *const u8, h_height: u64, h_prev32: *const u8, h_prev_is_genesis: i32, h_nonce: *const u8, h_nonce_len: usize, h_proof: *const u8, h_proof_len: usize, h_difficulty: u64) {
    let (Some(id), Some(prev_id)) = (unsafe { id_arg(h_id32) }, unsafe { id_arg(h_prev32) }) else {
        set_error("null block id".into());
        return;
    };
    let h = crate::included::Parent1 { height: h_height, prev_id, prev_is_genesis: h_prev_is_genesis != 0, nonce: blob_arg(h_nonce, h_nonce_len), proof: blob_arg(h_proof, h_proof_len), difficulty: h_difficulty };
    match std::panic::catch_unwind(|| crate::pool::global().lock().unwrap_or_else(|e| e.into_inner()).tip(id, &h)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => set_error(e),
        Err(_) => set_error("panic in sm64pow_pool_tip".into()),
    }
}

/// Offers one record (records.rs wire format, no count prefix) for our current tip: 1 new and valid
/// (queued for relay), 0 known / not for our tip / outside the window / over budget (ignore), -1
/// invalid (drop the sender), -3 bad witness (drop the sender), -2 the parent's state isn't cached.
/// Replays on this thread. Always 0 with included_proofs off.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pool_offer(record: *const u8, len: usize) -> i32 {
    use crate::pool::Offer;
    let b = blob_arg(record, len);
    match std::panic::catch_unwind(|| crate::pool::offer(b)) {
        Ok(Ok(Offer::New)) => 1,
        Ok(Ok(Offer::Ignored)) => 0,
        Ok(Ok(Offer::Invalid(why))) => {
            set_error(why);
            -1
        }
        Ok(Ok(Offer::BadWitness(why))) => {
            set_error(format!("bad witness: {why}"));
            -3
        }
        Ok(Ok(Offer::NeedParent)) => {
            set_error("the parent's end state isn't cached".into());
            -2
        }
        Ok(Err(e)) => {
            set_error(format!("{e:#}"));
            -1
        }
        Err(_) => {
            set_error("panic in sm64pow_pool_offer".into());
            -1
        }
    }
}

/// The next new record to gossip (NOTIFY_SM64_PROOF), its proof in the smallest stateless encoding.
/// Size-then-fill: returns its length and consumes it only when `cap` was enough (call with
/// `out` NULL to size); 0: none.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pool_take_relay(out: *mut u8, cap: usize) -> usize {
    std::panic::catch_unwind(|| {
        let mut p = crate::pool::global().lock().unwrap_or_else(|e| e.into_inner());
        let Some(b) = p.peek_relay() else { return 0 };
        let n = b.len();
        if !out.is_null() && cap >= n {
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), out, n) };
            p.pop_relay();
        }
        n
    })
    .unwrap_or(0)
}

/// The `sm64_proofs` field for a template on top of `tip_id32`: the best records for the tip's
/// height in canonical order, proofs stateless. Its root goes to `root_out32` (zero when none).
/// Returns the field's length (size-then-fill; 0: no records, or included_proofs off). The selection
/// is remembered by root.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pool_select(tip_id32: *const u8, out: *mut u8, cap: usize, root_out32: *mut u8) -> usize {
    let Some(id) = (unsafe { id_arg(tip_id32) }) else { return 0 };
    let sel = std::panic::catch_unwind(|| crate::pool::global().lock().unwrap_or_else(|e| e.into_inner()).select(&id)).unwrap_or(None);
    let (field, root) = sel.unwrap_or((vec![], [0; 32]));
    if !root_out32.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(root.as_ptr(), root_out32, 32) };
    }
    copy_out(&field, out, cap)
}

/// The field a selection with this root had (the daemon reattaches it at submit_block); 0: unknown.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_pool_field_for_root(root32: *const u8, out: *mut u8, cap: usize) -> usize {
    let Some(root) = (unsafe { id_arg(root32) }) else { return 0 };
    let f = std::panic::catch_unwind(|| crate::pool::global().lock().unwrap_or_else(|e| e.into_inner()).field_for_root(&root).map(|f| f.to_vec())).unwrap_or(None);
    copy_out(&f.unwrap_or_default(), out, cap)
}

/// A sealed block (template, committed proof, signature, and an optional trailing `sm64_proofs`
/// string) as a proof record (records.rs wire format, no count prefix). SIZE_MAX if it doesn't parse.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_record_from_block(block_blob: *const u8, len: usize, out: *mut u8, cap: usize) -> usize {
    match std::panic::catch_unwind(|| crate::pool::record_from_block(blob_arg(block_blob, len))) {
        Ok(Ok(r)) => {
            let mut b = Vec::new();
            r.encode_into(&mut b);
            copy_out(&b, out, cap)
        }
        Ok(Err(e)) => {
            set_error(e.to_string());
            usize::MAX
        }
        Err(_) => {
            set_error("panic in sm64pow_record_from_block".into());
            usize::MAX
        }
    }
}

/// The v0.4 extra nonce: the miner's `0x64 || key` nonce (anything after the key is replaced), the
/// payout TLV (`spend32`/`view32` NULL: none; `subaddress` 0/1) and the root TLV (`root32` NULL or
/// all zero: none). Size-then-fill; SIZE_MAX if the miner's nonce has no key, a payout key is not a
/// valid point, or the result is over Monero's 255 bytes.
#[unsafe(no_mangle)]
pub extern "C" fn sm64pow_template_nonce(miner_nonce: *const u8, miner_nonce_len: usize, subaddress: i32, spend32: *const u8, view32: *const u8, root32: *const u8, out: *mut u8, cap: usize) -> usize {
    let n = blob_arg(miner_nonce, miner_nonce_len);
    if n.len() < crate::sign::KEY_NONCE_LEN || n[0] != crate::sign::KEY_TAG {
        set_error("the miner's nonce has no key".into());
        return usize::MAX;
    }
    let key: [u8; 32] = n[1..crate::sign::KEY_NONCE_LEN].try_into().unwrap();
    let payout = match (unsafe { id_arg(spend32) }, unsafe { id_arg(view32) }) {
        (Some(spend), Some(view)) => Some(crate::records::Payout { subaddress: subaddress != 0, spend, view }),
        _ => None,
    };
    let root = unsafe { id_arg(root32) }.filter(|r| *r != [0; 32]);
    let nonce = crate::records::template_nonce(&key, payout.as_ref(), root.as_ref());
    if nonce.len() > 255 || crate::records::nonce_fields(&nonce).is_none() {
        set_error("the v0.4 extra nonce is invalid (a payout key that is not a point, or over 255 bytes)".into());
        return usize::MAX;
    }
    copy_out(&nonce, out, cap)
}
