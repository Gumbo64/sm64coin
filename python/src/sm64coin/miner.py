"""Mining loop and offline practice. A bot is a function `bot(env, stop) -> None`
that steps `env` until `env.is_success()` (or `stop()` turns true) and returns."""

import threading
import time

from . import rules
from .daemon import Daemon
from .env import MiningEnv, Task, load_rom


def practice(bot, seed=1, difficulty=1, stop=lambda: False, task=None):
    """Offline run of `bot` on a local task (from power-on; pass `task=Task.practice(seed, d,
    start_pads=...)` to start from a parent end state). Returns a dict: frames, max_frames,
    success, seconds, fps, proof (blob: frames + start + non-poked inputs, only when success),
    pads_raw (pads played on every frame). Verify with `env.verify` or Rust: see BOTS.md."""
    load_rom()
    task = task or Task.practice(seed, difficulty)
    env = MiningEnv(task)
    t0 = time.time()
    bot(env, stop)
    dt = time.time() - t0
    return {"seed_hash": task.seed.hex(), "difficulty": task.difficulty, "frames": env.frame,
            "max_frames": task.params.max_frames, "success": env.success, "seconds": dt,
            "start": task.start, "progress": env.progress,
            "fps": env.frame / max(dt, 1e-9), "proof": env.proof() if env.success else None,
            "pads_raw": env.proof_raw(), "env": env}


def wait_until_valid(d, address, t, frames, stop, log=print, poll=2):
    """Waits until a run of `frames` frames (over the template's limit) is valid: the daemon's
    `sm64_valid_at` (or, if it does not say, `rules.valid_after` on the retarget difficulty).
    Returns the unix time to timestamp the block with, or None if `stop()` (a new block) came first
    or the run can never be valid."""
    at = 0
    try:
        at = d.valid_at(address, frames)
    except (OSError, RuntimeError, KeyError):
        pass
    if not at and t.parent_timestamp:
        s = rules.valid_after(t.retarget_difficulty or t.difficulty, frames)
        at = t.parent_timestamp + s if s is not None else 0
    if not at:
        return None
    if at > time.time():
        log(f"  {frames} frames exceed the limit of {t.max_frames}; valid from {at} ({at - int(time.time())} s)")
    while time.time() < at:
        if stop():
            return None
        time.sleep(min(poll, max(at - time.time(), 0.05)))
    return max(int(time.time()), at)


def run_miner(bot, daemon_url, address, rom=None, blocks=0, log=print, play_frames=None):
    """Mines with `bot`: template -> bot -> submit, restarting on a new block. The block is
    played from the parent's end state (rebuilt from the daemon's cycle) or power-on, and stamped
    with the time of submission. `play_frames` lets the bot play past the template's limit
    (e.g. 18000, the most any difficulty ever allows): a winning run over the limit waits until the
    limit has grown to it (the time limit grows every second after a grace period), if no block
    arrives meanwhile."""
    load_rom(rom)
    d = Daemon(daemon_url)
    mined = 0
    while True:
        try:
            task, t = d.get_task(address, play_frames=play_frames)
        except OSError as e:
            log(f"daemon: {e}; retrying in 5 s")
            time.sleep(5)
            continue
        log(f"block {t.height} difficulty {t.retarget_difficulty or t.difficulty} limit {t.max_frames} frames, "
            f"{t.random_amount} pokes/100, start {task.start} ({task.start_progress.stars} stars, {task.start_progress.keys} keys)")
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

        threading.Thread(target=watch, daemon=True).start()
        env = MiningEnv(task)
        try:
            bot(env, flag.is_set)
            if not env.success:
                log(f"  no progress (stopped at frame {env.frame}); fetching a new template")
                time.sleep(1)
                continue
            ts = None
            if env.frame > t.max_frames:
                ts = wait_until_valid(d, address, t, env.frame, flag.is_set, log)
                if ts is None:
                    log("  the run did not become valid before the chain moved; fetching a new template")
                    continue
        finally:
            done.set()
        try:
            d.submit(d.seal(t, env.proof(), ts))
            mined += 1
            log(f"  BLOCK {t.height} ACCEPTED ({t.expected_reward / 1e12:.6f} SM64)")
            if blocks and mined >= blocks:
                return
        except RuntimeError as e:
            log(f"  block rejected: {e}")
