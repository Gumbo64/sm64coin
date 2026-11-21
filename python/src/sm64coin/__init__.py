"""SM64Coin bot SDK for Python: see BOTS.md at the repository root."""

from sm64_binds import GamePad, GameState, buttons

from .daemon import Daemon, Template, seal
from .env import MiningEnv, Snapshot, Task, load_rom, load_m64
from .miner import practice, run_miner
from .rules import Params, Seed, level8, params

__all__ = [
    "Daemon", "GamePad", "GameState", "MiningEnv", "Params", "Seed", "Snapshot",
    "Task", "Template", "buttons", "level8", "load_m64", "load_rom", "params",
    "practice", "run_miner", "seal",
]
