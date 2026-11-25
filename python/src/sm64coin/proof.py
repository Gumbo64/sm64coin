"""The proof-of-play blob (v0.4, mirror of pow/sm64pow/src/proof.rs and commit.rs):
`varint(frames) || start byte || input_hash (32) || codec id || codec body` (docs/V0.4-DESIGN.md 1.2).

`input_hash = keccak256("sm64run2" || frames u32 LE || start || 4 bytes per non-poked input pad)`;
`proof_id` is that hash read from the header, no decoding (a blob whose header does not parse gets
`keccak256("sm64bad!" || blob)`). The v0.3 layout (no hash) survives as `*_v03`.

`start` says where the run begins (0: the parent block's end state, 1: power-on). The body holds
only the inputs of the frames the perturbation did not poke. Codecs: 0 raw (4 bytes per pad),
1 run-length, 2 xz of the raw pads. Encoding here always uses raw (the daemon accepts any codec).
"""

import lzma
from dataclasses import dataclass, field
from typing import List

from sm64_binds import GamePad

from .keccak import keccak256

MAX_FRAMES = 1 << 20  # hard cap on a decoded proof

PARENT = 0
POWER_ON = 1


def varint(n: int) -> bytes:
    out = bytearray()
    while n >= 0x80:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    out.append(n)
    return bytes(out)


def get_varint(b: bytes, i: int):
    """(value, next index); canonical varints only (like Rust)."""
    v = 0
    for shift in range(0, 64, 7):
        if i >= len(b):
            raise ValueError("truncated varint")
        byte = b[i]
        i += 1
        v |= (byte & 0x7F) << shift
        if byte & 0x80 == 0:
            if byte == 0 and shift > 0:
                raise ValueError("non-canonical varint")
            return v & ((1 << 64) - 1), i     # bits shifted past 64 are dropped, like Rust's u64 <<
    raise ValueError("varint too long")


def pad_bytes(p: GamePad) -> bytes:
    """Raw wire format of a pad: u16 LE buttons, i8 x, i8 y."""
    return (p.button & 0xFFFF).to_bytes(2, "little") + bytes([p.stick_x & 0xFF, p.stick_y & 0xFF])


def pad_from(c: bytes) -> GamePad:
    x, y = c[2], c[3]
    return GamePad(c[0] | c[1] << 8, x - 256 if x > 127 else x, y - 256 if y > 127 else y)


@dataclass
class Proof:
    frames: int
    start: int = PARENT
    inputs: List[GamePad] = field(default_factory=list)


def _raw_decode(body: bytes, max_frames: int):
    if len(body) % 4:
        raise ValueError(f"raw proof length {len(body)} is not a multiple of 4")
    if len(body) // 4 > max_frames:
        raise ValueError(f"proof has {len(body) // 4} frames, over {max_frames}")
    return [pad_from(body[i:i + 4]) for i in range(0, len(body), 4)]


def _rle_decode(body: bytes, max_frames: int):
    pads, i, last = [], 0, None
    while i < len(body):
        n, i = get_varint(body, i)
        if n == 0:
            raise ValueError("zero-length run")
        c = body[i:i + 4]
        if len(c) < 4:
            raise ValueError("truncated run")
        i += 4
        p = pad_from(c)
        if last is not None and last == pad_bytes(p):
            raise ValueError("adjacent runs repeat the same pad")
        last = pad_bytes(p)
        if len(pads) + n > max_frames:
            raise ValueError(f"proof expands past {max_frames} frames")
        pads.extend([p] * n)
    return pads


def _xz_decode(body: bytes, max_frames: int):
    cap = max_frames * 4 + 1
    d = lzma.LZMADecompressor(format=lzma.FORMAT_XZ)
    try:
        raw = d.decompress(body, max_length=cap)
    except lzma.LZMAError as e:
        raise ValueError(f"xz: {e}")
    if len(raw) >= cap:
        raise ValueError(f"proof expands past {max_frames} frames")
    if not d.eof:
        raise ValueError("xz: truncated stream")
    return _raw_decode(raw, max_frames)


def decode_inputs(blob: bytes, max_frames: int):
    """`codec id || body` -> pads."""
    if not blob:
        raise ValueError("empty proof")
    dec = {0: _raw_decode, 1: _rle_decode, 2: _xz_decode}.get(blob[0])
    if dec is None:
        if blob[0] >= 3:
            # State-aware codecs (3, 4, 5, ...) are decoded frame by frame while replaying the game
            # with a context-mixing model (pow/sm64pow/src/statecodec.rs, statemix.rs); this SDK does
            # not port it. Ask the daemon (get_block with sm64_played) or use the Rust SDK.
            raise ValueError(f"proof codec {blob[0]} is state-aware: it can only be decoded by replaying the run (use the daemon's get_block sm64_played, or the Rust SDK)")
        raise ValueError(f"unknown proof codec {blob[0]}")
    return dec(blob[1:], min(max_frames, MAX_FRAMES))


