"""The controller perturbation ("pokes"), outside the game.

A line-by-line mirror of pow/perturb/src/lib.rs (the generator, the schedule)
and pow/perturb/src/sm64.rs (which game memory it reads). The Rust crate is the
source of truth; a run played with this class is checked against it by the
tests (the native verifier replays the proof).
"""

import struct
from dataclasses import dataclass

from sm64_binds import GamePad

M64 = (1 << 64) - 1

A_BUTTON = 0x8000
B_BUTTON = 0x4000
Z_TRIG = 0x2000


def _f32(x: float) -> float:
    """x rounded to f32 (Rust compares `prob < a_prob as f64` with a f32 a_prob)."""
    return struct.unpack("<f", struct.pack("<f", x))[0]


def _rotl(x: int, k: int) -> int:
    return ((x << k) | (x >> (64 - k))) & M64


def splitmix64(z: int) -> int:
    z = (z + 0x9E3779B97F4A7C15) & M64
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & M64
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & M64
    return z ^ (z >> 31)


@dataclass(frozen=True)
class PokeConfig:
    window_length: int
    random_amount: int
    burst_length: int
    a_prob: float = 0.5
    b_prob: float = 0.5
    z_prob: float = 0.2
    timing_mix: int = 100  # simulated frames between state mixes into the timing stream (0: never)
    pad_mix: int = 100     # the same for the pad stream


def _xoshiro(s) -> int:
    result = (_rotl((s[1] * 5) & M64, 7) * 9) & M64
    t = (s[1] << 17) & M64
    s[2] ^= s[0]
    s[3] ^= s[1]
    s[1] ^= s[2]
    s[0] ^= s[3]
    s[2] ^= t
    s[3] = _rotl(s[3], 45)
    return result


def _mix_into(s, digest) -> None:
    for w in digest:
        s[0] ^= splitmix64((w ^ s[3]) & M64)
        _xoshiro(s)


class Perturber:
    """Two xoshiro256** streams seeded from the whole block hash (timing: when pokes land; pad:
    what they press); see the module docs of lib.rs."""

    def __init__(self, seed_hash: bytes, cfg: PokeConfig):
        if len(seed_hash) != 32:
            raise ValueError("seed hash must be 32 bytes")
        w = [int.from_bytes(seed_hash[4 * k:4 * k + 4], "little") for k in range(8)]
        self.s = [w[0] | w[1] << 32, w[2] | w[3] << 32, w[4] | w[5] << 32, w[6] | w[7] << 32]
        if self.s == [0, 0, 0, 0]:
            self.s[0] = 1
        tag = 0x7061645F73747265  # "pad_stre"
        self.ps = [splitmix64(self.s[k] ^ tag ^ ((k * 0x9E3779B97F4A7C15) & M64)) for k in range(4)]
        if self.ps == [0, 0, 0, 0]:
            self.ps[0] = 1
        self.timing_mix, self.pad_mix = cfg.timing_mix, cfg.pad_mix
        self.ticks = 0
        self.undo = None
        self.window_length = max(cfg.window_length, 1)
        self.random_amount = cfg.random_amount
        self.burst_length = max(cfg.burst_length, 1)
        self.a_prob, self.b_prob, self.z_prob = _f32(cfg.a_prob), _f32(cfg.b_prob), _f32(cfg.z_prob)
        self.cur_window = 0
        self.cur_amount = 0
        self.cur_burst = 0
        self.last_clock = None

    def copy(self) -> "Perturber":
        c = Perturber.__new__(Perturber)
        c.__dict__.update(self.__dict__)
        c.s = list(self.s)
        c.ps = list(self.ps)
        return c

    def _next64(self) -> int:
        return _xoshiro(self.s)

    def _next32(self) -> int:
        return self._next64() >> 32

    def _prob(self) -> float:
        """Uniform in [0, 1) from 24 bits."""
        return (self._next32() >> 8) / float(1 << 24)

    def _pad_prob(self) -> float:
        return ((_xoshiro(self.ps) >> 32) >> 8) / float(1 << 24)

    def _stick(self) -> int:
        return (_xoshiro(self.ps) >> 32) % 161 - 80

    def _is_poke(self) -> bool:
        if self.cur_burst > 0:
            self.cur_burst -= 1
            return True
        if self.cur_window == 0:
            self.cur_window = self.window_length
            self.cur_amount = self.random_amount
        p = self.cur_amount / self.cur_window
        poke = self._prob() < p
        if poke:
            self.cur_amount -= 1
            self.cur_burst = self.burst_length - 1
        self.cur_window -= 1
        return poke

    def pad(self, clock: int, wanted: GamePad):
        """(pad to play, poked) given the game's clock before the frame."""
        ticking = self.last_clock is not None and self.last_clock != clock
        self.last_clock = clock
        self.undo = None
        if not ticking:
            return wanted, False
        undo = (list(self.s), list(self.ps), self.cur_window, self.cur_amount, self.cur_burst)
        if not self._is_poke():
            return wanted, False
        self.undo = undo
        b = 0
        if self._pad_prob() < self.a_prob:
            b |= A_BUTTON
        if self._pad_prob() < self.b_prob:
            b |= B_BUTTON
        if self._pad_prob() < self.z_prob:
            b |= Z_TRIG
        x = self._stick()
        y = self._stick()
        return GamePad(b, x, y), True

    def settle(self, simulated: bool, digest) -> None:
        """After the frame: mixes the digest into each stream on its interval if the game
        simulated; refunds the frame's poke if it didn't (it comes again next simulated frame)."""
        undo, self.undo = self.undo, None
        if simulated:
            self.ticks += 1
            if self.timing_mix > 0 and self.ticks % self.timing_mix == 0:
                _mix_into(self.s, digest)
            if self.pad_mix > 0 and self.ticks % self.pad_mix == 0:
                _mix_into(self.ps, digest)
        elif undo is not None:
            self.s, self.ps, self.cur_window, self.cur_amount, self.cur_burst = undo

    def mix(self, digest) -> None:
        """Folds a digest into both streams now (experiments; consensus uses settle)."""
        _mix_into(self.s, digest)
        _mix_into(self.ps, digest)


