"""Task and MiningEnv: the contract between a bot and the chain."""

import hashlib
import random

from sm64_binds import GamePad, SM64GameGenerator

from . import rules
from .perturb import Perturber, Progress, read_progress, read_view
from .proof import PARENT, POWER_ON, decode_proof, encode_proof, pad_bytes, varint
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


def replay_pads(pads, game=None):
    """Plays `pads` UNPERTURBED (plain game stepping) on a fresh game (or `game`) and returns the
    game: how the end state of a cycle's blocks is rebuilt from the daemon's `played_pads`."""
    g = game if game is not None else load_rom().create_game()
    for p in pads:
        g.step_game(p)
    return g


def _dataclass_replace(p, **kw):
    import dataclasses
    return dataclasses.replace(p, **kw)


class Task:
    """What one block asks: where it starts, its seed hash, the time limit and the pokes.

    `start` is "power-on" (a fresh game) or "parent" (the parent block's end state, `start_game`,
    a game that is copied for every run; its progress is `start_progress`). The block's run must
    first add a point of progress (a star, a key, the Grand Star) exactly on its last frame,
    within `max_frames` (the limit at the block's timestamp; it only grows with time).
    `play_frames` is how long a bot may play (default `max_frames`; more means a winning run may
    have to wait until the limit has grown, see `run_miner`).
    """

    def __init__(self, seed: Seed, difficulty: int, params: Params = None, template=None,
                 start: str = "power-on", start_game=None, start_progress: Progress = None,
                 max_frames: int = None, play_frames: int = None):
        if start not in ("power-on", "parent"):
            raise ValueError("start must be 'power-on' or 'parent'")
        if start == "parent" and start_game is None:
            raise ValueError("a parent start needs the parent's end state (start_game)")
        self.seed = seed
        self.difficulty = difficulty  # the retarget difficulty
        self.params = params or rules_params(difficulty)
        if max_frames is not None:
            self.params = _dataclass_replace(self.params, max_frames=max_frames)
        self.play_frames = play_frames if play_frames is not None else self.params.max_frames
        self.template = template
        self.start = start
        self.start_game = start_game
        if start_progress is None:
            start_progress = read_progress(start_game if start_game is not None else load_rom().create_game())
        self.start_progress = start_progress

    @property
    def start_byte(self) -> int:
        return PARENT if self.start == "parent" else POWER_ON

    @property
    def max_frames(self) -> int:
        return self.params.max_frames

    def new_game(self):
        """A game at this task's start (a copy of the parent's end state, or a fresh power-on)."""
        gen = load_rom()
        return gen.copy(self.start_game) if self.start == "parent" else gen.create_game()

    @classmethod
    def practice(cls, seed: int, difficulty: int = 1, start_pads=None, **kw) -> "Task":
        """A local task. With `start_pads` the run starts from the state those pads (played
        unperturbed from power-on) reach, like a parent block's end state. NOTE: the seed hash
        is derived differently from the Rust SDK's `Task::practice` (use
        `Task(Seed.from_hex(..), d)` to run the same task in both)."""
        h = hashlib.sha256(b"sm64coin-practice-%d" % seed).digest()
        if start_pads is not None:
            return cls(Seed(h), difficulty, start="parent", start_game=replay_pads(start_pads), **kw)
        return cls(Seed(h), difficulty, **kw)

    @classmethod
    def from_template(cls, t, cycle=None, start: str = None, play_frames: int = None) -> "Task":
        """A task from a daemon template (`Daemon.get_task`). `cycle` (`Daemon.cycle`) is needed
        for a parent start: its played pads rebuild the parent's end state. `start`: "parent" or
        "power-on" (default: what the template allows, the parent when either does). Raises if
        this SDK's rules copy disagrees with the daemon's."""
        if t.rules and t.rules != rules.NAME:
            raise ValueError(f"daemon runs rules {t.rules!r}; this SDK implements only {rules.NAME!r}")
        d = t.retarget_difficulty or t.difficulty
        p = rules_params(d)
        if (p.random_amount, p.window, p.burst, p.reseed_frames) != (t.random_amount, t.window_length or p.window, t.burst_length or p.burst, t.reseed_frames):
            raise ValueError(f"this SDK's pokes ({p.random_amount}/{p.window}) differ from the daemon's "
                             f"({t.rules}: {t.random_amount}/{t.window_length}): update sm64coin/rules.py")
        if t.timestamp and t.parent_timestamp:
            want = rules.max_frames(d, max(t.timestamp - t.parent_timestamp, 0))
            if want != t.max_frames:
                raise ValueError(f"this SDK's time limit ({want} frames) differs from the daemon's "
                                 f"({t.max_frames}): update sm64coin/rules.py")
        allowed = t.start or "parent"
        if start is None:
            start = "parent" if allowed in ("parent", "either") else "power-on"
        if start == "parent" and allowed == "power-on":
            raise ValueError("this block must start from power-on")
        if start == "power-on" and allowed == "parent":
            raise ValueError("this block must start from the parent (a power-on restart is allowed only after the restart delay)")
        seed = Seed.from_hex(t.seed_hash)
        if start == "power-on":
            return cls(seed, d, p, t, max_frames=t.max_frames, play_frames=play_frames)
        if cycle is None:
            raise ValueError("a parent start needs the cycle (Daemon.cycle) to rebuild the parent's end state")
        game = replay_pads(cycle.pads)
        prog = read_progress(game)
        if (prog.stars, prog.keys, prog.grand_star) != (cycle.stars, cycle.keys, cycle.grand_star):
            raise RuntimeError(f"replaying the cycle's pads gave {prog}, the daemon says "
                               f"{cycle.stars} stars, {cycle.keys} keys, grand star {cycle.grand_star}")
        return cls(seed, d, p, t, start="parent", start_game=game, start_progress=prog,
                   max_frames=t.max_frames, play_frames=play_frames)


