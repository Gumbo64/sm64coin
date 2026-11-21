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
/* frames (>0) if valid, 0 invalid, -1 internal error; reason in sm64pow_last_error */
/* hash32: cn_fast_hash(block hashing blob), the perturbation seed */
int64_t sm64pow_verify(const uint8_t *hash32, uint64_t difficulty, const uint8_t *proof, size_t len);
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
/* Proof blob -> raw pads (4 bytes/frame) / smallest encoding. Return the size
 * (out NULL to query), SIZE_MAX if the blob does not decode. */
size_t sm64pow_decode_raw(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
size_t sm64pow_reencode(const uint8_t *proof, size_t len, uint8_t *out, size_t cap);
void sm64pow_probs(uint64_t difficulty, float *a_prob, float *b_prob, float *z_prob);
void sm64pow_rules_info(char *name, size_t name_cap, char *goal, size_t goal_cap);
#ifdef __cplusplus
}
#endif
