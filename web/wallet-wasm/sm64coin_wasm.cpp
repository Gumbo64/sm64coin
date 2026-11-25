// SM64COIN web wallet: the one piece of the daemon's proof-of-play interface the wallet needs.
//
// A block id commits to the proof-of-play (cryptonote_format_utils.cpp calls
// sm64coin::proof_id), and the wallet computes block ids for every block it scans. The full
// implementation is the Rust crate pow/sm64pow (commit::proof_id in commit.rs), which links the whole game
// engine; since v0.4 the browser wallet needs no decoding at all: a proof blob is
//
//   varint(frames) || start (0|1) || input_hash (32 bytes) || codec id || body
//
// and its id is the committed input_hash read from the header (docs/V0.4-DESIGN.md 1.2):
//   header parses:        the 32 bytes after varint(frames) and start (the encoding may be empty or garbage)
//   header doesn't parse: Keccak256("sm64bad!" || blob)
// A header parses when: varint(frames) is canonical LEB128 of at most 10 bytes (bits shifted past 64 are
// dropped, like Rust's u64 <<; a final 0x00 byte after the first is rejected), frames <= 1 << 20, the start byte
// is 0 or 1, and 32 hash bytes follow. Keep this in step with pow/sm64pow/src/{commit,proof}.rs (docs/CORE.md).
#include <cstdint>
#include <string>
#include "crypto/hash.h"
#include "sm64coin/sm64coin.h"

namespace sm64coin
{
  // proof.rs get_varint: LEB128, at most 10 bytes (shifts 0, 7, ..., 63), canonical.
  static bool get_varint(const std::string &b, size_t &i, uint64_t &v)
  {
    v = 0;
    for (unsigned shift = 0; shift < 64; shift += 7)
    {
      if (i >= b.size()) return false;
      const uint8_t byte = (uint8_t)b[i++];
      v |= (uint64_t)(byte & 0x7f) << shift;
      if (!(byte & 0x80)) return !(byte == 0 && shift > 0);
    }
    return false;
  }

  crypto::hash proof_id(const std::string &proof)
  {
    const uint64_t MAX_FRAMES = 1u << 20; // proof.rs MAX_FRAMES
    size_t i = 0;
    uint64_t frames;
    if (get_varint(proof, i, frames) && frames <= MAX_FRAMES && i < proof.size() && (uint8_t)proof[i] <= 1 && proof.size() - i >= 33)
    {
      crypto::hash h; // commit.rs split: the committed input_hash
      for (int k = 0; k < 32; ++k) h.data[k] = proof[i + 1 + k];
      return h;
    }
    const std::string bad = std::string("sm64bad!") + proof;
    crypto::hash h;
    crypto::cn_fast_hash(bad.data(), bad.size(), h);
    return h;
  }
}
