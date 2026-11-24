// SM64COIN web wallet: the one piece of the daemon's proof-of-play interface the wallet needs.
//
// A block id commits to the proof-of-play (cryptonote_format_utils.cpp calls
// sm64coin::proof_id), and the wallet computes block ids for every block it scans. The full
// implementation is the Rust crate pow/sm64pow (proof_id in lib.rs), which links the whole game
// engine; the browser wallet only needs this hash, which is a few lines on top of Keccak:
//
//   state codecs (ids 3, 4, 5):  Keccak256("sm64state" || blob)        (statecodec.rs state_proof_id)
//   raw (0), run-length (1), xz (2):
//        Keccak256("sm64run2" || frames u32 LE || start u8 || the decoded pads, 4 bytes each)
//   undecodable:                 Keccak256("sm64bad!" || blob)
//
// Blob layout (proof.rs): varint(frames) || start (0|1) || codec id || body.
// Keep this in step with pow/sm64pow/src/{lib,proof,statecodec}.rs.
#include <cstdint>
#include <string>
#include <vector>
#include "crypto/hash.h"
#include "sm64coin/sm64coin.h"
#if defined(SM64_WALLET_XZ)
#include <lzma.h>
#endif

namespace sm64coin
{
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

  static crypto::hash keccak_of(const std::string &s)
  {
    crypto::hash h;
    crypto::cn_fast_hash(s.data(), s.size(), h);
    return h;
  }

  static crypto::hash bad(const std::string &proof) { return keccak_of(std::string("sm64bad!") + proof); }

  crypto::hash proof_id(const std::string &proof)
  {
    const size_t MAX_FRAMES = 1 << 20;
    size_t i = 0;
    uint64_t frames;
    if (!get_varint(proof, i, frames) || frames > MAX_FRAMES || i + 2 > proof.size()) return bad(proof);
    const uint8_t start = (uint8_t)proof[i];
    const uint8_t codec = (uint8_t)proof[i + 1];
    if (start > 1) return bad(proof);
    const std::string body = proof.substr(i + 2);

    if (codec == 3 || codec == 4 || codec == 5)
      return keccak_of(std::string("sm64state") + proof);

    std::string raw; // the decoded pads, 4 bytes per input
    if (codec == 0)
    {
      if (body.size() % 4 != 0 || body.size() / 4 > frames) return bad(proof);
      raw = body;
    }
    else if (codec == 1)
    {
      size_t j = 0;
      std::string last;
      uint64_t total = 0;
      while (j < body.size())
      {
        uint64_t n;
        if (!get_varint(body, j, n) || n == 0 || j + 4 > body.size()) return bad(proof);
        const std::string pad = body.substr(j, 4);
        j += 4;
        if (pad == last) return bad(proof);
        last = pad;
        total += n;
        if (total > frames) return bad(proof);
        for (uint64_t k = 0; k < n; ++k) raw += pad;
      }
    }
#if defined(SM64_WALLET_XZ)
    else if (codec == 2)
    {
      const size_t cap = (size_t)frames * 4 + 1;
      raw.resize(cap);
      uint64_t memlimit = UINT64_MAX;
      size_t in_pos = 0, out_pos = 0;
      if (lzma_stream_buffer_decode(&memlimit, 0, nullptr, (const uint8_t *)body.data(), &in_pos, body.size(),
                                    (uint8_t *)&raw[0], &out_pos, cap) != LZMA_OK || out_pos >= cap || out_pos % 4)
        return bad(proof);
      raw.resize(out_pos);
    }
#endif
    else
      return bad(proof);

    std::string pre = "sm64run2";
    for (int k = 0; k < 4; ++k) pre.push_back((char)((frames >> (8 * k)) & 0xff));
    pre.push_back((char)start);
    pre += raw;
    return keccak_of(pre);
  }
}
