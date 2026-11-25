#!/usr/bin/env python3
"""Budget / thread choice as each miner's strategic choice (FAIRNESS.md section 4).
A class of miners (e.g. all the 1.6x machines) picks one strategy for all its machines:
threads per search k (native: 1, 2, 4; browser: 1) and a restart budget b (M frames: a search
that has not found a run after b frames is dropped and a new one started; inf = the default
search, which in the measured runs never gave up). Payoff = the class's block share.
Best responses are iterated class by class (common random numbers: the same seeds for every
candidate) until nobody moves or `rounds` is reached.
usage: fair_game.py OUT.json [--procs 3] [--blocks 1200] [--seeds 3]"""
import argparse, json, math, itertools
from multiprocessing import Pool
import numpy as np
import fair
from fair import Rules, Law, simulate
import fair_run as FR

ap = argparse.ArgumentParser()
ap.add_argument("out"); ap.add_argument("--procs", type=int, default=3)
ap.add_argument("--blocks", type=int, default=1200); ap.add_argument("--seeds", type=int, default=3)
ap.add_argument("--eff", default="1:1,2:0.93,4:0.87")
ap.add_argument("--only", default="")
a, _ = ap.parse_known_args()
EFF = {int(k): float(v) for k, v in (x.split(":") for x in a.eff.split(","))}
BUD = [math.inf, 45.0, 35.0, 28.0, 22.0]


def strategies(kind, cores):
    ks = [1] if kind == "browser" else [k for k in (1, 2, 4) if k <= cores]
    return list(itertools.product(ks, BUD))


def evaluate(job):
    fair.EFF.update(EFF)
    mixname, N, rules, prof, cls, strat = job
    ms, lab = FR.mix(mixname, N)
    prof = dict(prof); prof[cls] = strat
    for m, l in zip(ms, lab):
        if l in prof:
            m.k, m.budget = prof[l]
            m.k = min(m.k, m.cores)
    sh = []
    for s in range(a.seeds):
        r = simulate(ms, Rules(**rules), Law(), blocks=a.blocks, warm=150, seed=500 + s)
        c = FR.classify(dict(share=r["share"], compute=r["compute"]), lab)
        sh.append({k: v["share"] for k, v in c.items()} | {"_mean": r["mean"], "_cv": r["cv"]})
    return (cls, tuple(strat), {k: float(np.mean([x[k] for x in sh])) for k in sh[0]}, FR.classify(dict(share=r["share"], compute=r["compute"]), lab))


def game(pool, mixname, N, rules, rounds=4):
    ms, lab = FR.mix(mixname, N)
    classes = [c for c in dict.fromkeys(lab) if c not in ("bg",)]
    kinds = {l: m for m, l in zip(ms, lab)}
    prof = {c: (kinds[c].k, math.inf) for c in classes}  # default: automine (k=4 native, 1 browser), full budget
    hist = []
    base = evaluate((mixname, N, rules, prof, classes[0], prof[classes[0]]))
    hist.append(dict(profile={c: list(v) for c, v in prof.items()}, shares=base[2]))
    compute = {c: v["compute"] for c, v in base[3].items()}
    for rd in range(rounds):
        moved = False
        for c in classes:
            S = strategies(kinds[c].kind, kinds[c].cores)
            res = pool.map(evaluate, [(mixname, N, rules, prof, c, s) for s in S])
            best = max(res, key=lambda r: r[2][c])
            cur = [r for r in res if r[1] == tuple(prof[c])][0]
            if best[2][c] > cur[2][c] + 0.005 and best[1] != tuple(prof[c]):
                prof[c] = best[1]; moved = True
            hist.append(dict(round=rd, cls=c, options={f"k{s[0]} b{s[1]}": r[2][c] for s, r in zip(S, res)}, chosen=list(prof[c])))
        if not moved:
            break
    fin = evaluate((mixname, N, rules, prof, classes[0], prof[classes[0]]))
    return dict(mix=mixname, N=N, rules=rules, compute=compute, default=hist[0], equilibrium=dict(profile={c: list(v) for c, v in prof.items()}, shares=fin[2]),
                converged=not moved, history=hist)


if __name__ == "__main__":
    games = [("speed", 10), ("speed", 100), ("speed1c", 10), ("browser", 10), ("browser", 100), ("bigsmall", 10), ("box16", 3)]
    if a.only:
        games = [g for g in games if f"{g[0]}:{g[1]}" in a.only.split(",")]
    out = []
    with Pool(a.procs) as pool:
        for (mx, N), G in itertools.product(games, [0, 240]):
            r = game(pool, mx, N, dict(G=G))
            print(mx, N, G, "default", {k: round(v, 3) for k, v in r["default"]["shares"].items()}, "eq", r["equilibrium"]["profile"],
                  {k: round(v, 3) for k, v in r["equilibrium"]["shares"].items()}, "compute", {k: round(v, 3) for k, v in r["compute"].items()}, flush=True)
            out.append(r)
            json.dump(out, open(a.out, "w"), default=float)
