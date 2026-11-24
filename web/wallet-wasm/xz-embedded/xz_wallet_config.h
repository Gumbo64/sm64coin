/* Build options of the xz-embedded decoder for the wallet; included before xz.h by xz_unity.c and
 * ../sm64coin_wasm.cpp so both see the same declarations.
 * Mirrors Rust's xz2 XzDecoder (liblzma stream decoder, no memory limit): one xz stream of LZMA2 with
 * CRC32/CRC64/no check (other check ids are accepted unverified; liblzma would verify SHA-256, never
 * produced by xz2's encoder). No BCJ/delta filters, no concatenated streams. */
#define XZ_DEC_SINGLE
#define XZ_USE_CRC64
#define XZ_DEC_ANY_CHECK
