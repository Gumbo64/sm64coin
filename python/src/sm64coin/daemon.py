"""Daemon JSON-RPC client."""

import json
import time
import urllib.request
from dataclasses import dataclass, field
from typing import List

from .proof import pad_from, varint
from .sign import Keypair, sign_proof


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
    key: Keypair = None             # the miner key the template was requested with
    tip: str = ""                   # the chain's tip when the template was made: usually prev_hash, not when the node
                                    # builds around an equivocated tip, so "a new block landed" = the tip moved from THIS


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


MAX_RESPONSE = 64 << 20


class Daemon:
    def __init__(self, url="http://127.0.0.1:16481", timeout=60):
        self.url, self.timeout = url.rstrip("/"), timeout

    def call(self, method, params=None):
        body = json.dumps({"jsonrpc": "2.0", "id": 0, "method": method, "params": params or {}}).encode()
        req = urllib.request.Request(self.url + "/json_rpc", body, {"Content-Type": "application/json"})
        # `timeout` is per socket operation; the deadline covers the whole call, and the answer is
        # size-limited (a hostile node can't stall or flood us).
        deadline = time.monotonic() + self.timeout
        chunks, size = [], 0
        with urllib.request.urlopen(req, timeout=self.timeout) as r:
            while True:
                chunk = r.read(65536)
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > MAX_RESPONSE:
                    raise RuntimeError(f"{method}: response too large")
                if time.monotonic() > deadline:
                    raise TimeoutError(f"{method}: timed out after {self.timeout} s")
        resp = json.loads(b"".join(chunks))
        if "error" in resp:
            raise RuntimeError(f"{method}: {resp['error'].get('message', 'error')}")
        return resp["result"]

    def get_template(self, address, key: Keypair, frames=None) -> Template:
        """The block template for the miner key `key` (v0.3: its `extra_nonce()` goes into the coinbase,
        so the seed covers the key; use the same key for every template call of one run). With
        `frames`, `valid_at` says when a run of that many frames becomes valid (use only that
        field: a fresh template may carry another seed)."""
        p = {"wallet_address": address, "extra_nonce": key.extra_nonce().hex()}
        if frames is not None:
            p["sm64_frames"] = int(frames)
        try:
            tip = self.top_hash()
        except (OSError, RuntimeError):
            tip = ""
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
            valid_at=r.get("sm64_valid_at", 0), key=key, tip=tip or r["prev_hash"])

    def valid_at(self, address, key, frames) -> int:
        """Unix time from which a run of `frames` frames is valid for the next block."""
        return self.get_template(address, key, frames).valid_at

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

    def get_task(self, address, key: Keypair = None, start=None, play_frames=None):
        """(Task, Template) for the next block, requested for the miner `key` (default: a fresh
        `Keypair.generate()`; the template keeps it as `template.key`). The task starts from the
        parent's end state (rebuilt from `cycle`) or power-on, as the template allows (`start` forces one)."""
        from .env import Task
        t = self.get_template(address, key or Keypair.generate())
        s = start or ("parent" if t.start in ("parent", "either") else "power-on")
        cycle = self.cycle(t.prev_hash) if s == "parent" else None
        return Task.from_template(t, cycle, s, play_frames), t

    def top_hash(self) -> str:
        return self.call("get_info").get("top_block_hash", "")

    def seal(self, template, proof: bytes, timestamp=None, key: Keypair = None) -> str:
        """The signed, submit-ready block: signs `proof` with the template's key (or `key`)."""
        key = key or template.key
        sig = sign_proof(key, bytes.fromhex(template.seed_hash), proof)
        return seal(template.blocktemplate_blob, proof, sig, timestamp)

    def submit(self, block_hex: str):
        self.call("submit_block", [block_hex])


def seal(template_hex: str, proof: bytes, sig: bytes, timestamp=None) -> str:
    """The template blob (v0.4: it ends with the empty proof, a 0 byte, the empty signature, 64
    zero bytes, and the empty proof records, a 0 byte: the daemon reattaches the template's records)
    with `proof` and its signature `sig` (`sign.sign_proof`) in their place, and the block
    header's timestamp replaced by `timestamp` (unix seconds; default: now). The seed does not depend
    on the timestamp, so a miner stamps its block when it submits and the signature stays valid.
    Header: varint major, varint minor, varint timestamp, prev_id (32), nonce (4), ..."""
    blob = bytes.fromhex(template_hex)
    if len(blob) < 66 or any(blob[-66:]):
        raise ValueError("template does not end with an empty proof, signature and proof records; is this an SM64Coin v0.4 daemon?")
    if len(sig) != 64:
        raise ValueError("a signature is 64 bytes")
    blob = blob[:-66]
    _, i = get_varint_loose(blob, 0)   # major version
    _, i = get_varint_loose(blob, i)   # minor version
    _, j = get_varint_loose(blob, i)   # timestamp
    ts = int(time.time()) if timestamp is None else int(timestamp)
    return (blob[:i] + varint(ts) + blob[j:] + varint(len(proof)) + bytes(proof) + bytes(sig) + b"\x00").hex()


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
