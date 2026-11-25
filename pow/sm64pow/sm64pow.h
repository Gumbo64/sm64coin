/* SM64Coin proof-of-play, implemented in Rust (pow/sm64pow, see src/ffi.rs).
 * All consensus rules live there; the daemon only calls these. */
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#ifdef __cplusplus
extern "C" {
#endif
/* 1 on success. rom_path NULL/empty: $SM64COIN_ROM, ./baserom.us.z64, ~/.sm64coin/baserom.us.z64 */
int32_t sm64pow_init(const char *rom_path);
/* Verify block id32 on top of parent32 (NULL: the genesis block), timestamped
 * gap_seconds after it, at effective difficulty (0: rebuild an accepted block,
 * no time limit). frames (>0) valid, 0 invalid, -2 parent state not cached,
 * -1 internal error; reason in sm64pow_last_error. Caches the end state. */
int64_t sm64pow_verify_block(const uint8_t *id32, const uint8_t *parent32, uint64_t gap_seconds,
                             const uint8_t *hash32, uint64_t difficulty, const uint8_t *proof, size_t len);
/* SM64COIN v0.3, signed proofs (sign.rs): as sm64pow_verify_block, but first
 * checks that the coinbase extra nonce commits a miner key (0x64 || key32) and
 * that sig64 signs (seed, proof_id, frames) with it; no replay if not.
 * difficulty 0 (rebuild) skips the signature. */
int64_t sm64pow_verify_signed_block(const uint8_t *id32, const uint8_t *parent32, uint64_t gap_seconds,
                                    const uint8_t *hash32, uint64_t difficulty, const uint8_t *proof, size_t len,
                                    const uint8_t *nonce, size_t nonce_len, const uint8_t *sig64);
/* 1 the block's signature is valid, 0 not (reason in sm64pow_last_error). */
int32_t sm64pow_check_signature(const uint8_t *hash32, const uint8_t *proof, size_t len,
                                const uint8_t *nonce, size_t nonce_len, const uint8_t *sig64);
/* Equivocation (equivocation.rs, relay policy): a second run for the template of
 * a block in our chain. 0 same run, 3 bad signature, 4 equivocation (any other signed run) (272 bytes of evidence to out; relay), 5 known evidence, -1 error. */
int32_t sm64pow_judge_sibling(const uint8_t *hash32, const uint8_t *nonce, size_t nonce_len,
                              const uint8_t *main_proof, size_t main_len, const uint8_t *main_sig64,
                              const uint8_t *alt_proof, size_t alt_len, const uint8_t *alt_sig64,
                              uint8_t *out, size_t cap);
/* Self-contained evidence (272 bytes + the accused template) from peer `host`, our top block at `top`:
 * 1 new and valid (relay it), 0 not (known, or over the peer's rate), -1 forged or malformed (drop the peer). */
int32_t sm64pow_evidence_received(const uint8_t *bytes, size_t len, const char *host, uint64_t top);
size_t sm64pow_evidence_max(void);
/* The evidence for template hash32 (sent along with its block): its length (copied if cap allows); 0 none. */
size_t sm64pow_evidence_for(const uint8_t *hash32, uint8_t *out, size_t cap);
/* The index-th piece of recent evidence (accused block within 8 heights of top), for a new peer; 0: no more. */
size_t sm64pow_evidence_recent(uint64_t top, size_t index, uint8_t *out, size_t cap);
/* Keeps recent evidence in the file `path` across restarts (loads it now). */
void sm64pow_evidence_persist(const char *path);
/* While entered: the block this thread handles (its blob, our top height), so evidence about it carries its template. */
void *sm64pow_block_scope_enter(const uint8_t *blob, size_t len, const uint8_t *hash32, uint64_t height, uint64_t top);
void sm64pow_block_scope_leave(void *handle);
/* Fork choice with the equivocation penalty: 1 switch to the alternative chain. Seeds: 32 bytes each. */
int32_t sm64pow_fork_choice(uint64_t main_len, const uint8_t *main_seeds, size_t n_main, uint64_t alt_len, const uint8_t *alt_seeds, size_t n_alt);
/* A block of our chain at startup: eligible at its own stamp (the displacement window after a restart). */
void sm64pow_note_history(const uint8_t *id32, const uint8_t *parent32, const uint8_t *hash32, uint32_t frames, uint64_t timestamp, const uint8_t *sig64, const uint8_t *proof, size_t len);
/* SM64COIN v0.3 publish-on-find (pof.rs, relay / fork-choice policy). note_eligible:
 * block id32 became eligible now (valid, fitting by the node's clock). displaces: 1
 * when eligible sibling alt32 replaces our childless tip32 (a strictly shorter run of
 * another key inside the 60 s window, or a fresh block against a stale tip).
 * pof_candidate: 1 when such a sibling is worth a replay. stale: 1 when a block
 * stamped timestamp is stale now (not relayed). */
void sm64pow_note_eligible(const uint8_t *id32, const uint8_t *parent32, const uint8_t *hash32, uint32_t frames, uint64_t timestamp, const uint8_t *sig64, const uint8_t *proof, size_t len); /* sig64, proof: the block's sm64_sig and sm64_proof (the equal-length tie-break: canonical proof size, then the template seed's hash) */
int32_t sm64pow_displaces(const uint8_t *tip32, const uint8_t *alt32);
int32_t sm64pow_pof_candidate(const uint8_t *tip32, const uint8_t *id32, const uint8_t *hash32, uint32_t frames, uint64_t timestamp, const uint8_t *sig64, const uint8_t *proof, size_t len);
int32_t sm64pow_stale(uint64_t timestamp);
/* The freshness bound for block id32, judged when it became eligible here (a queued claim: max(arrival, due)). */
int32_t sm64pow_stale_block(const uint8_t *id32, uint64_t timestamp);
/* A copy of our block tip32's run (same seed, another stamp): 1 if it is the preferred copy
 * (earliest stamp, then lowest id; inside the displacement window). */
int32_t sm64pow_restamp_preferred(const uint8_t *tip32, const uint8_t *hash32, uint64_t timestamp, const uint8_t *id32);
/* Restricted RPC: the most seconds ahead of arrival a submitted run may be due. */
uint64_t sm64pow_rpc_submit_ahead(void);
/* An already queued claim sent again by this thread's source (a fallback if the first source is penalised). */
void sm64pow_queue_also(const uint8_t *id32);
/* Reorganisation cap (reorg.rs): the operator's cap (0 unlimited, default 36), a one-time allowance by block id,
 * and the check: may the node pop `depth` blocks for the alternative chain with these n ids? */
void sm64pow_set_max_reorg(uint64_t n);
void sm64pow_allow_reorg(const uint8_t *id32);
int32_t sm64pow_reorg_allowed(uint64_t depth, const uint8_t *alt_ids, size_t n);
/* Restricted RPC's bulk answers (get_sm64_state, get_sm64_cycle), all clients together: 1 may answer; charge after. */
int32_t sm64pow_rpc_bulk_ok(void);
void sm64pow_rpc_bulk_charge(uint64_t bytes);
/* One run per key (equivocation.rs): 0 bad signature, 1 the template's first run,
 * 2 the same run again, 3 a different run (equivocation; evidence queued for relay), -1 error. */
int32_t sm64pow_note_claim(const uint8_t *hash32, const uint8_t *proof, size_t len,
                           const uint8_t *nonce, size_t nonce_len, const uint8_t *sig64);
/* The replay queue (queue.rs). facts: 0 parent not in our chain, 1 it is the tip,
 * 2 it has a main-chain child (id, frames out). Called from worker threads. */
typedef int32_t (*sm64pow_facts_cb)(const uint8_t *parent32, uint8_t *child_id32, uint32_t *child_frames);
void sm64pow_queue_set_facts(sm64pow_facts_cb cb);
void sm64pow_queue_set_threads(uint32_t n);
/* 0 queued, 1 known, 2 full, 3 source penalised. */
int32_t sm64pow_queue_submit(const uint8_t *id32, const uint8_t *parent32, int32_t genesis_parent, uint64_t gap_seconds,
                             const uint8_t *hash32, uint64_t difficulty, const uint8_t *proof, size_t len,
                             uint32_t frames, uint64_t stamp, uint64_t due, const uint8_t *sig64);
/* 1 valid, 0 invalid, 2 skipped, 3 not decided here. */
int32_t sm64pow_queue_replay_now(const uint8_t *id32, const uint8_t *parent32, int32_t genesis_parent, uint64_t gap_seconds,
                                 const uint8_t *hash32, uint64_t difficulty, const uint8_t *proof, size_t len,
                                 uint32_t frames, uint64_t stamp, const uint8_t *sig64);
int32_t sm64pow_queue_wait(uint32_t timeout_ms, uint8_t *out_id32);
int32_t sm64pow_queue_take_cleared(const uint8_t *id32);
/* The next new evidence to relay to peers (272 bytes to out); 0: none. */
size_t sm64pow_take_evidence(uint8_t *out, size_t cap);
/* Where to build when the tip (tip_seed32) is childless: -1 the tip, i the i-th of
 * the n blocks at its height (sibling_seeds), n its parent. */
int64_t sm64pow_choose_parent(const uint8_t *tip_seed32, const uint8_t *sibling_seeds, size_t n);
int32_t sm64pow_has_state(const uint8_t *id32);
size_t sm64pow_end_state_gz(const uint8_t *id32, uint8_t *out, size_t cap);
int32_t sm64pow_block_progress(const uint8_t *id32, uint32_t *stars, uint32_t *keys, int32_t *grand_star, int32_t *ends_cycle);
size_t sm64pow_block_event(const uint8_t *id32, char *out, size_t cap); /* "c09s1", "key1", "grand"; 0 if unknown */
int32_t sm64pow_proof_start(const uint8_t *proof, size_t len); /* 0 parent, 1 power-on, -1 bad */
/* Whether fork choice counts blocks instead of cumulative difficulty (Rules::weigh_by_blocks). */
bool sm64pow_weigh_by_blocks(void);
/* The hold policy: 0 to process the block now, else the unix second until which to hold it. */
uint64_t sm64pow_hold_until(uint64_t difficulty, uint64_t parent_ts, uint64_t block_ts, uint64_t now, uint32_t frames, uint64_t restart_delay);
/* The poke streams' mix intervals (simulated frames; 0: never mixed). */
void sm64pow_mix(uint64_t difficulty, uint32_t *timing_mix, uint32_t *pad_mix);
/* No block but the genesis block may be timestamped before this (unix seconds). */
uint64_t sm64pow_launch_timestamp(void);
/* Seconds earlier a power-on restart on top of a parent with this progress takes its limit; -1: not allowed. */
int64_t sm64pow_restart_delay(uint32_t stars, uint32_t keys, bool grand_star);
uint32_t sm64pow_max_frames(uint64_t difficulty, uint64_t elapsed_seconds);
uint32_t sm64pow_reseed_frames(uint64_t difficulty);
size_t sm64pow_last_error(char *buf, size_t cap);
uint32_t sm64pow_seed(const uint8_t *hashing_blob_hash32);
void sm64pow_proof_id(const uint8_t *proof, size_t len, uint8_t *out32);
uint64_t sm64pow_next_difficulty(const uint64_t *timestamps, const uint64_t *cumulative_lo,
                                 const uint64_t *cumulative_hi, size_t n);
uint64_t sm64pow_effective_difficulty(uint64_t difficulty, uint64_t elapsed_seconds);
uint64_t sm64pow_target_seconds(void);
void sm64pow_params(uint64_t difficulty, uint32_t *max_frames, uint32_t *random_amount,
                    uint32_t *window_length, uint32_t *burst_length);
/* A proof is varint(frames) || start || codec || inputs of the non-poked frames
 * (pokes are regenerated from the seed, never stored).
 * decode_raw: the inputs (4 bytes each; stateless codecs only); reencode:
 * smallest stateless encoding. Return the size (out NULL to query), SIZE_MAX if
 * the blob does not decode. */
size_t sm64pow_decode_raw(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
size_t sm64pow_reencode(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
uint32_t sm64pow_proof_frames(const uint8_t *proof, size_t len);
/* every frame's played pad (4 bytes each), replaying the run; SIZE_MAX error, SIZE_MAX-1 parent state not cached */
size_t sm64pow_played_block(const uint8_t *id32, const uint8_t *parent32, uint64_t gap_seconds, const uint8_t *hash32,
                            const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
/* the proof's inputs (non-poked frames, 4 bytes each), codec-free: replays the run */
size_t sm64pow_wanted_block(const uint8_t *id32, const uint8_t *parent32, uint64_t gap_seconds, const uint8_t *hash32,
                            const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
void sm64pow_probs(uint64_t difficulty, float *a_prob, float *b_prob, float *z_prob);
void sm64pow_rules_info(char *name, size_t name_cap, char *goal, size_t goal_cap);
/* SM64COIN: the last few blocks' end states on disk (loaded now; returns how
 * many), and the parallel pre-pass of a sync batch (chain::preverify). */
int32_t sm64pow_set_state_dir(const char *path);
uint64_t sm64pow_preverify(size_t n, const uint8_t *ids, const uint8_t *parents, const uint8_t *seeds,
                           const uint8_t *blobs, const uint64_t *offsets, const uint64_t *gaps, uint32_t threads);
/* gaps (may be NULL): each block's timestamp gap to its parent (UINT64_MAX unknown); a run longer
 * than difficulty 1 allows at that gap is not pre-replayed. */

/* SM64COIN: admission policy (admit.rs, docs/DOS.md): which blocks earn a
 * replay. Relay policy, not consensus. source: 1 relayed unasked, 2 a sync
 * span whose last block is at height top, 3 restricted RPC; leave with the
 * returned handle. admit_*: 1 replay, 0 skip (reason in sm64pow_last_error). */
void *sm64pow_source_enter(const char *host, int32_t source, uint64_t top);
void sm64pow_source_leave(void *handle);
int32_t sm64pow_admit_alt(const uint8_t *id32, const uint8_t *parent32, const uint8_t *hash32, uint32_t frames,
                          uint64_t height, uint64_t top, const uint8_t *main_seed32);
int32_t sm64pow_admit_relay(uint32_t frames);
int32_t sm64pow_source_kind(void);
int32_t sm64pow_rpc_submit_allowed(const char *host);
/* An RPC call that may replay (played pads, cycles, templates): draws on the RPC replay budget. */
void *sm64pow_rpc_enter(void);
void sm64pow_rpc_leave(void *handle);

#ifdef __cplusplus
}
#endif
