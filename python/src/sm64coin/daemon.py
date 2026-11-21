"""Daemon JSON-RPC client."""

import json
import urllib.request
from dataclasses import dataclass


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

    def get_template(self, address) -> Template:
        r = self.call("get_block_template", {"wallet_address": address, "reserve_size": 0})
        return Template(
            blocktemplate_blob=r["blocktemplate_blob"], difficulty=r["difficulty"], height=r["height"],
            prev_hash=r["prev_hash"], expected_reward=r["expected_reward"], seed_hash=r["sm64_seed_hash"],
            reseed_frames=r["sm64_reseed_frames"], max_frames=r["sm64_max_frames"],
            random_amount=r["sm64_random_amount"], easier_at=r.get("sm64_easier_at", 0),
            rules=r.get("sm64_rules", ""), goal=r.get("sm64_goal", ""))

    def get_task(self, address):
        """(Task, Template) for the next block."""
        from .env import Task
        t = self.get_template(address)
        return Task.from_template(t), t

    def top_hash(self) -> str:
        return self.call("get_info").get("top_block_hash", "")

    def seal(self, template, proof: bytes) -> str:
        return seal(template.blocktemplate_blob, proof)

    def submit(self, block_hex: str):
        self.call("submit_block", [block_hex])


def seal(template_hex: str, proof: bytes) -> str:
    """The template blob with `proof` in place of its empty proof (the blob's last byte, 0)."""
    blob = bytearray.fromhex(template_hex)
    if not blob or blob.pop() != 0:
        raise ValueError("template does not end with an empty proof; is this an SM64Coin daemon?")
    n = len(proof)
    while n >= 0x80:
        blob.append((n & 0x7F) | 0x80)
        n >>= 7
    blob.append(n)
    blob += proof
    return blob.hex()