def _pads_raw(pads) -> bytes:
    """Raw wire bytes of `pads`: bytes already, or an iterable of GamePad."""
    return bytes(pads) if isinstance(pads, (bytes, bytearray, memoryview)) else b"".join(pad_bytes(p) for p in pads)


def input_hash(frames: int, start: int, pads) -> bytes:
    """The committed hash of a run: `pads` are the non-poked frames' inputs (GamePads or raw 4-byte pads)."""
    return keccak256(b"sm64run2" + frames.to_bytes(4, "little") + bytes([start]) + _pads_raw(pads))


def split(blob: bytes):
    """(frames, start, input_hash, encoding) of a committed blob; raises ValueError when the header does not parse
    (truncated, bad varint, frames over MAX_FRAMES, start not 0|1, fewer than 32 hash bytes). The encoding
    (`codec id || body`) may be empty or garbage."""
    frames, i = get_varint(blob, 0)
    if frames > MAX_FRAMES:
        raise ValueError(f"proof claims {frames} frames")
    if i >= len(blob):
        raise ValueError("truncated proof")
    if blob[i] not in (PARENT, POWER_ON):
        raise ValueError(f"unknown start {blob[i]}")
    h = blob[i + 1:i + 33]
    if len(h) < 32:
        raise ValueError("truncated proof header")
    return frames, blob[i], bytes(h), blob[i + 33:]


def proof_id(blob: bytes) -> bytes:
    """What a block id and the signature commit to: the header's input hash (never decodes anything);
    `keccak256("sm64bad!" || blob)` when the header does not parse."""
    try:
        return split(blob)[2]
    except ValueError:
        return keccak256(b"sm64bad!" + bytes(blob))


def _rle_encode(pads: bytes) -> bytes:
    out, i = bytearray(), 0
    while i < len(pads):
        j = i + 4
        while pads[j:j + 4] == pads[i:i + 4]:
            j += 4
        out += varint((j - i) // 4) + pads[i:i + 4]
        i = j
    return bytes(out)


def commit(frames: int, start: int, pads, codec: int = 0) -> bytes:
    """A committed proof blob; `pads` the non-poked frames' inputs (GamePads or raw bytes). Codec 0 raw,
    1 run-length, 2 xz (any valid xz stream decodes; bytes need not equal Rust's encoder)."""
    raw = _pads_raw(pads)
    body = {0: lambda: raw, 1: lambda: _rle_encode(raw), 2: lambda: lzma.compress(raw, format=lzma.FORMAT_XZ, check=lzma.CHECK_CRC32)}.get(codec)
    if body is None:
        raise ValueError(f"cannot encode codec {codec} (stateless codecs are 0, 1, 2)")
    return varint(frames) + bytes([start]) + input_hash(frames, start, raw) + bytes([codec]) + body()


def encode_proof(frames: int, start: int, inputs, codec: int = 0) -> bytes:
    """A committed proof blob (raw codec unless `codec`)."""
    return commit(frames, start, inputs, codec)


def decode_proof(blob: bytes, max_frames: int = MAX_FRAMES) -> Proof:
    """Decodes a committed blob statelessly (codecs 0-2) and checks the inputs hash to the header's
    input_hash; rejects more than `max_frames` frames or more inputs than frames. ValueError otherwise."""
    frames, start, h, enc = split(blob)
    if frames > min(max_frames, MAX_FRAMES):
        raise ValueError(f"proof claims {frames} frames, over {max_frames}")
    inputs = decode_inputs(enc, frames)
    if len(inputs) > frames:
        raise ValueError("more inputs than frames")
    if input_hash(frames, start, inputs) != h:
        raise ValueError("bad encoding: the inputs do not hash to the committed input_hash")
    return Proof(frames, start, inputs)


def encode_proof_v03(frames: int, start: int, inputs) -> bytes:
    """A v0.3-layout raw blob (`varint(frames) || start || 0 || pads`): no committed hash."""
    return varint(frames) + bytes([start]) + b"\x00" + _pads_raw(inputs)


def decode_proof_v03(blob: bytes, max_frames: int = MAX_FRAMES) -> Proof:
    """Decodes a v0.3-layout blob (no hash)."""
    frames, i = get_varint(blob, 0)
    if frames > min(max_frames, MAX_FRAMES):
        raise ValueError(f"proof claims {frames} frames, over {max_frames}")
    if i >= len(blob):
        raise ValueError("truncated proof")
    if blob[i] not in (PARENT, POWER_ON):
        raise ValueError(f"unknown start {blob[i]}")
    inputs = decode_inputs(blob[i + 1:], frames)
    if len(inputs) > frames:
        raise ValueError("more inputs than frames")
    return Proof(frames, blob[i], inputs)


def proof_start(blob: bytes) -> int:
    """Just the start of a committed proof blob."""
    return split(blob)[1]