# ---- the Super Mario 64 adapter (pow/perturb/src/sm64.rs, headless build) ----

HEADLESS = dict(
    gMarioStates=0xb0a010, gLakituState=0xb097c8, gGlobalTimer=0xb09e94, gRandomSeed16=0xb05f20,
    gAreaUpdateCounter=0xbb0bc8, gCurrLevelNum=0x7ba0a2, gCurrAreaIndex=0xb09756,
    gSaveBuffer=0xbf7b70, gCurrSaveFileNum=0x7ba0a0,
)

MARIO_ACTION, MARIO_FACE_ANGLE, MARIO_POS, MARIO_VEL = 12, 44, 60, 72
MARIO_FORWARD_VEL, MARIO_NUM_COINS, MARIO_NUM_STARS, MARIO_HEALTH = 84, 168, 170, 174
MARIO_SIZE, LAKITU_YAW, LAKITU_SIZE = 176, 124, 126


def regions(a=HEADLESS):
    """(address, length) to read, in order."""
    return [
        (a["gAreaUpdateCounter"], 2),  # the clock
        (a["gMarioStates"], MARIO_SIZE),
        (a["gLakituState"], LAKITU_SIZE),
        (a["gGlobalTimer"], 4),
        (a["gRandomSeed16"], 2),
        (a["gCurrLevelNum"], 2),
        (a["gCurrAreaIndex"], 2),
    ]


def view(b: bytes):
    """(clock, digest) from the regions' bytes, concatenated in order."""
    u16 = lambda o: b[o] | b[o + 1] << 8
    u32 = lambda o: b[o] | b[o + 1] << 8 | b[o + 2] << 16 | b[o + 3] << 24
    m = 2
    l = m + MARIO_SIZE
    rest = l + LAKITU_SIZE
    clock = u16(0)
    digest = [
        u32(m + MARIO_POS) << 32 | u32(m + MARIO_POS + 4),
        u32(m + MARIO_POS + 8) << 32 | u32(m + MARIO_FORWARD_VEL),
        u32(m + MARIO_VEL) << 32 | u32(m + MARIO_VEL + 4),
        u32(m + MARIO_VEL + 8) << 32 | u32(m + MARIO_ACTION),
        u16(m + MARIO_FACE_ANGLE) << 48 | u16(m + MARIO_FACE_ANGLE + 2) << 32 | u16(m + MARIO_HEALTH) << 16 | u16(m + MARIO_NUM_COINS),
        u16(rest + 4) << 16 | u16(rest + 6),  # not gGlobalTimer (at rest): it counts paused frames
        u16(l + LAKITU_YAW) << 48 | u16(rest + 8) << 16 | u16(m + MARIO_NUM_STARS),
    ]
    return clock, digest


def read_view(game):
    """The (clock, digest) of a headless game."""
    return view(b"".join(game.read_memory(a, n) for a, n in regions()))


# ---- Progress: what a block must add (a star, a Bowser key, a cap switch, the Grand Star) ----
# Mirror of `progress` / `progress_regions` in pow/perturb/src/sm64.rs.

