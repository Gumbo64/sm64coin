"""Mirror of the consensus rules in pow/sm64pow/src/{lib,rules}.rs (`StarCycle`, "star-cycle-70").

The Rust crate is the source of truth. Each block is the game's next progress (a star, a Bowser
key, the Grand Star after 70 stars) played from the parent's end state (or from power-on, see
`env.Task`); the Grand Star ends the cycle. Difficulty is ONLY the time limit; the pokes are fixed.
The limit grows continuously with the time since the parent after a grace period (`max_frames`).
Daemon tasks carry the daemon's own numbers and `Task.from_template` refuses a mismatch, so a stale
copy cannot silently mine invalid blocks. All arithmetic is integer, identical to Rust's u64
(saturating where Rust saturates).
"""

from dataclasses import dataclass

from .perturb import PokeConfig, Progress

NAME = "star-cycle-70"
BASE_FRAMES = 20 * 60 * 30
MIN_FRAMES = 20 * 30
SHRINK_PERCENT = 98
RANDOM = 5  # pokes: bursts per window
WINDOW = 100
BURST = 2
TARGET = 300  # seconds between blocks the retarget aims for
DECAY_GRACE = 0  # seconds the limit stays flat after the parent (0: the frame-length auction)
DECAY_HALVING = 60  # seconds per difficulty halving of the limit's growth
START_HALVINGS = 20  # the limit starts this many halvings below the retarget's (StarCycle::start_halvings)
RESTART_BASE_SECONDS = 3600  # a restart's limit is the limit this much earlier...
RESTART_POINT_SECONDS = 60  # ...plus this per progress point thrown away...
RESTART_MAX_SECONDS = 2 * 3600  # ...at most this
TIMING_MIX = 1  # simulated frames between state mixes into the poke timing stream (0: never)
PAD_MIX = 1     # the same for the pad stream
RESEED_FRAMES = 0  # one 256-bit seed per run (the perturbation is seeded once, from the whole hash)
U64 = (1 << 64) - 1


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
        return PokeConfig(self.window, self.random_amount, self.burst, 0.5, 0.5, 0.2, TIMING_MIX, PAD_MIX)


def params(difficulty: int) -> Params:
    """The task at (effective) `difficulty`: the time limit and the (fixed) pokes."""
    l8 = level8(difficulty)
    frames = BASE_FRAMES
    for _ in range(l8):
        frames = frames * SHRINK_PERCENT // 100
        if frames <= MIN_FRAMES:
            break
    return Params(level8=l8, max_frames=max(frames, MIN_FRAMES), random_amount=RANDOM,
                  window=WINDOW, burst=BURST, reseed_frames=RESEED_FRAMES)


def max_frames_any() -> int:
    """The most frames any difficulty allows."""
    return params(1).max_frames


def effective_difficulty(difficulty: int, elapsed_seconds: int) -> int:
    """A block's weight: its retarget difficulty, whatever its timestamp."""
    return max(difficulty, 1)


def max_frames(difficulty: int, elapsed_seconds: int) -> int:
    """The time limit of a block at retarget `difficulty` timestamped `elapsed_seconds` after its
    parent. Continuous in time: after the grace period (none) the limit grows every second (one
    difficulty halving, 8 levels, per DECAY_HALVING seconds)."""
    t = DECAY_HALVING
    grace = DECAY_GRACE
    decay = min(max(elapsed_seconds - grace, 0) * 8, U64)  # in 1/t levels
    level = max((level8(difficulty) + 8 * START_HALVINGS) * t - decay, 0)  # in 1/t levels
    whole, frac = divmod(level, t)
    frames = BASE_FRAMES
    for _ in range(whole):
        frames = frames * SHRINK_PERCENT // 100
        if frames <= MIN_FRAMES:
            return MIN_FRAMES
    step = 100 - SHRINK_PERCENT  # percent per level
    frames = frames * (100 * t - step * frac) // (100 * t)
    return max(frames, MIN_FRAMES)


def valid_after(difficulty: int, frames: int):
    """The smallest number of seconds after its parent a block at retarget `difficulty` may be
    timestamped for a run of `frames` frames to be within the limit (None: never)."""
    if frames > max_frames(difficulty, 1 << 62):
        return None
    lo, hi = 0, 1 << 62
    while lo < hi:
        mid = (lo + hi) // 2
        if max_frames(difficulty, mid) >= frames:
            hi = mid
        else:
            lo = mid + 1
    return lo


def is_success(start: Progress, now: Progress) -> bool:
    """Whether `now` (the state after a frame) completes a block that started at `start`."""
    return now.points() > start.points()


def ends_cycle(end: Progress) -> bool:
    """Whether a block ending at `end` ends the cycle (the next block starts from power-on)."""
    return end.grand_star


def restart_delay(parent: Progress):
    """How many seconds earlier a power-on restart on top of `parent` takes its limit (it is
    valid only that long after the parent). Mirrors StarCycle::restart_delay."""
    return min(RESTART_BASE_SECONDS + parent.points() * RESTART_POINT_SECONDS,
               max(RESTART_MAX_SECONDS, RESTART_BASE_SECONDS))


def target_seconds() -> int:
    return TARGET


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
