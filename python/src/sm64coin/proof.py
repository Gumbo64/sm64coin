"""The proof-of-play blob (mirror of pow/sm64pow/src/proof.rs):
`varint(frames) || start byte || codec id || codec body`.

`start` says where the run begins (0: the parent block's end state, 1: power-on). The body holds
only the inputs of the frames the perturbation did not poke. Codecs: 0 raw (4 bytes per pad),
1 run-length, 2 xz of the raw pads. Encoding here always uses raw (the daemon accepts any codec).
"""

import lzma
from dataclasses import dataclass, field
from typing import List

from sm64_binds import GamePad

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
            return v, i
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
        raise ValueError(f"unknown proof codec {blob[0]}")
    return dec(blob[1:], min(max_frames, MAX_FRAMES))


def encode_proof(frames: int, start: int, inputs) -> bytes:
    """A proof blob in the raw codec."""
    return varint(frames) + bytes([start]) + b"\x00" + b"".join(pad_bytes(p) for p in inputs)


def decode_proof(blob: bytes, max_frames: int = MAX_FRAMES) -> Proof:
    """Decodes a proof blob; rejects more than `max_frames` frames or more inputs than frames."""
    frames, i = get_varint(blob, 0)
    if frames > min(max_frames, MAX_FRAMES):
        raise ValueError(f"proof claims {frames} frames, over {max_frames}")
    if i >= len(blob):
        raise ValueError("truncated proof")
    if blob[i] not in (PARENT, POWER_ON):
        raise ValueError(f"unknown start {blob[i]}")
    inputs = decode_inputs(blob[i + 1:], frames)
    return Proof(frames, blob[i], inputs)


def proof_start(blob: bytes) -> int:
    """Just the start of a proof blob."""
    _, i = get_varint(blob, 0)
    if i >= len(blob):
        raise ValueError("truncated proof")
    if blob[i] not in (PARENT, POWER_ON):
        raise ValueError(f"unknown start {blob[i]}")
    return blob[i]
