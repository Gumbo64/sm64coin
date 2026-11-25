"""(v0.4 design, docs/V0.4-DESIGN.md section 2.4) v0.4 fork-choice healing variants on top of fairness-research fair_fork.py (dff3dbf), target 1200 only.
usage: heal.py race|part OUT.json [--procs 5] [--seeds 8] [--trials 200]"""
import sys, json, argparse
sys.path.insert(0, "/workspaces/sm64-combined/sm64coin-fair/pow/tools/auction/headstart")
import numpy as np
import fair_fork as ff
from multiprocessing import Pool

HEAL = [6, 12, 18]
ff.RULES = ["height", "proofs", "and_proofs", "cap36", "persist02", "persist05", "persist10", "persist20", "persist10_proofs"] + [f"heal{d}" for d in HEAL]
ff.PRULES = ["height", "proofs", "and_proofs", "cap36"] + [f"heal{d}" for d in HEAL]
_prefer, _switch = ff.prefer, ff.switch

def prefer(rule, a, h, cmA, cmH, wA, wH, dH):
    if rule.startswith("heal"):
        D = int(rule[4:])
        return ((a > h) & (wA > wH)) | ((np.minimum(a, h) >= D) & (wA > wH))
    return _prefer(rule, a, h, cmA, cmH, wA, wH, dH)

def switch(rule, hOwn, hOth, cmOwn, cmOth, wOwn, wOth):
    if rule.startswith("heal"):
        D = int(rule[4:])
        return (hOth > hOwn and wOth > wOwn) or (min(hOwn, hOth) >= D and wOth > wOwn)
    return _switch(rule, hOwn, hOth, cmOwn, cmOth, wOwn, wOth)

ff.prefer, ff.switch = prefer, switch

def race_w(c): return ff.race_cell(c)
def part_w(c): return ff.part_cell(c)

if __name__ == "__main__":
    ap = argparse.ArgumentParser(); ap.add_argument("what"); ap.add_argument("out")
    ap.add_argument("--procs", type=int, default=5); ap.add_argument("--seeds", type=int, default=8); ap.add_argument("--trials", type=int, default=200)
    a = ap.parse_args()
    cells = []
    if a.what == "race":
        for al in (0.2, 0.33):
            for s in range(a.seeds):
                cells.append(dict(alpha=al, target=1200.0, seed=s, trials=a.trials, K=150 if al > 0.3 else 100))
        fn = race_w
    else:
        for (fr, att) in [(0.3, False), (0.5, False), (0.33, True)]:
            for T in (10800.0, 36000.0):
                for s in range(a.seeds * 4):
                    cells.append(dict(frac=fr, att=att, T=T, target=1200.0, seed=s, tail=40 * 1200.0))
        fn = part_w
    print(len(cells), "cells", flush=True)
    done = []
    with Pool(a.procs) as p:
        for i, r in enumerate(p.imap_unordered(fn, cells)):
            done.append(r)
            if i % 10 == 0: print(i, flush=True)
    json.dump(done, open(a.out, "w")); print("done", flush=True)
