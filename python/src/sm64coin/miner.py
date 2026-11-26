"""Mining loop and offline practice. A bot is a function `bot(env, stop) -> None`
that steps `env` until `env.is_success()` (or `stop()` turns true) and returns."""

import json
import os
import threading
import time

from . import rules
from .daemon import Daemon
from .sign import Keypair, proof_id
from .payout import Payouts
from .env import MiningEnv, Task, load_rom


class SignedStore:
    """One run per key (v0.3): the miner key of a template may sign at most ONE run for that template's
    seed (a second different signed run is equivocation: honest nodes avoid the block). This remembers
    seed hash -> (proof id, frames) in a JSON file so a restart never signs a second run for a seed.
    `path=None` or "" keeps it in memory only. Signing the very same run again is fine (same block)."""

    KEEP = 256

    def __init__(self, path=None):
        self.path = path or None
        self.map = {}
        try:
            if self.path:
                with open(self.path) as f:
                    self.map = json.load(f)
        except (OSError, ValueError):
            self.map = {}

    def signed_frames(self, seed_hash, proof):
        """None if `proof` may be signed for `seed_hash`; else the frames of the different run already signed."""
        old = self.map.get(seed_hash)
        if old and old["proof_id"] != proof_id(proof).hex():
            return old["frames"]
        return None

    def record(self, seed_hash, proof, frames):
        """Remembers `proof` for `seed_hash` (saved before the caller signs)."""
        self.map[seed_hash] = {"proof_id": proof_id(proof).hex(), "frames": int(frames), "at": int(time.time())}
        while len(self.map) > self.KEEP:
            del self.map[min(self.map, key=lambda k: self.map[k].get("at", 0))]
        if self.path:
            d = os.path.dirname(self.path)
            if d:
                os.makedirs(d, exist_ok=True)
            tmp = self.path + ".tmp"
            with open(tmp, "w") as f:
                json.dump(self.map, f)
            os.replace(tmp, self.path)


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


def wait_until_valid(d, address, t, frames, stop, log=print, poll=2, key=None, lead=0):
    """Waits until a run of `frames` frames (over the template's limit) is valid: the daemon's
    `sm64_valid_at` (or, if it does not say, `rules.valid_after` on the retarget difficulty).
    Returns when it is `lead` seconds before that moment (the unix time to stamp the block with, the
    second the run becomes valid, or now if later), or None if `stop()` (a new block) came first or
    the run can never be valid."""
    at = 0
    try:
        at = d.valid_at(address, key or t.key, frames)
    except (OSError, RuntimeError, KeyError):
        pass
    if not at and t.parent_timestamp:
        s = rules.valid_after(t.retarget_difficulty or t.difficulty, frames)
        at = t.parent_timestamp + s if s is not None else 0
    if not at:
        return None
    if at > time.time():
        log(f"  {frames} frames exceed the limit of {t.max_frames}; valid from {at} ({at - int(time.time())} s)")
    while time.time() + lead < at:
        if stop():
            return None
        time.sleep(min(poll, max(at - lead - time.time(), 0.05)))
    return max(int(time.time()), at)


def proof_verdict(v):
    return {1: "in the pool", 0: "ignored (known, not for the node's tip height, outside the window, or pool full)",
            -2: "the node lacks the parent state", -3: "bad encoding"}.get(v, "refused")


def publish_proof(d, signed, t, proof, frames, log=print, ts=None):
    """v0.4 stage B: a run that lost its height (another block became the tip first, or it was displaced or
    refused) is still a proof for that height. Signs it with the template's key (ONE run per key: a different run
    already signed for this seed is never signed, the very same one is fine), checks the archive window
    (`rules.window_frames`: at most 60 s of auction time after the winner's due time; the node has the last
    word) and offers it with `submit_sm64_proof`. Returns the node's verdict (1 pooled), or None if not sent."""
    prev = signed.signed_frames(t.seed_hash, proof)
    if prev is not None:
        log(f"  not publishing as a proof: a different run ({prev} frames) was already signed for this seed")
        return None
    w = d.winner_frames(t.height)
    if w is not None:
        limit = rules.window_frames(t.retarget_difficulty or t.difficulty, w)
        if frames > limit:
            log(f"  not a proof for height {t.height}: {frames} frames is outside the archive window "
                f"(the winner has {w}, the window ends at {limit})")
            return None
    signed.record(t.seed_hash, proof, frames)
    try:
        v, why = d.submit_proof(d.seal(t, proof, ts))
    except (OSError, RuntimeError, ValueError) as e:
        log(f"  publishing the run as a proof failed: {e}")
        return None
    log(f"  published as a proof for height {t.height} ({frames} frames): {proof_verdict(v)}"
        + (f" ({why})" if why else "") + ("; an included proof pays an extra coinbase output to its payout address" if v == 1 else ""))
    return v


