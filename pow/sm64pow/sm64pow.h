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
void sm64pow_note_eligible(const uint8_t *id32, const uint8_t *parent32, const uint8_t *hash32, uint32_t frames, uint64_t timestamp, const uint8_t *sig64, const uint8_t *proof, size_t len); /* sig64, proof: the block's sm64_sig and sm64_proof (the equal-length tie-break: canonical proof size, then the signature's hash) */
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
/* v0.4 (features.rs): the consensus features, bits 0 committed inputs, 1 included proofs,
 * 2 proof weight, 3 proof rewards; 0 ok, -1 dependencies don't hold. sm64pow_verify_block
 * returns -3 for a bad encoding (committed inputs: never mark the block id invalid); the queue's
 * verdict 4 is a bad encoding too (drop the claim, not the id). */
int32_t sm64pow_set_features(uint32_t bits);
/* The equal-length tie-break's final key (pof::TieKey): 0 signature hash (default), 1 seed hash, 2 first seen. */
void sm64pow_set_tie_key(uint32_t k);

/* v0.4 codec negotiation (codecs.rs, node policy): this node's advert (NOTIFY_SM64_CODECS, (id, version)
 * pairs); a peer's set (32 bytes, a bit per id) from its advert (len 0: the baseline 0-2); what sending a
 * blob to a peer needs (0 nothing, 1 a stateless transcode, 2 the run's inputs first: sm64pow_wanted_block);
 * the blob re-encoded for the peer (same header, same block id; 0: send as is; SIZE_MAX error); a blob's codec id. */
size_t sm64pow_codecs_advert(uint8_t *out, size_t cap);
void sm64pow_codecs_mask(const uint8_t *advert, size_t len, uint8_t *out32);
int32_t sm64pow_codecs_need(const uint8_t *proof, size_t len, const uint8_t *mask32);
size_t sm64pow_transcode(const uint8_t *proof, size_t len, const uint8_t *inputs, size_t inputs_len, const uint8_t *mask32, uint8_t *out, size_t cap);
int32_t sm64pow_proof_codec(const uint8_t *proof, size_t len);

/* v0.4 stage B, included proofs (included.rs, pool.rs; docs/V0.4-DESIGN.md sections 2 and 6).
 * Verify a block's sm64_proofs field (varint(n) || records; len 0 = none) against the including block's
 * extra nonce (root TLV 0x02) and its parent block h (height, its parent B_{h-1}, its key nonce and
 * committed proof, its retarget difficulty). 1 valid, 0 invalid, -3 bad witness (never invalid: drop the
 * sender), -2 B_{h-1}'s end state isn't cached (rebuild, retry), -1 internal error. *counted: records
 * counted for weight (0 while proof_weight is off). normalized (may be NULL): the field with each
 * stateless proof in its smallest encoding (state-coded ones kept); *normalized_len always set. Empty
 * field with no root: 1 at once, no state needed. With included_proofs off any record or root is invalid. */
int32_t sm64pow_verify_records(const uint8_t *field, size_t field_len, const uint8_t *nonce, size_t nonce_len,
                               uint64_t h_height, const uint8_t *h_prev32, int32_t h_prev_is_genesis,
                               const uint8_t *h_nonce, size_t h_nonce_len, const uint8_t *h_proof, size_t h_proof_len,
                               uint64_t h_difficulty, uint32_t threads, uint32_t *counted,
                               uint8_t *normalized, size_t cap, size_t *normalized_len);
/* The proof pool (node policy). Our new tip is block h: the pool collects proofs for height h. */
void sm64pow_pool_tip(const uint8_t *h_id32, uint64_t h_height, const uint8_t *h_prev32, int32_t h_prev_is_genesis,
                      const uint8_t *h_nonce, size_t h_nonce_len, const uint8_t *h_proof, size_t h_proof_len, uint64_t h_difficulty);
/* One record (wire format, no count prefix) for our current tip: 1 new and valid (queued for relay), 0 known /
 * not for our tip / outside the window / over budget / included_proofs off (ignore), -1 invalid and -3 bad
 * witness (drop the sender), -2 the parent's state isn't cached. Replays on this thread. */
int32_t sm64pow_pool_offer(const uint8_t *record, size_t len);
/* The next new record to gossip (NOTIFY_SM64_PROOF), smallest stateless encoding. Size-then-fill: consumed only
 * when cap was enough (out NULL: size only); 0: none. */
size_t sm64pow_pool_take_relay(uint8_t *out, size_t cap);
/* The sm64_proofs field for a template on top of tip: best 64 records by (frames, seed hash), canonical order,
 * proofs stateless; its root into root_out32 (zero when none); returns the length (0: none or switch off).
 * Remembered by root (the last 32). */
size_t sm64pow_pool_select(const uint8_t *tip_id32, uint8_t *out, size_t cap, uint8_t *root_out32);
/* The field a selection with this root had (reattach at submit_block); 0: unknown. */
size_t sm64pow_pool_field_for_root(const uint8_t *root32, uint8_t *out, size_t cap);
/* A sealed block blob (an optional trailing sm64_proofs string after the signature is read) as a proof record;
 * SIZE_MAX if it doesn't parse. */
