import json, sys, os, time, subprocess, urllib.request, shutil, atexit
S = os.path.dirname(os.path.abspath(__file__)); binary = sys.argv[1]
ROM = os.environ.get("SM64COIN_ROM", os.path.join(S, "../../../baserom.us.z64"))
ents = json.load(open(os.environ.get("BOARD_WALLETS", f"{S}/wallets.json")))   # made by gen.py
D = os.path.join(os.environ.get("BOARD_TMP", "/tmp"), "sm64board-func"); shutil.rmtree(D, ignore_errors=True); os.makedirs(D)
port = 28530; URL = f"http://127.0.0.1:{port+1}/json_rpc"
p = None
def start(extra=()):
    global p
    p = subprocess.Popen([binary, "--regtest", "--offline", "--sm64-rom", ROM, "--data-dir", D, "--fixed-difficulty", "1",
        "--rpc-bind-port", str(port+1), "--p2p-bind-port", str(port), "--zmq-rpc-bind-port", str(port+2), "--non-interactive",
        "--log-file", f"{D}/d.log", "--log-level", "1", *extra], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, preexec_fn=os.setsid, env=dict(os.environ, SM64_TESTONLY_TS="1"))
    for _ in range(60):
        try: rpc("get_info"); return
        except Exception: time.sleep(1)
def stop():
    os.killpg(os.getpgid(p.pid), 15); p.wait(60)
atexit.register(lambda: p and p.poll() is None and os.killpg(os.getpgid(p.pid), 15))
def rpc(m, params=None):
    r = urllib.request.urlopen(urllib.request.Request(URL, json.dumps({"jsonrpc": "2.0", "id": "0", "method": m, "params": params or {}}).encode()), timeout=60)
    return json.loads(r.read())
def pop(n=1):
    r = urllib.request.urlopen(urllib.request.Request(URL.replace("/json_rpc", "/pop_blocks"), json.dumps({"nblocks": n}).encode()), timeout=60)
    return json.loads(r.read())
def mine(i, n=1): assert "result" in rpc("generateblocks", {"wallet_address": ents[i]["address"], "amount_of_blocks": n})
def join(i, hs, name=None, ts=None, sig=None):
    e = ents[i]; prm = {"name": name or e["name"], "address": e["address"], "view_key": e["view_key"], "timestamp": ts or e["timestamp"], "signature": sig or e["signature"], "heights": hs}
    return rpc("sm64_board_join", prm)["result"]
def board(): r = rpc("sm64_board")["result"]; return {x["address"]: x for x in r.get("entries", [])}, r.get("disabled", False)
def wait(cond, t=30):
    for _ in range(t):
        if cond(): return True
        time.sleep(1)
    return False
ok = True
def check(name, c):
    global ok; ok &= bool(c); print(("PASS " if c else "FAIL ") + name, flush=True)
start()
mine(0, 1); mine(1, 1); mine(0, 1); mine(2, 1); mine(0, 1)   # heights: w0: 1,3,5  w1: 2  w2: 4
for _ in range(2): mine(149, 1)                               # 6,7
r = join(0, [1, 3, 5]); check("honest join queued", r.get("queued") or r.get("joined"))
check("w0 on the board with 3 blocks", wait(lambda: board()[0].get(ents[0]["address"], {}).get("blocks") == 3))
r = join(3, [1]); check("lying heights refused (w3 claims w0's block)", wait(lambda: not r.get("joined")) )
time.sleep(3); check("w3 not on the board", ents[3]["address"] not in board()[0])
r = join(1, []); check("no heights: update your wallet", "update your wallet" in r["reason"])
r = join(1, [2, 2]); check("duplicate heights invalid", not r.get("joined") and not r.get("queued"))
r = join(1, [99]); check("height above chain refused", not r.get("joined") and not r.get("queued"))
r = join(1, [2]); check("w1 queued", r.get("queued")); wait(lambda: ents[1]["address"] in board()[0])
# rename: first accepted, second within 10 min refused
e = ents[0]; rn = e["renames"]
r = join(0, [1, 3, 5], rn[0]["name"], rn[0]["timestamp"], rn[0]["signature"]); check("rename accepted", r.get("joined") and not r["reason"])
check("renamed on board", board()[0][e["address"]]["name"] == rn[0]["name"])
r = join(0, [1, 3, 5], rn[1]["name"], rn[1]["timestamp"], rn[1]["signature"]); check("2nd rename within 10 min refused", not r.get("joined") and "recently" in r["reason"])
# live update: a new block for w1, no rescan
mine(1, 1); check("live hit counted", wait(lambda: board()[0][ents[1]["address"]]["blocks"] == 2, 15))
# reorg: pop that block, mine elsewhere -> w1 back to 1 block
pop(1); mine(5, 1)
check("reorg forgets the hit", wait(lambda: board()[0][ents[1]["address"]]["blocks"] == 1, 15))
check("others intact after reorg", board()[0][ents[0]["address"]]["blocks"] == 3)
# off switch
r = rpc("sm64_board_set", {"enabled": False})["result"]; check("switch off", r["enabled"] is False)
b, dis = board(); check("board reports disabled", dis is True)
r = join(2, [4]); check("join refused when off", not r.get("joined") and "turned off" in r["reason"])
mine(0, 1)  # a block while off
rpc("sm64_board_set", {"enabled": True}); check("catches up the block mined while off", wait(lambda: board()[0][ents[0]["address"]]["blocks"] == 4, 15))
# restart: persisted, not re-verified from scratch
time.sleep(61); mine(0, 1); wait(lambda: board()[0][ents[0]["address"]]["blocks"] == 5, 10)
stop(); start()
check("restart keeps the board with counts", wait(lambda: board()[0].get(ents[0]["address"], {}).get("blocks") == 5, 20))
stop(); start(["--sm64-no-leaderboard"])
b, dis = board(); check("--sm64-no-leaderboard: disabled", dis is True)
stop()
print("ALL PASS" if ok else "SOME FAILED")
