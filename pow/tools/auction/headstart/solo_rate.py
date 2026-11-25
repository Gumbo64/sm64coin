#!/usr/bin/env python3
"""How fast could a chain be extended by a miner holding only a fraction alpha of the compute,
at the difficulty the whole network settles on? (the retarget fixes d for block time 300 s at alpha=1)."""
import math, sys
import numpy as np
from hs import *

def block_time(alpha, C, ld, n=300, seed=1, **kw):
    s = Sim([alpha, 1 - alpha] if alpha < 1 else [1.0], C, seed=seed, fixed_ld=ld, H=0, **kw)
    s.dn[0] = 2.0 ** ld
    ts = []
    for i in range(n):
        s.rng = np.random.default_rng(seed * 100003 + i)
        c, v, F, fd, stamp = s.plan(0, 0, 0.0)
        ts.append(v)
    return float(np.mean(ts))

MODELS = {"baseline (fmin 1800, med 6000, sigma 0.8)": dict(),
          "wider spread (fmin 1500, med 6000, sigma 1.2)": dict(fmin=1500.0, sigma=1.2),
          "very wide (fmin 1000, med 8000, sigma 1.5)": dict(fmin=1000.0, med=8000.0, sigma=1.5)}
for name, mk in MODELS.items():
    print(f"\n{name}: mean seconds for a miner holding fraction alpha of the compute, alone, to find a valid block at the network's difficulty\n")
    print("| C | log2 d | alpha=1 | 0.5 | 0.25 | 0.1 | 0.05 | 0.02 |\n|---|---|---|---|---|---|---|---|")
    for C in [3, 30, 100, 300]:
        ld = calibrate_ld([1.0], C, H=0, nb=400, model=Model(**mk))
        row = [C, round(ld, 2)] + [round(block_time(a, C, ld, model=Model(**mk))) for a in [1.0, 0.5, 0.25, 0.1, 0.05, 0.02]]
        print("| " + " | ".join(map(str, row)) + " |")
