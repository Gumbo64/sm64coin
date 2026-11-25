// SM64COIN web wallet: the one piece of the daemon's proof-of-play interface the wallet needs.
//
// A block id commits to the proof-of-play (cryptonote_format_utils.cpp calls
// sm64coin::proof_id), and the wallet computes block ids for every block it scans. The full
// implementation is the Rust crate pow/sm64pow (proof_id in proof_id.rs), which links the whole game
// engine; the browser wallet only needs this hash, which is a few lines on top of Keccak:
//
//   state codecs (ids 3, 4, 5):  Keccak256("sm64state" || blob)        (statecodec.rs state_proof_id)
//   raw (0), run-length (1), xz (2):
//        Keccak256("sm64run2" || frames u32 LE || start u8 || the decoded pads, 4 bytes each)
//   undecodable:                 Keccak256("sm64bad!" || blob)
//
// Blob layout (proof.rs): varint(frames) || start (0|1) || codec id || body.
// Keep this in step with pow/sm64pow/src/{proof_id,proof,statecodec,rules}.rs (docs/CORE.md).
#include <cstdint>
#include <string>
#include <vector>
#include "crypto/hash.h"
#include "sm64coin/sm64coin.h"
#include "xz-embedded/xz_wallet_config.h"
extern "C" {
#include "xz-embedded/xz.h" // xz-embedded (vendored; xz_unity.c is the C side), no liblzma in the browser
}

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
    // Rust (proof_id.rs proof_id), in this order:
    // 1. proof.rs split_blob: a canonical varint(frames) <= MAX_FRAMES (1 << 20), start 0|1 and a codec id byte;
    //    if that holds and the id is a state codec (3, 4, 5): Keccak256("sm64state" || blob), whatever the body.
    // 2. otherwise decode_proof(blob, rules().max_frames_any()): frames over min(the mainnet rules' largest time
    //    limit, MAX_FRAMES) is undecodable (rules.rs: StarCycle base_frames 36000 at difficulty 1).
    const uint64_t MAX_FRAMES = 1u << 20;  // proof.rs MAX_FRAMES
    const uint64_t MAX_FRAMES_ANY = 36000; // rules().max_frames_any()
    size_t i = 0;
    uint64_t frames;
    if (!get_varint(proof, i, frames) || frames > MAX_FRAMES || i + 2 > proof.size()) return bad(proof);
    const uint8_t start = (uint8_t)proof[i];
    const uint8_t codec = (uint8_t)proof[i + 1];
    if (start > 1) return bad(proof);

    if (codec == 3 || codec == 4 || codec == 5)
      return keccak_of(std::string("sm64state") + proof);

    if (frames > MAX_FRAMES_ANY) return bad(proof);
    const std::string body = proof.substr(i + 2);

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
    else if (codec == 2)
    {
      // Rust (proof.rs Xz::decode): xz2's XzDecoder (liblzma stream decoder, no memory limit) read through
      // take(frames*4+1): a stream that fills the cap, a failing stream, any byte after the stream (more
      // streams, stream padding, garbage: measured against the native decoder) or an output that is not
      // whole pads is undecodable. xz-embedded's single-call mode decodes into the output buffer of exactly
      // that size (a stream that would not fit, or is cut short, does not return STREAM_END).
      const size_t cap = (size_t)frames * 4 + 1;
      raw.resize(cap);
      static const bool crc_ready = (xz_crc32_init(), xz_crc64_init(), true); // the CRC tables (thread-safe static init)
      (void)crc_ready;
      struct xz_dec *dec = xz_dec_init(XZ_SINGLE, 0);
      if (!dec) return bad(proof);
      struct xz_buf b;
      b.in = (const uint8_t *)body.data(); b.in_pos = 0; b.in_size = body.size();
      b.out = (uint8_t *)&raw[0]; b.out_pos = 0; b.out_size = cap;
      const xz_ret r = xz_dec_run(dec, &b);
      xz_dec_end(dec);
      if (r != XZ_STREAM_END || b.in_pos != b.in_size || b.out_pos >= cap || b.out_pos % 4) return bad(proof);
      raw.resize(b.out_pos);
    }
    else
      return bad(proof);

    std::string pre = "sm64run2";
    for (int k = 0; k < 4; ++k) pre.push_back((char)((frames >> (8 * k)) & 0xff));
    pre.push_back((char)start);
    pre += raw;
    return keccak_of(pre);
  }
}
