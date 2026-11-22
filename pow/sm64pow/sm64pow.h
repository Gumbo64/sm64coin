/* SM64Coin proof-of-play, implemented in Rust (pow/sm64pow, see src/ffi.rs).
 * All consensus rules live there; the daemon only calls these. */
#pragma once
#include <stdint.h>
#include <stddef.h>
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
int32_t sm64pow_has_state(const uint8_t *id32);
int32_t sm64pow_block_progress(const uint8_t *id32, uint32_t *stars, uint32_t *keys, int32_t *grand_star, int32_t *ends_cycle);
int32_t sm64pow_proof_start(const uint8_t *proof, size_t len); /* 0 parent, 1 power-on, -1 bad */
uint64_t sm64pow_stall_seconds(void);
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
/* A proof is varint(frames) || codec || inputs of the non-poked frames (pokes
 * are regenerated from the seed, never stored).
 * decode_raw: the inputs (4 bytes each); played_raw: every frame's played pad
 * (replays the run); reencode: smallest encoding. Return the size (out NULL to
 * query), SIZE_MAX if the blob does not decode. */
size_t sm64pow_decode_raw(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
size_t sm64pow_reencode(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
uint32_t sm64pow_proof_frames(const uint8_t *proof, size_t len);
/* SIZE_MAX error, SIZE_MAX-1 parent state not cached */
size_t sm64pow_played_block(const uint8_t *id32, const uint8_t *parent32, uint64_t gap_seconds, const uint8_t *hash32,
                            const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
void sm64pow_probs(uint64_t difficulty, float *a_prob, float *b_prob, float *z_prob);
void sm64pow_rules_info(char *name, size_t name_cap, char *goal, size_t goal_cap);
#ifdef __cplusplus
}
#endif
