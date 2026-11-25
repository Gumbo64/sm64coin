import json, sys, os, time, subprocess, urllib.request, shutil, random
S = os.path.dirname(os.path.abspath(__file__))
binary, tag, nblocks_per = sys.argv[1], sys.argv[2], int(sys.argv[3])   # blocks per board wallet
NB = 40
ROM = os.environ.get("SM64COIN_ROM", os.path.join(S, "../../../baserom.us.z64"))
ents = json.load(open(os.environ.get("BOARD_WALLETS", f"{S}/wallets.json")))   # made by gen.py
D = os.path.join(os.environ.get("BOARD_TMP", "/tmp"), f"sm64board-{tag}"); shutil.rmtree(D, ignore_errors=True); os.makedirs(D)
port = 28500 + (0 if tag == "base" else 10)
URL = f"http://127.0.0.1:{port+1}/json_rpc"
p = subprocess.Popen([binary, "--regtest", "--offline", "--sm64-rom", ROM, "--data-dir", D, "--fixed-difficulty", "1",
    "--rpc-bind-port", str(port+1), "--p2p-bind-port", str(port), "--zmq-rpc-bind-port", str(port+2), "--non-interactive",
    "--log-file", f"{D}/d.log", "--log-level", "1"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, preexec_fn=os.setsid, env=dict(os.environ, SM64_TESTONLY_TS="1"))
import atexit; atexit.register(lambda: os.killpg(os.getpgid(p.pid), 15) if p.poll() is None else None)
open(f"{D}/pid", "w").write(str(p.pid))
def rpc(m, params=None, t=120):
    r = urllib.request.urlopen(urllib.request.Request(URL, json.dumps({"jsonrpc": "2.0", "id": "0", "method": m, "params": params or {}}).encode()), timeout=t)
    return json.loads(r.read())
def pop(n=1):
    r = urllib.request.urlopen(urllib.request.Request(URL.replace("/json_rpc", "/pop_blocks"), json.dumps({"nblocks": n}).encode()), timeout=60)
    return json.loads(r.read())
def cpu():
    f = open(f"/proc/{p.pid}/stat").read().rsplit(")", 1)[1].split(); return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")
for _ in range(60):
    try: rpc("get_info"); break
    except Exception: time.sleep(1)
out = {}
def log(k, v): out[k] = v; print(k, v, flush=True)
# fixture chain: round-robin blocks, board wallets 0..NB-1, filler wallet 149
heights = {i: [] for i in range(NB)}
h = 1
for rnd in range(nblocks_per):
    for i in range(NB):
        r = rpc("generateblocks", {"wallet_address": ents[i]["address"], "amount_of_blocks": 1})
        assert "result" in r, r
        heights[i].append(h); h += 1
    if rnd % 10 == 0:
        rpc("generateblocks", {"wallet_address": ents[149]["address"], "amount_of_blocks": 5}); h += 5
log("height", rpc("get_info")["result"]["height"])
time.sleep(25)
c0 = cpu(); time.sleep(20); log("idle_cpu_20s", round(cpu() - c0, 3))
def join(e, name=None, ts=None, sig=None, hs=None):
    prm = {"name": name or e["name"], "address": e["address"], "view_key": e["view_key"], "timestamp": ts or e["timestamp"], "signature": sig or e["signature"]}
    if hs is not None: prm["heights"] = hs
    return rpc("sm64_board_join", prm)["result"]
def board(): return rpc("sm64_board")["result"].get("entries", [])
# A. honest joins
c0 = cpu(); t0 = time.time()
res = [join(ents[i], hs=heights[i]) for i in range(NB)]
log("A_honest_joins_replied_s", round(time.time() - t0, 2))
log("A_honest_replies", {"joined": sum(1 for r in res if r["joined"]), "queued": sum(1 for r in res if r.get("queued"))})
for _ in range(60):
    if len(board()) >= NB: break
    time.sleep(1)
log("A_board_size", len(board())); log("A_honest_cpu_s", round(cpu() - c0, 3))
# B. join flood: valid wallets that never mined (40..148), attacker claims random heights
time.sleep(5)
c0 = cpu(); t0 = time.time(); n = 0; reasons = {}
for rep in range(2):
    for i in range(NB, 149):
        hs = sorted(random.sample(range(1, h), 3)) if rep == 0 else None
        r = join(ents[i], hs=hs); n += 1
        k = (r["reason"] or "ok")[:40]; reasons[k] = reasons.get(k, 0) + 1
log("B_join_flood_requests", n); log("B_join_flood_wall_s", round(time.time() - t0, 2)); log("B_reasons", reasons)
time.sleep(25); log("B_join_flood_cpu_s", round(cpu() - c0, 3))
log("B_board_size_after", len(board()))
# C. rename flood: every board member sends its 5 renames in order, twice
c0 = cpu(); t0 = time.time(); acc = 0; tot = 0
for rep in range(2):
    for i in range(NB):
        for rn in ents[i]["renames"]:
            r = join(ents[i], name=rn["name"], ts=rn["timestamp"], sig=rn["signature"], hs=heights[i]); tot += 1
            acc += 1 if r["joined"] and r["reason"] != "already on the board" else 0
log("C_rename_requests", tot); log("C_rename_accepted", acc)
time.sleep(25); log("C_rename_cpu_s", round(cpu() - c0, 3))
# D. reorg flips: pop 1 block, mine 1 (different address -> new block id), repeat
c0 = cpu(); t0 = time.time()
for k in range(3):
    print("pop", pop(1), flush=True)
    print("gen", str(rpc("generateblocks", {"wallet_address": ents[100 + k]["address"], "amount_of_blocks": 1}))[:150], flush=True)
    time.sleep(22 if tag == "base" else 4)
log("D_3_reorgs_cpu_s", round(cpu() - c0, 3)); log("D_wall_s", round(time.time() - t0, 1))
log("board_top3", [(e["name"], e["blocks"]) for e in board()[:3]])
json.dump(out, open(f"{D}/result.json", "w"))
os.killpg(os.getpgid(p.pid), 15); p.wait(30)