def run_miner(bot, daemon_url, address, rom=None, blocks=0, log=print, play_frames=None, next_key=None,
              submit_lead=5, signed_file=None, payout_view_secret=None, publish_proofs=True):
    """Mines with `bot`: template -> bot -> submit, restarting on a new block. The block is
    played from the parent's end state (rebuilt from the daemon's cycle) or power-on, and stamped
    with the time of submission. `play_frames` lets the bot play past the template's limit
    (e.g. 36000, the most any difficulty ever allows): a winning run over the limit waits until the
    limit has grown to it (the time limit grows every second after a grace period), if no block
    arrives meanwhile. Every block is signed by a per-template key (v0.3): a fresh `Keypair.generate()`
    each round, or `next_key(round) -> Keypair` (e.g. the secret a group leader shared).

    ONE RUN PER KEY: a key signs at most one run per template (a second different signed run is
    equivocation, and honest nodes avoid the block). The run is signed and submitted once,
    `submit_lead` seconds before it is valid (already valid: at once), stamped with the second it
    becomes valid. `signed_file` (default `$SM64COIN_SIGNED_FILE` or ./sm64coin-signed.json) remembers
    which seeds were signed, so a restart never signs a second run for a seed; pass "" for memory only.

    v0.4 (docs/V0.4-DESIGN.md): the template for a new tip is taken only after the node's
    `sm64_records_ready_at` (so the block includes the previous height's proofs); with `payout_view_secret`
    (hex, the wallet's secret view key for `address`) each template commits a fresh payout subaddress (0, k),
    k from a counter in `<signed_file>.payout`, else the template's payout is `address`; and a run that lost
    its height (a new block first, a displacement, a refusal) is published as a proof (`publish_proof`) unless
    `publish_proofs` is false."""
    load_rom(rom)
    # Restricted (public) RPC refuses a run due more than 30 s after it arrives (rules.rs RPC_SUBMIT_AHEAD_SECONDS).
    submit_lead = max(0, min(int(submit_lead), 25))
    if signed_file is None:
        signed_file = os.environ.get("SM64COIN_SIGNED_FILE", "sm64coin-signed.json")
    signed = SignedStore(signed_file)
    d = Daemon(daemon_url)
    payouts = Payouts(address, payout_view_secret, signed_file) if payout_view_secret else None
    mined = 0
    rounds = 0
    while True:
        try:
            key = next_key(rounds) if next_key else Keypair.generate()
            rounds += 1
            task, t = d.get_task(address, key, play_frames=play_frames, payout=payouts.next() if payouts else None,
                                 wait_ready=True, log=log)
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
                    if dd.top_hash() != (t.tip or t.prev_hash):
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
                ts = wait_until_valid(d, address, t, env.frame, flag.is_set, log, key=key, lead=submit_lead)
                if ts is None:
                    log("  the run did not become valid before the chain moved; fetching a new template")
                    if publish_proofs and flag.is_set():
                        publish_proof(d, signed, t, env.proof(), env.frame, log)
                    continue
        finally:
            done.set()
        proof = env.proof()
        prev = signed.signed_frames(t.seed_hash, proof)
        if prev is not None:
            log(f"  already signed a different run ({prev} frames) for this seed: not signing a second one; waiting for a new block")
            while True:
                try:
                    if d.top_hash() != (t.tip or t.prev_hash):
                        break
                except OSError:
                    pass
                time.sleep(2)
            continue
        signed.record(t.seed_hash, proof, env.frame)
        try:
            d.submit(d.seal(t, proof, ts))
            mined += 1
            log(f"  BLOCK {t.height} ACCEPTED ({t.expected_reward / 1e12:.6f} SM64)")
            if blocks and mined >= blocks:
                return
        except RuntimeError as e:
            log(f"  block rejected: {e}")
            if publish_proofs:  # the same signed run (no second signature), offered as a proof
                publish_proof(d, signed, t, proof, env.frame, log, ts)
