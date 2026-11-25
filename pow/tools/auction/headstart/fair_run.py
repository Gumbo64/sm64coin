#!/usr/bin/env python3
"""Experiments for FAIRNESS.md (progress-aware model in fair.py).
usage: fair_run.py EXP[,EXP..] OUT.json [--procs 4] [--blocks 1500] [--seeds 3]
EXP: mix grace gracec target pof attack backdate  (all: every one)
Each cell is run over --seeds independent seeds; results keep every seed's numbers."""
import argparse, json, math, os, sys, itertools
from multiprocessing import Pool
import numpy as np
import fair
from fair import Miner, Rules, Law, browser, simulate

ap = argparse.ArgumentParser()
ap.add_argument("exps"); ap.add_argument("out")
ap.add_argument("--procs", type=int, default=4); ap.add_argument("--blocks", type=int, default=1500)
ap.add_argument("--seeds", type=int, default=3)
ap.add_argument("--eff", default="", help="threads scaling e.g. 1:1,2:0.93,4:0.9")
ap.add_argument("--law", default="", help="Law overrides e.g. rho=0")
ap.add_argument("--model", default="", help="fair_model.json: take the law and the thread scaling from it")
a = ap.parse_args() if __name__ == "__main__" else ap.parse_args(["-", "-"])


def parse_kv(s):
    d = {}
    for x in filter(None, s.split(",")):
        k, v = x.split("=") if "=" in x else x.split(":")
        d[k] = float(v)
    return d


EFFOVR = {int(k): v for k, v in parse_kv(a.eff).items()}
LAWOVR = parse_kv(a.law)
if a.model:
    _m = json.load(open(a.model))
    fair.LAW.update(_m["law"])
    EFFOVR = {int(k): float(v) for k, v in _m["eff"].items()} | EFFOVR


def law():
    return Law(**LAWOVR)


