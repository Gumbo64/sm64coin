#!/usr/bin/env python3
"""Bootstrap the pooled min-of-n fit L(n)=a n^-b + floor (resample the single searches), and the per-doubling gains."""
import json, sys
import numpy as np
from scipy.optimize import curve_fit
rows = [json.loads(l) for l in open(sys.argv[1])]
F = np.array([r["frames"] for r in rows if r.get("status") == "found" and r["arm"] in "SG"], float)
rng = np.random.default_rng(7)
f = lambda n, a, b, fl: a * n ** -b + fl
def curve(F, nmax=16, trials=300):
    return np.array([np.mean([F[rng.choice(len(F), n, replace=False)].min() for _ in range(trials)]) for n in range(1, nmax + 1)])
ns = np.arange(1, 17.0)
fl, bs, gains = [], [], []
for _ in range(60):
    Fb = rng.choice(F, len(F))
    c = curve(Fb)
    try:
        p, _ = curve_fit(f, ns, c, p0=[c[0] - c[-1], 0.7, c[-1] * 0.9], bounds=([0, 0.01, 0], [1e6, 5, c.min()]), maxfev=20000)
        fl.append(p[2]); bs.append(p[1])
    except Exception:
        pass
    gains.append([c[1] / c[0] - 1, c[3] / c[1] - 1, c[7] / c[3] - 1, c[15] / c[7] - 1])
c = curve(F)
print(f"N={len(F)} floor fit: median {np.median(fl):.0f}, 10-90% {np.percentile(fl,10):.0f}-{np.percentile(fl,90):.0f}; exponent b median {np.median(bs):.2f} ({np.percentile(bs,10):.2f}-{np.percentile(bs,90):.2f})")
g = np.array(gains)
print("change of the best run per doubling of compute (1->2, 2->4, 4->8, 8->16 searches): " + ", ".join(f"{np.mean(g[:,i])*100:.1f}% ({np.percentile(g[:,i],10)*100:.1f}..{np.percentile(g[:,i],90)*100:.1f})" for i in range(4)))