size_t sm64pow_record_from_block(const uint8_t *block_blob, size_t len, uint8_t *out, size_t cap);
/* The v0.4 extra nonce: the miner's 0x64||key nonce (the rest replaced) + payout TLV (spend/view NULL: none) +
 * root TLV (root32 NULL or zero: none). Size-then-fill; SIZE_MAX if invalid. */
size_t sm64pow_template_nonce(const uint8_t *miner_nonce, size_t miner_nonce_len, int32_t subaddress,
                              const uint8_t *spend32, const uint8_t *view32, const uint8_t *root32,
                              uint8_t *out, size_t cap);

/* ---- v0.4 stage C and wiring ---- */
/* The coinbase outputs that pay a block's (verified) sm64_proofs `field`, in record order, output index
 * first_index + i (m = 1: first_index 1). Per record 73 bytes: output key P_j (32) || view tag (1) ||
 * additional tx pubkey R_j (32) || amount u64 LE (8); amount = min(R/128, 25% pot / n) of base_reward.
 * Size-then-fill. 0: no records or proof_rewards off. SIZE_MAX: the field doesn't parse (last_error). */
size_t sm64pow_proof_outputs(const uint8_t *field, size_t field_len, uint64_t base_reward, uint32_t first_index,
                             uint8_t *out, size_t cap);
/* The coinbase rule. outs: n_outs x 41 bytes (key 32 || view tag 1 || amount u64 LE 8), the coinbase's outputs in
 * order (view tag 0 = untagged txout_to_key, not compared; otherwise it must equal the derived tag);
 * add_keys: n_add x 32 (tx_extra_additional_pub_keys). With proof_rewards on and n records: n_outs == 1 + n,
 * outputs 1..n match sm64pow_proof_outputs, n_add == n_outs and add_keys[1+i] == R_j (add_keys[0] free).
 * No records or rewards off: n_outs == 1 and n_add <= 1. 1 ok, 0 invalid (last_error). The money check's raise
 * by the proofs' total is the caller's (records::coinbase_total). */
int32_t sm64pow_check_miner_tx(const uint8_t *field, size_t field_len, uint64_t base_reward,
                               const uint8_t *outs, size_t n_outs, const uint8_t *add_keys, size_t n_add);
/* How many of the block's records count for weight (structural check, no replay, no start check); arguments as
 * sm64pow_verify_records up to h_difficulty. 0 when proof_weight is off or anything fails. */
uint32_t sm64pow_count_records(const uint8_t *field, size_t field_len, const uint8_t *nonce, size_t nonce_len,
                               uint64_t h_height, const uint8_t *h_prev32, int32_t h_prev_is_genesis,
                               const uint8_t *h_nonce, size_t h_nonce_len, const uint8_t *h_proof, size_t h_proof_len,
                               uint64_t h_difficulty);
/* Fork choice with proof weight: the chains above the fork point, seeds 32 bytes and counted-proofs per block
 * (nearest the fork first; main_len/alt_len above the entries given: the rest weigh 1). 1: switch to alt.
 * proof_weight on: longer AND heavier (block weight 1 + counted, 0 if equivocation-tainted; the 1.5-block length
 * penalty stays). Off: exactly sm64pow_fork_choice. The 36-block reorg cap stays with the caller. */
int32_t sm64pow_fork_choice_weighted(uint64_t main_len, const uint8_t *main_seeds, const uint32_t *main_counted, size_t n_main,
                                     uint64_t alt_len, const uint8_t *alt_seeds, const uint32_t *alt_counted, size_t n_alt);
/* Sync pre-pass for records, after sm64pow_preverify: n full block blobs in chain order (blocks[offsets[i]..offsets[i+1]],
 * n + 1 offsets), ids (n x 32), each block's own retarget difficulty (0: unknown, window not applied). Replays the
 * records of blocks whose parent block h is the previous block of the batch, in parallel, caching run summaries.
 * Returns how many records it replayed. Never decides validity. */
uint64_t sm64pow_preverify_records(size_t n, const uint8_t *ids, const uint8_t *blocks, const uint64_t *offsets,
                                   const uint64_t *difficulties, uint32_t threads);
/* The sm64_proofs field with every record proof in an encoding a peer with codec set mask32 (NULL: baseline) decodes.
 * Size-then-fill. 0: no change needed. SIZE_MAX (last_error): unparsable, or a state-coded record's inputs are not
 * cached (inputs of recently replayed records are kept, 256): send the block as it is. */
size_t sm64pow_records_for_peer(const uint8_t *field, size_t field_len, const uint8_t *mask32, uint8_t *out, size_t cap);
/* Unix second at which the template on tip should be built: the tip's eligible time (as sm64pow_note_eligible
 * recorded it) + displacement window (30 s) + collection grace (10 s). 0: unknown. */
uint64_t sm64pow_records_ready_at(const uint8_t *tip_id32);
/* Monero subaddress (major, minor): spend_out32 = D = B + m G, view_out32 = C = a D, m = Hs("SubAddr\0" || a ||
 * major LE32 || minor LE32); (0, 0) is the main address (B, a G). 1 ok, 0 bad key. */
int32_t sm64pow_subaddress(const uint8_t *view_secret32, const uint8_t *spend_public32, uint32_t major, uint32_t minor,
                           uint8_t *spend_out32, uint8_t *view_out32);

#ifdef __cplusplus
}
#endif
