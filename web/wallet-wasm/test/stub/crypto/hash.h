// Test stub of monero's crypto/hash.h: just what sm64coin_wasm.cpp uses.
#pragma once
#include <cstddef>
#include <cstdint>
extern "C" void keccak(const uint8_t *in, size_t inlen, uint8_t *md, int mdlen);  // monero/src/crypto/keccak.c
namespace crypto
{
  struct hash { char data[32]; };
  inline void cn_fast_hash(const void *data, size_t length, hash &h) { keccak((const uint8_t *)data, length, (uint8_t *)h.data, 32); }
}
