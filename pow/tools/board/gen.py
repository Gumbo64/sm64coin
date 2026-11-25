import json, sys, time, urllib.request
N = int(sys.argv[1]); out = sys.argv[2]
def rpc(m, p):
    r = urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:28483/json_rpc",
        json.dumps({"jsonrpc": "2.0", "id": "0", "method": m, "params": p}).encode()), timeout=60)
    j = json.loads(r.read()); assert "error" not in j, j; return j["result"]
T = int(time.time()) - 5
res = []
for i in range(N):
    fn = f"w{i}"
    if i: rpc("create_wallet", {"filename": fn, "password": "", "language": "English"})
    a = rpc("get_address", {})["address"]; v = rpc("query_key", {"key_type": "view_key"})["key"]
    def sig(ts, name): return rpc("sign", {"data": f"sm64coin-board:{ts}:{name}", "signature_type": "spend"})["signature"]
    e = {"address": a, "view_key": v, "name": f"bot{i}", "timestamp": T, "signature": sig(T, f"bot{i}"), "renames": []}
    for k in range(1, 6): e["renames"].append({"name": f"bot{i}-r{k}", "timestamp": T + k, "signature": sig(T + k, f"bot{i}-r{k}")})
    res.append(e)
json.dump(res, open(out, "w"))