ACT_JUMBO_STAR_CUTSCENE = 0x0000_1909
SAVE_FILE_SIZE = 0x38  # struct SaveFile
SAVE_FILES = 4         # NUM_SAVE_FILES, two copies each
SAVE_FLAGS = 8         # SaveFile.flags
COURSE_STARS = 12      # SaveFile.courseStars[25]
HAVE_WING_CAP = 1 << 1   # SAVE_FLAG_HAVE_WING_CAP (red switch)
HAVE_METAL_CAP = 1 << 2  # green switch
HAVE_VANISH_CAP = 1 << 3  # blue switch
HAVE_KEY_1 = 1 << 4
HAVE_KEY_2 = 1 << 5
UNLOCKED_BASEMENT_DOOR = 1 << 6
UNLOCKED_UPSTAIRS_DOOR = 1 << 7
GRAND_STAR_MIN_STARS = 70  # stars needed before the Grand Star counts (the 70-star category)


@dataclass(frozen=True)
class Progress:
    """How far into the game a state is: one point per star, per Bowser key (held, or already
    used on its door), per cap switch pressed (v0.5) and for the Grand Star (with at least 70 stars)."""
    stars: int = 0
    keys: int = 0
    grand_star: bool = False
    # Which stars (bookkeeping, not consensus): index 0 the castle's secret stars, 1..=25 course
    # N's stars (bit k = star k+1).
    star_flags: tuple = (0,) * 26
    # The cap switches pressed: bit 0 wing (red), 1 metal (green), 2 vanish (blue).
    caps: int = 0

    def points(self) -> int:
        return self.stars + self.keys + int(self.grand_star) + bin(self.caps).count("1")

    def base_points(self) -> int:
        """`points` without the cap switches (what the restart delay counts)."""
        return self.stars + self.keys + int(self.grand_star)


def progress_regions(a=HEADLESS):
    """(address, length) that `progress` reads, in order: Mario, the save files (first copies),
    the current file's number."""
    return [
        (a["gMarioStates"], MARIO_SIZE),
        (a["gSaveBuffer"], SAVE_FILE_SIZE * 2 * SAVE_FILES),
        (a["gCurrSaveFileNum"], 2),
    ]


def progress_regions_len(a=HEADLESS) -> int:
    return sum(n for _, n in progress_regions(a))


def progress(b: bytes) -> Progress:
    """`Progress` from the bytes of `progress_regions`, concatenated."""
    u16 = lambda o: b[o] | b[o + 1] << 8
    u32 = lambda o: b[o] | b[o + 1] << 8 | b[o + 2] << 16 | b[o + 3] << 24
    stars = u16(MARIO_NUM_STARS)
    action = u32(MARIO_ACTION)
    saves = MARIO_SIZE
    file = u16(saves + SAVE_FILE_SIZE * 2 * SAVE_FILES)
    flags = u32(saves + (file - 1) * 2 * SAVE_FILE_SIZE + SAVE_FLAGS) if 1 <= file <= SAVE_FILES else 0
    star_flags = [0] * 26
    if 1 <= file <= SAVE_FILES:
        f = saves + (file - 1) * 2 * SAVE_FILE_SIZE
        star_flags[0] = (flags >> 24) & 0x7F
        for c in range(25):
            star_flags[c + 1] = b[f + COURSE_STARS + c] & 0x7F
    keys = int(flags & (HAVE_KEY_1 | UNLOCKED_BASEMENT_DOOR) != 0) + int(flags & (HAVE_KEY_2 | UNLOCKED_UPSTAIRS_DOOR) != 0)
    caps = int(flags & HAVE_WING_CAP != 0) | int(flags & HAVE_METAL_CAP != 0) << 1 | int(flags & HAVE_VANISH_CAP != 0) << 2
    return Progress(stars, keys, action == ACT_JUMBO_STAR_CUTSCENE and stars >= GRAND_STAR_MIN_STARS, tuple(star_flags), caps)


def event(before: Progress, after: Progress):
    """What a block added, as a short stable name: "c09s1" (course 9 star 1), "c00s3" (castle
    secret star 3), "key1", "wing"/"metal"/"vanish", "grand"; None if nothing (the first difference, like Rust `event`)."""
    if after.grand_star and not before.grand_star:
        return "grand"
    for c in range(26):
        new = after.star_flags[c] & ~before.star_flags[c]
        if new:
            return f"c{c:02d}s{(new & -new).bit_length()}"
    if after.keys > before.keys:
        return f"key{after.keys}"
    new_caps = after.caps & ~before.caps
    if new_caps:
        return ("wing", "metal", "vanish")[(new_caps & -new_caps).bit_length() - 1]
    return None


def read_progress(game) -> Progress:
    """The `Progress` of a headless game (what the rules' success condition reads)."""
    return progress(b"".join(game.read_memory(a, n) for a, n in progress_regions()))
