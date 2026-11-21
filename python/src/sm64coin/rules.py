"""Mirror of the consensus rules in pow/sm64pow/src/{lib,rules}.rs.

The Rust crate is the source of truth. This is a copy of the current
`any-star` rules; daemon tasks carry the daemon's own numbers and
`Task.from_template` refuses a mismatch, so a stale copy cannot silently mine
invalid blocks. The perturbation is covered by tests that verify Python
proofs with the Rust verifier.
"""

from dataclasses import dataclass

from .perturb import PokeConfig

BASE_FRAMES = 10 * 60 * 30
MIN_FRAMES = 60 * 30
SHRINK_PERCENT = 98
BASE_RANDOM = 5
MAX_RANDOM = 40
WINDOW = 100
BURST = 2
RESEED_FRAMES = 0  # one 256-bit seed per run (the perturbation is seeded once, from the whole hash)


def level8(d: int) -> int:
    """About 8*log2(d) in integer arithmetic (0 for d <= 1)."""
    if d <= 1:
        return 0
    msb = d.bit_length() - 1
    frac = (d >> (msb - 3)) & 7 if msb >= 3 else (d << (3 - msb)) & 7
    return msb * 8 + frac


@dataclass(frozen=True)
class Params:
    level8: int
    max_frames: int
    random_amount: int
    window: int
    burst: int
    reseed_frames: int

    def poke_config(self) -> PokeConfig:
        return PokeConfig(self.window, self.random_amount, self.burst, 0.5, 0.5, 0.2)


def params(difficulty: int) -> Params:
    """What the `any-star` rules ask at `difficulty`."""
    l8 = level8(difficulty)
    frames = BASE_FRAMES
    for _ in range(l8):
        frames = frames * SHRINK_PERCENT // 100
        if frames <= MIN_FRAMES:
            break
    return Params(
        level8=l8,
        max_frames=max(frames, MIN_FRAMES),
        random_amount=min(BASE_RANDOM + l8 // 8, MAX_RANDOM),
        window=WINDOW,
        burst=BURST,
        reseed_frames=RESEED_FRAMES,
    )


class Seed:
    """A block's perturbation seed: the 32 bytes of cn_fast_hash(hashing blob). The perturbation
    (perturb.py) is seeded once from all of it."""

    def __init__(self, data: bytes):
        if len(data) != 32:
            raise ValueError("seed hash must be 32 bytes")
        self.data = bytes(data)

    @classmethod
    def from_hex(cls, h: str) -> "Seed":
        return cls(bytes.fromhex(h))

    def hex(self) -> str:
        return self.data.hex()

    def word(self, k: int) -> int:
        """32-bit LE word (k % 8) of the hash, XOR (k // 8) * 0x9E3779B9 (mod 2^32); 0 becomes 1.
        (Mirrors `Seed::word` in Rust; no longer used by the perturbation.)"""
        i = (k % 8) * 4
        w = int.from_bytes(self.data[i:i + 4], "little")
        w ^= ((k // 8) * 0x9E3779B9) & 0xFFFFFFFF
        return w or 1