class Snapshot:
    def __init__(self, game, pert, frame, inputs, success, progress):
        self.game, self.pert, self.frame, self.inputs, self.success = game, pert, frame, inputs, success
        self.progress = progress


class MiningEnv:
    """A game being played for a Task, with the consensus perturbation applied.

    The perturbation (perturb.py) lives outside the game, seeded once from the block hash.
    `step(wanted)` is the only way to advance a committed run: it reads the game's clock, lets
    the perturbation replace `wanted` with a random pad (a poke) when its schedule says so,
    steps the game with the pad that came out and, if the game simulated, mixes the new state's
    digest into the perturbation. Game and perturbation are one state: `fork`/`save`/`restore`
    carry both. The proof keeps only the inputs of the frames that were not poked.

    Success: after a frame the game's progress (`env.progress`: stars, keys, grand star) has more
    points than the task's start progress. It must first hold on the run's last frame, so no
    frame may be played after it.
    """

    def __init__(self, task: Task, _game=None, _pert=None):
        self.task = task
        self._gen = load_rom()
        self.game = _game if _game is not None else task.new_game()
        self.pert = _pert if _pert is not None else Perturber(task.seed.data, task.params.poke_config())
        self.pads = []    # pads played on every frame
        self.inputs = []  # the wanted pads of the frames that weren't poked (the proof's inputs)
        self.success = False
        self.progress = task.start_progress
        self._state = None

    def _play(self, wanted: GamePad):
        """One frame, no guards: (pad played, poked). Updates `progress` and `success`."""
        before, _ = read_view(self.game)
        played, poked = self.pert.pad(before, wanted)
        self.game.step_game(played)
        after, digest = read_view(self.game)
        self.pert.settle(after != before, digest)
        self.pads.append(played)
        if not poked:
            self.inputs.append(wanted)
        self._state = None
        self.progress = read_progress(self.game)
        self.success = rules.is_success(self.task.start_progress, self.progress)
        return played, poked

    def step(self, wanted: GamePad) -> GamePad:
        """Plays one frame; returns the pad actually played (differs from `wanted` on pokes)."""
        if self.success:
            raise RuntimeError(f"already at the success condition (frame {len(self.pads)}); no more frames may be played")
        if self.is_over():
            raise RuntimeError(f"frame limit of {self.task.play_frames} reached")
        return self._play(wanted)[0]

    @property
    def frame(self) -> int:
        return len(self.pads)

    def frames_left(self) -> int:
        return max(0, self.task.play_frames - len(self.pads))

    def state(self):
        """Mario/level state after the last frame (`GameState`: pos, vel, numStars, levelNum, ...)."""
        if self._state is None:
            self._state = self.game.get_game_state()
        return self._state

    def is_success(self) -> bool:
        return self.success

    def is_over(self) -> bool:
        return self.success or len(self.pads) >= self.task.play_frames

    def fork(self) -> "MiningEnv":
        """An independent copy (game and perturbation) to simulate futures on."""
        e = MiningEnv(self.task, _game=self._gen.copy(self.game), _pert=self.pert.copy())
        e.pads, e.inputs, e.success, e._state = list(self.pads), list(self.inputs), self.success, self._state
        e.progress = self.progress
        return e

    def save(self) -> Snapshot:
        """A save point. (The Python binding has no byte-level save state: a snapshot is a clone.)"""
        return Snapshot(self._gen.copy(self.game), self.pert.copy(), len(self.pads), len(self.inputs),
                        self.success, self.progress)

    def restore(self, snap: Snapshot):
        if snap.frame > len(self.pads):
            raise ValueError("snapshot is ahead of this run (it belongs to another line)")
        self._gen.copy_to(snap.game, self.game)
        self.pert = snap.pert.copy()
        del self.pads[snap.frame:]
        del self.inputs[snap.inputs:]
        self.success, self.progress, self._state = snap.success, snap.progress, None

    def proof(self) -> bytes:
        """Proof blob: varint(frames) || start byte || codec 0 (raw) || 4 bytes per NON-POKED frame's input.
        The daemon re-encodes to the smallest codec; pokes are regenerated from the seed."""
        return encode_proof(len(self.pads), self.task.start_byte, self.inputs)

    def proof_raw(self) -> bytes:
        """The pads played on every frame (pokes included), raw."""
        return b"".join(pad_bytes(p) for p in self.pads)


