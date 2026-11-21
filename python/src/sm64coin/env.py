"""Task and MiningEnv: the contract between a bot and the chain."""

import hashlib
import random

from sm64_binds import GamePad, SM64GameGenerator

from .rules import Params, Seed, params as rules_params

_generator = None


def load_rom(path=None):
    """Loads the ROM once per process (path: US .z64; default $SM64COIN_ROM,
    ./baserom.us.z64, ~/.sm64coin/baserom.us.z64). Returns the generator."""
    global _generator
    if _generator is not None:
        return _generator
    import os
    cands = [path] if path else [os.environ.get("SM64COIN_ROM"), "baserom.us.z64",
                                 os.path.expanduser("~/.sm64coin/baserom.us.z64")]
    for p in cands:
        if p and os.path.exists(p):
            _generator = SM64GameGenerator.from_file(p)
            return _generator
    raise FileNotFoundError("no SM64 ROM: pass a path, set SM64COIN_ROM or put baserom.us.z64 in the working directory")


def load_m64(path):
    """GamePads from a Mupen64 movie (0x400 byte header) or a raw 4-bytes-a-frame file
    (u16 big-endian buttons, i8 x, i8 y)."""
    b = open(path, "rb").read()
    if len(b) >= 0x400 and b[:4] == b"M64\x1a":
        b = b[0x400:]
    out = []
    for i in range(0, len(b) - len(b) % 4, 4):
        x, y = b[i + 2], b[i + 3]
        out.append(GamePad((b[i] << 8) | b[i + 1], x - 256 if x > 127 else x, y - 256 if y > 127 else y))
    return out


def pad_bytes(p: GamePad) -> bytes:
    """Raw wire format of a pad: u16 LE buttons, i8 x, i8 y."""
    return (p.button & 0xFFFF).to_bytes(2, "little") + bytes([p.stick_x & 0xFF, p.stick_y & 0xFF])


class Task:
    """What one block asks: seed hash, difficulty and the numbers derived from it."""

    def __init__(self, seed: Seed, difficulty: int, params: Params = None, template=None):
        self.seed = seed
        self.difficulty = difficulty
        self.params = params or rules_params(difficulty)
        self.template = template

    @classmethod
    def practice(cls, seed: int, difficulty: int = 1) -> "Task":
        """A local task. NOTE: the seed hash is derived differently from the Rust SDK's
        `Task::practice` (use `Task(Seed.from_hex(..), d)` to run the same task in both)."""
        h = hashlib.sha256(b"sm64coin-practice-%d" % seed).digest()
        return cls(Seed(h), difficulty)

    @classmethod
    def from_template(cls, t) -> "Task":
        """A task from a daemon template (`Daemon.get_task`). Raises if this SDK's rules copy
        disagrees with the daemon's."""
        p = rules_params(t.difficulty)
        if (p.max_frames, p.random_amount, p.reseed_frames) != (t.max_frames, t.random_amount, t.reseed_frames):
            raise ValueError(
                f"this SDK's rules ({p.max_frames} frames, {p.random_amount} pokes) differ from the daemon's "
                f"({t.rules}: {t.max_frames} frames, {t.random_amount} pokes): update sm64coin/rules.py")
        if t.rules and t.rules != "any-star":
            raise ValueError(f"daemon runs rules {t.rules!r}; this SDK implements only 'any-star'")
        return cls(Seed.from_hex(t.seed_hash), t.difficulty, p, t)


class Snapshot:
    def __init__(self, game, frame, success):
        self.game, self.frame, self.success = game, frame, success


class MiningEnv:
    """A game being played for a Task, with the consensus perturbation applied.

    `step(wanted)` is the only way to advance a committed run: it reseeds when frame f > 0 and
    f % reseed_frames == 0, runs `wanted` through `game.rng_pad`, steps the game with the result
    and records it. The recorded pads are the proof.
    """

    def __init__(self, task: Task, _game=None):
        self.task = task
        self._gen = load_rom()
        if _game is None:
            _game = self._gen.create_game()
            _game.set_rng_seed(task.seed.word(0))
            _game.set_rng_config(task.params.rng_config())
        self.game = _game
        self.pads = []
        self.success = False
        self._state = None

    def step(self, wanted: GamePad) -> GamePad:
        """Plays one frame; returns the pad actually played (differs from `wanted` on pokes)."""
        if self.success:
            raise RuntimeError(f"already at the success condition (frame {len(self.pads)}); no more frames may be played")
        if self.is_over():
            raise RuntimeError(f"frame limit of {self.task.params.max_frames} reached")
        f, r = len(self.pads), self.task.params.reseed_frames
        if f > 0 and f % r == 0:
            self.game.set_rng_seed(self.task.seed.word(f // r))
        played = self.game.rng_pad(wanted)
        self.game.step_game(played)
        self.pads.append(played)
        self._state = self.game.get_game_state()
        self.success = self._state.numStars > 0  # the 'any-star' success condition
        return played

    @property
    def frame(self) -> int:
        return len(self.pads)

    def frames_left(self) -> int:
        return max(0, self.task.params.max_frames - len(self.pads))

    def state(self):
        """Mario/level state after the last frame (`GameState`: pos, vel, numStars, levelNum, ...)."""
        if self._state is None:
            self._state = self.game.get_game_state()
        return self._state

    def is_success(self) -> bool:
        return self.success

    def is_over(self) -> bool:
        return self.success or len(self.pads) >= self.task.params.max_frames

    def fork(self) -> "MiningEnv":
        """An independent copy to simulate futures on."""
        e = MiningEnv(self.task, _game=self._gen.copy(self.game))
        e.pads, e.success, e._state = list(self.pads), self.success, self._state
        return e

    def save(self) -> Snapshot:
        """A save point. (The Python binding has no byte-level save state: a snapshot is a clone.)"""
        return Snapshot(self._gen.copy(self.game), len(self.pads), self.success)

    def restore(self, snap: Snapshot):
        if snap.frame > len(self.pads):
            raise ValueError("snapshot is ahead of this run (it belongs to another line)")
        self._gen.copy_to(snap.game, self.game)
        del self.pads[snap.frame:]
        self.success, self._state = snap.success, None

    def proof(self) -> bytes:
        """Proof blob: codec 0 (raw) + 4 bytes per pad. The daemon re-encodes to the smallest codec."""
        return b"\x00" + self.proof_raw()

    def proof_raw(self) -> bytes:
        """Raw pads for Rust's `examples/verify_file`."""
        return b"".join(pad_bytes(p) for p in self.pads)
