"""SM64Coin bot SDK for Python: see BOTS.md at the repository root."""

from sm64_binds import GamePad, GameState, buttons

from .daemon import Cycle, Daemon, Template, seal
from .env import MiningEnv, Snapshot, Task, load_rom, load_m64, replay_pads, verify
from .miner import practice, run_miner
from .perturb import Progress, event
from .proof import Proof, decode_proof, encode_proof
from .rules import Params, Seed, level8, max_frames, params

__all__ = [
    "Cycle", "Daemon", "GamePad", "GameState", "MiningEnv", "Params", "Progress", "Proof", "Seed",
    "Snapshot", "Task", "Template", "buttons", "decode_proof", "encode_proof", "event", "level8", "load_m64",
    "load_rom", "max_frames", "params", "practice", "replay_pads", "run_miner", "seal", "verify",
]