def verify(task: Task, blob: bytes, max_frames: int = None):
    """Replays a proof blob on `task` like the daemon (chain.rs `replay`): (True, frames) when the
    success condition first holds exactly on the last frame with every input used and the proof's
    start is the task's; else (False, why). `max_frames`: the limit to enforce (default the task's)."""
    limit = task.max_frames if max_frames is None else max_frames
    try:
        pr = decode_proof(blob, limit)
    except ValueError as e:
        return False, f"bad proof: {e}"
    if pr.frames == 0:
        return False, "empty proof"
    if pr.frames > limit:
        return False, f"{pr.frames} frames exceeds the limit of {limit}"
    if pr.start != task.start_byte:
        return False, f"the proof starts from {'power-on' if pr.start == POWER_ON else 'the parent'}, the task from {task.start}"
    env = MiningEnv(task)
    nxt = 0
    for f in range(pr.frames):
        wanted = pr.inputs[nxt] if nxt < len(pr.inputs) else GamePad()
        _, poked = env._play(wanted)
        if not poked:
            if nxt >= len(pr.inputs):
                return False, f"frame {f} needs an input but the proof has run out"
            nxt += 1
        if env.success:
            if f + 1 != pr.frames:
                return False, f"success at frame {f + 1} but the proof runs to {pr.frames}"
            if nxt != len(pr.inputs):
                return False, f"{len(pr.inputs) - nxt} inputs left over"
            return True, pr.frames
    return False, "no progress was made (no new star, key or Grand Star)"
