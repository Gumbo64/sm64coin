"""Mining loop and offline practice. A bot is a function `bot(env, stop) -> None`
that steps `env` until `env.is_success()` (or `stop()` turns true) and returns."""

import threading
import time

from .daemon import Daemon
from .env import MiningEnv, Task, load_rom


def practice(bot, seed=1, difficulty=1, stop=lambda: False, task=None):
    """Offline run of `bot` on a local task. Returns a dict: frames, max_frames, success, seconds,
    fps, proof (blob: frames + non-poked inputs, only when success), pads_raw (pads played on every frame).
    Verify with Rust: see BOTS.md."""
    load_rom()
    task = task or Task.practice(seed, difficulty)
    env = MiningEnv(task)
    t0 = time.time()
    bot(env, stop)
    dt = time.time() - t0
    return {"seed_hash": task.seed.hex(), "difficulty": task.difficulty, "frames": env.frame,
            "max_frames": task.params.max_frames, "success": env.success, "seconds": dt,
            "fps": env.frame / max(dt, 1e-9), "proof": env.proof() if env.success else None,
            "pads_raw": env.proof_raw(), "env": env}


def run_miner(bot, daemon_url, address, rom=None, blocks=0, log=print):
    """Mines with `bot`: template -> bot -> submit, restarting on a new block."""
    load_rom(rom)
    d = Daemon(daemon_url)
    mined = 0
    while True:
        try:
            task, t = d.get_task(address)
        except OSError as e:
            log(f"daemon: {e}; retrying in 5 s")
            time.sleep(5)
            continue
        log(f"block {t.height} difficulty {t.difficulty} limit {t.max_frames} frames, {t.random_amount} pokes/100")
        flag, done = threading.Event(), threading.Event()

        def watch():
            dd = Daemon(daemon_url)
            while not done.wait(2):
                try:
                    if dd.top_hash() != t.prev_hash:
                        flag.set()
                        return
                except OSError:
                    pass
                if t.easier_at and time.time() >= t.easier_at:
                    flag.set()
                    return

        threading.Thread(target=watch, daemon=True).start()
        env = MiningEnv(task)
        try:
            bot(env, flag.is_set)
        finally:
            done.set()
        if not env.success:
            log(f"  no star (stopped at frame {env.frame}); fetching a new template")
            time.sleep(1)
            continue
        try:
            d.submit(d.seal(t, env.proof()))
            mined += 1
            log(f"  BLOCK {t.height} ACCEPTED ({t.expected_reward / 1e12:.6f} SM64)")
            if blocks and mined >= blocks:
                return
        except RuntimeError as e:
            log(f"  block rejected: {e}")
