#!/usr/bin/env python3
"""make-vectors.py [NODE_RPC] > in.jsonl: the real chain's proofs as input for
pow/sm64pow/examples/proofid_vectors.rs (frames, start and the decoded input pads of every block; state-coded
blocks are decoded by the node, `sm64_played`). Default node http://127.0.0.1:29781."""
import json, sys, urllib.request

node = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:29781"

def rpc(method, params=None):
    req = urllib.request.Request(node + "/json_rpc", json.dumps({"jsonrpc": "2.0", "id": "0", "method": method, "params": params or {}}).encode())
    return json.load(urllib.request.urlopen(req, timeout=60))["result"]

def varint(b, i=0):
    v = s = 0
    while True:
        c = b[i]; i += 1
        v |= (c & 0x7F) << s; s += 7
        if not c & 0x80: return v, i

h = rpc("get_info")["height"]
for height in range(1, h):
    r = rpc("get_block", {"height": height, "sm64_played": True})
    n = r["sm64_proof_bytes"]
    proof = bytes.fromhex(r["blob"])[-n:]
    frames, i = varint(proof)
    if not r["sm64_pads"] or frames == 0 or frames > 36000:
        continue
    print(json.dumps({"name": f"h{height}-c{proof[i + 1]}", "frames": frames, "start": proof[i], "pads": r["sm64_pads"]}))
