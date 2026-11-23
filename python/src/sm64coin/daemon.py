"""Daemon JSON-RPC client."""

import json
import time
import urllib.request
from dataclasses import dataclass, field
from typing import List

from .proof import pad_from, varint


@dataclass
class Template:
    blocktemplate_blob: str
    difficulty: int
    height: int
    prev_hash: str
    expected_reward: int
    seed_hash: str
    reseed_frames: int
    max_frames: int
    random_amount: int
    easier_at: int = 0
    rules: str = ""
    goal: str = ""
    window_length: int = 0
    burst_length: int = 0
    timestamp: int = 0              # the template's own timestamp (sm64_timestamp)
    retarget_difficulty: int = 0    # the difficulty the time limit derives from
    start: str = "parent"           # where the run may start: "parent", "power-on" or "either"
    power_on_at: int = 0            # unix time from which a power-on restart is allowed (parent + restart delay)
    restart_delay: int = 0          # a restart's limit is the limit this many seconds earlier
    parent_timestamp: int = 0
    parent_stars: int = 0
    parent_keys: int = 0
    cycle_start_height: int = 0
    valid_at: int = 0               # only when asked with `frames`: when a run of that many frames is valid


@dataclass
class CycleBlock:
    height: int
    hash: str
    frames: int
    power_on: bool


@dataclass
class Cycle:
    """The current cycle (`get_sm64_cycle`): its blocks and the pad played on every frame from the
    cycle's first block to the tip. Replaying `pads` UNPERTURBED from power-on reaches the tip's
    end state, the next block's start."""
    blocks: List[CycleBlock] = field(default_factory=list)
    pads: list = field(default_factory=list)
    stars: int = 0
    keys: int = 0
    grand_star: bool = False
    ends_cycle: bool = False


class Daemon:
    def __init__(self, url="http://127.0.0.1:16481", timeout=60):
        self.url, self.timeout = url.rstrip("/"), timeout

    def call(self, method, params=None):
        body = json.dumps({"jsonrpc": "2.0", "id": 0, "method": method, "params": params or {}}).encode()
        req = urllib.request.Request(self.url + "/json_rpc", body, {"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=self.timeout) as r:
            resp = json.load(r)
        if "error" in resp:
            raise RuntimeError(f"{method}: {resp['error'].get('message', 'error')}")
        return resp["result"]

    def get_template(self, address, frames=None) -> Template:
        """The block template. With `frames`, `valid_at` says when a run of that many frames
        becomes valid (use only that field: a fresh template may carry another seed)."""
        p = {"wallet_address": address, "reserve_size": 0}
        if frames is not None:
            p["sm64_frames"] = int(frames)
        r = self.call("get_block_template", p)
        return Template(
            blocktemplate_blob=r["blocktemplate_blob"], difficulty=r["difficulty"], height=r["height"],
            prev_hash=r["prev_hash"], expected_reward=r["expected_reward"], seed_hash=r["sm64_seed_hash"],
            reseed_frames=r["sm64_reseed_frames"], max_frames=r["sm64_max_frames"],
            random_amount=r["sm64_random_amount"], easier_at=r.get("sm64_easier_at", 0),
            rules=r.get("sm64_rules", ""), goal=r.get("sm64_goal", ""),
            window_length=r.get("sm64_window_length", 0), burst_length=r.get("sm64_burst_length", 0),
            timestamp=r.get("sm64_timestamp", 0), retarget_difficulty=r.get("sm64_retarget_difficulty", 0),
            start=r.get("sm64_start", "parent"), power_on_at=r.get("sm64_power_on_at", 0), restart_delay=r.get("sm64_restart_delay", 0),
            parent_timestamp=r.get("sm64_parent_timestamp", 0), parent_stars=r.get("sm64_parent_stars", 0),
            parent_keys=r.get("sm64_parent_keys", 0), cycle_start_height=r.get("sm64_cycle_start_height", 0),
            valid_at=r.get("sm64_valid_at", 0))

    def valid_at(self, address, frames) -> int:
        """Unix time from which a run of `frames` frames is valid for the next block."""
        return self.get_template(address, frames).valid_at

    def cycle(self, tip=None) -> Cycle:
        """The cycle ending at block `tip` (hash; default the chain's top): its blocks and every
        played pad (replay them unperturbed from power-on to get the tip's end state)."""
        r = self.call("get_sm64_cycle", {"hash": tip} if tip else {})
        raw = bytes.fromhex(r.get("played_pads", ""))
        if len(raw) % 4:
            raise ValueError("played_pads is not a whole number of pads")
        return Cycle(
            blocks=[CycleBlock(b.get("height", 0), b.get("hash", ""), b.get("frames", 0), bool(b.get("power_on", False)))
                    for b in r.get("blocks", [])],
            pads=[pad_from(raw[i:i + 4]) for i in range(0, len(raw), 4)],
            stars=r.get("stars", 0), keys=r.get("keys", 0), grand_star=bool(r.get("grand_star", False)),
            ends_cycle=bool(r.get("ends_cycle", False)))

    def get_task(self, address, start=None, play_frames=None):
        """(Task, Template) for the next block. The task starts from the parent's end state
        (rebuilt from `cycle`) or power-on, as the template allows (`start` forces one)."""
        from .env import Task
        t = self.get_template(address)
        s = start or ("parent" if t.start in ("parent", "either") else "power-on")
        cycle = self.cycle(t.prev_hash) if s == "parent" else None
        return Task.from_template(t, cycle, s, play_frames), t

    def top_hash(self) -> str:
        return self.call("get_info").get("top_block_hash", "")

    def seal(self, template, proof: bytes, timestamp=None) -> str:
        return seal(template.blocktemplate_blob, proof, timestamp)

    def submit(self, block_hex: str):
        self.call("submit_block", [block_hex])


def seal(template_hex: str, proof: bytes, timestamp=None) -> str:
    """The template blob with `proof` in place of its empty proof (the blob's last byte, 0), and
    the block header's timestamp replaced by `timestamp` (unix seconds; default: now). The seed no
    longer depends on the timestamp, so a miner stamps its block when it submits. Header:
    varint major, varint minor, varint timestamp, prev_id (32), nonce (4), ..."""
    blob = bytes.fromhex(template_hex)
    if not blob or blob[-1] != 0:
        raise ValueError("template does not end with an empty proof; is this an SM64Coin daemon?")
    blob = blob[:-1]
    _, i = get_varint_loose(blob, 0)   # major version
    _, i = get_varint_loose(blob, i)   # minor version
    _, j = get_varint_loose(blob, i)   # timestamp
    ts = int(time.time()) if timestamp is None else int(timestamp)
    return (blob[:i] + varint(ts) + blob[j:] + varint(len(proof)) + bytes(proof)).hex()


def get_varint_loose(b: bytes, i: int):
    """Monero varint (little-endian base 128), not necessarily canonical."""
    v, shift = 0, 0
    while True:
        if i >= len(b):
            raise ValueError("truncated block header")
        byte = b[i]
        i += 1
        v |= (byte & 0x7F) << shift
        if byte & 0x80 == 0:
            return v, i
        shift += 7
        if shift > 63:
            raise ValueError("header varint too long")