# ---------------------------------------------------------------- miner mixes
def mix(name, N):
    """Returns (miners, class labels). N = network size in 4-core machines (compute units)."""
    if name == "speed":      # equal cores, per-core speed 1.0 / 1.3 / 1.6 (thirds of the machines)
        n = max(1, N // 3)
        ms = [Miner(f"s{s}-{i}", 4, s) for s in (1.0, 1.3, 1.6) for i in range(n)]
        return ms, [f"x{m.speed:g}" for m in ms]
    if name == "speed1c":    # same but single-core machines (1 thread per search)
        n = max(1, N // 3)
        ms = [Miner(f"s{s}-{i}", 1, s, k=1) for s in (1.0, 1.3, 1.6) for i in range(n)]
        return ms, [f"x{m.speed:g}" for m in ms]
    if name == "box16":      # one 16-core box (4 searches x 4 threads) vs 16 one-core machines, + background
        ms = [Miner("box", 16, 1.0, k=4)] + [Miner(f"one{i}", 1, 1.0, k=1) for i in range(16)]
        lab = ["box16"] + ["1core"] * 16
        bg = max(0, N - 8)
        ms += [Miner(f"bg{i}", 4, 1.0) for i in range(bg)]; lab += ["bg"] * bg
        return ms, lab
    if name == "box16t1":    # the box runs 16 single-thread searches instead
        ms, lab = mix("box16", N)
        ms[0] = Miner("box", 16, 1.0, k=1)
        return ms, lab
    if name == "bigsmall":   # one farm with 40% of the compute + many single 4-core machines
        small = max(1, int(round(N * 0.6)))
        big = max(1, int(round(N * 0.4)))
        ms = [Miner("farm", 4 * big, 1.0, k=4)] + [Miner(f"m{i}", 4, 1.0) for i in range(small)]
        return ms, ["farm"] + ["small"] * small
    if name == "browser":    # half the machines are browsers (4 workers, 1 thread each, 0.5x speed)
        n = max(1, N // 2)
        ms = [Miner(f"n{i}", 4, 1.0) for i in range(n)] + [browser(f"b{i}", 4, 0.5) for i in range(n)]
        return ms, ["native"] * n + ["browser"] * n
    if name == "top4":       # 40/30/20/10 (RESULTS.md's mix), each a farm of 4-core machines
        ws = (0.4, 0.3, 0.2, 0.1)
        ms = [Miner(f"f{int(w*100)}", max(4, 4 * int(round(w * N))), 1.0, k=4) for w in ws]
        return ms, [f"{int(w*100)}%" for w in ws]
    if name == "top4t1":     # the same with single-core machines (1 thread per search)
        ws = (0.4, 0.3, 0.2, 0.1)
        ms = [Miner(f"f{int(w*100)}", max(1, int(round(4 * w * N))), 1.0, k=1) for w in ws]
        return ms, [f"{int(w*100)}%" for w in ws]
    if name.startswith("probe"):  # probe:k:speed -- N 4-thread natives + one small probe miner
        _, k, sp = name.split(":")
        pc = max(4, 4 * int(round(0.05 * N)))  # the probe: ~5% of the compute (25% at N = 3)
        ms = [Miner(f"n{i}", 4, 1.0) for i in range(N)] + [Miner("probe", pc, float(sp), k=int(k))]
        return ms, ["net"] * N + ["probe"]
    raise KeyError(name)


def classify(r, lab):
    out = {}
    sh = np.array(r["share"]); cp = np.array(r["compute"])
    for c in dict.fromkeys(lab):
        m = np.array([x == c for x in lab])
        out[c] = dict(share=float(sh[m].sum()), compute=float(cp[m].sum()), ratio=float(sh[m].sum() / cp[m].sum()))
    return out


def run_cell(cell):
    try:
        return _run_cell(cell)
    except Exception as e:  # keep the sweep going; the failure is reported in the output
        return dict(cell=cell, res=[], error=repr(e))


def _run_cell(cell):
    fair.EFF.update(EFFOVR)
    kind = cell["kind"]
    if kind == "honest":
        ms, lab = mix(cell["mix"], cell["N"])
        for i, m in enumerate(ms):
            for k, v in cell.get("miner", {}).items():
                setattr(m, k, v)
            for c, kv in cell.get("per_class", {}).items():
                if lab[i] == c:
                    for k, v in kv.items():
                        setattr(m, k, v)
        res = []
        for s in range(cell.get("seeds", a.seeds)):
            r = simulate(ms, Rules(**cell.get("rules", {})), law(), blocks=cell.get("blocks", a.blocks), warm=150, seed=1000 * s + 7)
            r["classes"] = classify(r, lab)
            del r["share"], r["compute"], r["ratio"]
            res.append(r)
        return dict(cell=cell, res=res)
    if kind == "selfish":
        res = [fair.selfish(cell["alpha"], rules=Rules(**cell.get("rules", {})), law=law(), gamma=cell["gamma"],
                            blocks=cell.get("blocks", 2000), seed=s, scale=cell.get("scale", 1.0),
                            H_honest=cell.get("H_honest", 120.0)) for s in range(cell.get("seeds", 2))]
        return dict(cell=cell, res=res)
    if kind == "race":
        r = fair.private_race(cell["alpha"], rules=Rules(**cell.get("rules", {})), law=law(), trials=cell.get("trials", 400),
                              K=cell.get("K", 120), seed=cell.get("seed", 0), scale=cell.get("scale", 1.0),
                              H_honest=cell.get("H_honest", 120.0))
        r["z"] = fair.z_of(r["P"])
        return dict(cell=cell, res=[r])
    raise KeyError(kind)


NS = [3, 10, 30, 100, 1000]
MIXES = ["speed", "speed1c", "box16", "box16t1", "bigsmall", "browser"]
GS = [0, 60, 120, 180, 240, 480, 900]


def cells(exp):
    out = []
    if exp == "mix":  # fairness vs network size, today's rules
        for mx, N in itertools.product(MIXES + ["top4"], NS):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N))
    if exp == "grace":
        for mx, N, G in itertools.product(MIXES + ["top4"], [3, 10, 100], GS):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N, rules=dict(G=G)))
    if exp == "target":  # longer block target instead of (or with) a grace
        for mx, N, T in itertools.product(["speed", "speed1c", "box16t1", "browser", "box16"], [10, 100, 1000], [300, 450, 600, 900, 1200]):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N, rules=dict(target=T)))
    if exp == "gracec":  # grace with start_halvings lowered by G/60: the floor limit at time G is today's
        for mx, N, G in itertools.product(["speed1c", "browser", "speed"], [10, 100], [0, 120, 240, 480]):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N, rules=dict(G=G, sh=20 - G // 60)))
    if exp == "threshold":  # a small probe miner's share/compute vs its per-search latency
        for N, k, sp, rl in itertools.product([3, 10, 100, 1000], [1, 4], [0.25, 0.5, 0.75, 1.0, 1.3, 1.6, 2.0, 3.0],
                                              [dict(), dict(G=240), dict(G=480), dict(target=600), dict(rule="pof", omega=60), dict(rule="pof", omega=120)]):
            if N == 1000 and len(rl) > 0:
                continue
            out.append(dict(kind="honest", exp=exp, mix=f"probe:{k}:{sp}", N=N, rules=rl, miner=dict(H=-1) if rl.get("rule") == "pof" else {}))
    if exp == "stamps":  # grace vs timestamp games in a one-thread-search network (speed1c)
        sc = {"none": {}, "fast-bd": {"x1.6": dict(bd=True)}, "all-bd": {"x1": dict(bd=True), "x1.3": dict(bd=True), "x1.6": dict(bd=True)},
              "slow-fd": {"x1": dict(fd=120)}, "fast-bd+slow-fd": {"x1.6": dict(bd=True), "x1": dict(fd=120)}}
        for N, G, (nm, pc), fr in itertools.product([10, 100], [0, 240, 480, 900], sc.items(), [math.inf, 30, 60, 120]):
            if nm in ("none", "slow-fd") and fr != math.inf:
                continue
            out.append(dict(kind="honest", exp=exp, mix="speed1c", N=N, rules=dict(G=G, fresh=fr), per_class=pc, scen=nm))
        for N, G in itertools.product([10, 100], [0, 240, 480, 900]):
            out.append(dict(kind="honest", exp=exp, mix="speed1c", N=N, rules=dict(G=G, clock="valid"), scen="clock=valid"))
    if exp == "pof":
        for mx, N, om, H, G in itertools.product(MIXES + ["top4"], [3, 10, 100], [30, 60, 120], [-1, 120], [0, 240]):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N, rules=dict(rule="pof", omega=om, G=G), miner=dict(H=H)))
    if exp in ("attack", "attack2"):
        rls = [dict(), dict(G=240), dict(rule="pof", omega=60), dict(rule="pof", omega=120), dict(rule="pof", omega=60, G=240)]
        if exp == "attack2":
            rls = [dict(target=600), dict(G=480), dict(rule="pof", omega=60, target=600)]
        for rl in rls:
            Hh = -1 if rl.get("rule") == "pof" else 120.0
            for al, g in itertools.product([0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4], [0.0, 0.5]):
                if rl.get("rule") == "pof" and g > 0:
                    continue  # lead-1 races are decided by run length, gamma plays no part
                out.append(dict(kind="selfish", exp=exp, alpha=al, gamma=g, rules=rl, H_honest=Hh))
            for al in [0.1, 0.2, 0.33]:
                out.append(dict(kind="race", exp=exp, alpha=al, rules=rl, H_honest=Hh, trials=400, K=150 if al > 0.3 else 80))
    if exp == "backdate":  # the 30% farm withholds D s and back-dates its stamp; the others are honest
        for mx, N, G, D, fr in itertools.product(["top4", "top4t1"], [10, 100], [0, 120, 240, 480], [0, 30, 60, 120, 240], [math.inf, 30, 60, 120]):
            out.append(dict(kind="honest", exp=exp, mix=mx, N=N, rules=dict(G=G, fresh=fr), per_class={"30%": dict(D=D, bd=True)}))
    return out


if __name__ == "__main__":
    exps = ["mix", "grace", "gracec", "target", "pof", "attack", "backdate"] if a.exps == "all" else a.exps.split(",")
    todo = [c for e in exps for c in cells(e)]
    print(len(todo), "cells", flush=True)
    done = []
    with Pool(a.procs) as p:
        for i, r in enumerate(p.imap_unordered(run_cell, todo)):
            done.append(r)
            if i % 20 == 0:
                print(i, flush=True)
                json.dump(done, open(a.out, "w"), default=float)
    json.dump(done, open(a.out, "w"), default=float)
    print("done", flush=True)
