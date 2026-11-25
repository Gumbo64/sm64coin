#!/usr/bin/env python3
"""Tables (slowdecay_tables.md) and plots from slowdecay_results.json. usage: slowdecay_report.py RESULTS.json OUTDIR [human_factor]"""
import json, sys, math
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from slowdecay import SHAPES, CS, ZS, KS, depth_for

res = json.load(open(sys.argv[1])); out = sys.argv[2]
hf = float(sys.argv[3]) if len(sys.argv) > 3 else 2.0
shapes = list(SHAPES)
L = []
def g(kind, *a):
    return res.get("|".join(map(str, (kind,) + a)))

def row(*c): L.append("| " + " | ".join(str(x) for x in c) + " |")

# ---- (a) solo
L.append("## (a) Compute sensitivity: solo block time of an alpha miner at the network's difficulty (s; network = alpha 1)\n")
for C in CS:
    L.append(f"\nC = {C} machines\n")
    row("shape", "log2 d", "network", "33%", "20%", "10%", "10% / network", "run F (net)", "run F (10%)"); row(*["---"] * 9)
    for sh in shapes:
        r = g("solo", sh, C)
        if not r: continue
        n = r["1.0"]["time"]
        row(sh, f"{r['ld']:.1f}", f"{n:.0f}", f"{r['0.33']['time']:.0f}", f"{r['0.2']['time']:.0f}", f"{r['0.1']['time']:.0f}", f"{r['0.1']['time']/n:.2f}", f"{r['1.0']['F']:.0f}", f"{r['0.1']['F']:.0f}")

# ---- (b) attack
L.append("\n## (b) Private-chain double-spend: confirmation depth z for P(success) < 0.1% (tail extrapolated log-linearly), and P at z = 6\n")
depth = {}
for C in CS:
    L.append(f"\nC = {C}. Cells: z(0.1%) / P(z=6) for block-count fork choice (k=0) | weighted k=1 | k=2 | k=4\n")
    row("shape", "alpha", "blocks (k=0)", "k=1", "k=2", "k=4"); row(*["---"] * 6)
    for sh in shapes:
        for al in (0.1, 0.2, 0.33):
            r = g("attack", sh, C, al)
            if not r: continue
            cells = []
            for k in KS:
                pz = {z: r[f"{k}|{z}"] for z in ZS}
                d = depth_for(pz)
                depth[(sh, C, al, k)] = d
                cells.append(f"{'inf' if d is None or d == float('inf') else f'{d:.0f}'} / {pz[6]*100:.1f}%")
            row(sh, al, *cells)

# ---- (c) honest
L.append("\n## (c) Hold time W and the #1 miner (40/30/20/10 compute), H = 120\n")
for C in CS:
    L.append(f"\nC = {C}\n")
    row("shape", "block time", "log2 d", "W mean (s)", "W median", "P(W>1s)", "#1 share (40% compute)", "excess pp", "same-winner", "stale", "winning run F p10/p50/p90"); row(*["---"] * 11)
    for sh in shapes:
        r = g("honest", sh, C)
        if not r: continue
        m = lambda k: np.mean([x[k] for x in r])
        sh1 = np.mean([x["share"][0] for x in r])
        q = np.mean([x["Fw_q"] for x in r], axis=0)
        row(sh, f"{m('blocktime'):.0f}", f"{m('ld'):.1f}", f"{m('W_mean'):.0f}", f"{m('W_med'):.0f}", f"{m('W_pos'):.2f}", f"{sh1:.3f}", f"{(sh1-0.40)*100:+.1f}", f"{m('same_winner'):.2f}", f"{m('stale'):.3f}", f"{q[0]:.0f}/{q[1]:.0f}/{q[2]:.0f}")

# ---- (d) selfish
L.append("\n## (d) Selfish mining (Eyal-Sirer SM1, H = 120, block-count fork choice): the smallest alpha at which selfish share > alpha\n")
row("shape", "C", "gamma 0 threshold", "gamma 0.5 threshold", "share at alpha=0.1 (g0)", "share at 0.25 (g0)", "share at 0.1 (g0.5)"); row(*["---"] * 7)
alphas = [0.1, 0.2, 0.25, 0.3, 0.35, 0.4]
def thr(sh, C, gm):
    xs = [(a, g("selfish", sh, C, a, gm)) for a in alphas]
    if any(v is None for _, v in xs): return None
    for (a0, v0), (a1, v1) in zip(xs, xs[1:]):
        if v0 <= a0 and v1 > a1:
            f = (a0 - v0) / ((v1 - a1) - (v0 - a0))
            return a0 + f * (a1 - a0)
    return "<0.1" if xs[0][1] > xs[0][0] else ">0.4"
for sh in shapes:
    for C in CS:
        t0, t5 = thr(sh, C, 0.0), thr(sh, C, 0.5)
        if t0 is None: continue
        f = lambda t: t if isinstance(t, str) else f"{t:.2f}"
        row(sh, C, f(t0), f(t5), f"{g('selfish', sh, C, 0.1, 0.0):.3f}", f"{g('selfish', sh, C, 0.25, 0.0):.3f}", f"{g('selfish', sh, C, 0.1, 0.5):.3f}")

# ---- (e) retarget
L.append("\n## (e) Retarget stability: interval CV, p95 interval, sd of log2 d; response to a x4 / x1/4 compute step (mean block time over successive 20-block windows after the step)\n")
row("shape", "C", "interval CV", "p95 (s)", "sd log2 d", "x4 step: windows 1-3, 6, 12", "x1/4 step: windows 1-3, 6, 12"); row(*["---"] * 7)
for sh in shapes:
    for C in CS:
        r = g("honest", sh, C); a = g("step", sh, C, 4.0); b = g("step", sh, C, 0.25)
        if not r or not a or not b: continue
        m = lambda k: np.mean([x[k] for x in r])
        pick = lambda s: ", ".join(f"{s['post20'][i]:.0f}" for i in (0, 1, 2, 5, 11) if i < len(s['post20']))
        row(sh, C, f"{m('iv_cv'):.2f}", f"{m('iv_p95'):.0f}", f"{m('ld_sd'):.2f}", pick(a), pick(b))

# ---- humans
L.append(f"\n## Human / manual miners: a human run of F_h = {hf:g} x the search's median first find\n")
j = json.load(open("measure_model.json"))
F1 = float(np.median(j["emp"])); Fh = hf * F1
L.append(f"search first-find median {F1:.0f} frames; human F_h = {Fh:.0f}. P(human beats the winner) = fraction of winning runs with F >= F_h (the human's run would have become valid first); the delay is how much later than the machine winner a run of F_h is valid.\n")
row("shape", "C", "winner F p50", "winner F p10", "P(F_win >= F_h)", "delay of F_h behind a median winner (s)", "delay in blocks of 300 s"); row(*["---"] * 7)
for sh in shapes:
    for C in CS:
        r = g("honest", sh, C)
        if not r: continue
        Fw = np.concatenate([x["Fw"] for x in r])
        spl = SHAPES[sh]["dh"] / 8.0
        dl = spl * math.log(Fh / np.median(Fw)) / (-math.log(0.98))
        row(sh, C, f"{np.median(Fw):.0f}", f"{np.quantile(Fw, 0.1):.0f}", f"{np.mean(Fw >= Fh):.3f}", f"{dl:.0f}", f"{dl/300:.1f}")

# ---- (b2) heavier quality weights, and the floor sensitivity
import os
from slowdecay import KS2
extra = sys.argv[4:]
if extra:
    a2 = json.load(open(extra[0]))
    L.append("\n## (b2) Heavier fork-choice weights: z(0.1%) / P(z=6) for weight (2400/F)^k, k = 0 (blocks), 4, 16, 40, and 'work' = 1/CDF of the fitted single-search law\n")
    for C in CS:
        L.append(f"\nC = {C}\n")
        row("shape", "alpha", *[f"k={k}" for k in KS2]); row(*["---"] * (2 + len(KS2)))
        for sh in shapes:
            for al in (0.1, 0.2, 0.33):
                r = a2.get(f"attack|{sh}|{C}|{al}")
                if not r: continue
                cells = []
                for k in KS2:
                    pz = {z: r[f"{k}|{z}"] for z in ZS}
                    d = depth_for(pz)
                    cells.append(f"{'inf' if d is None or d == float('inf') else f'{d:.0f}'} / {pz[6]*100:.0f}%")
                row(sh, al, *cells)
    for tag, path in zip(("f0 = 1000 (lower floor, fatter tail)", "f0 = 2000 (higher floor)"), extra[1:3]):
        v = json.load(open(path))
        L.append(f"\n## Floor sensitivity, {tag}, C = 100: solo ratio and z(0.1%) for block-count fork choice\n")
        row("shape", "10% solo / network", "z(0.1%) a=0.1", "a=0.2", "a=0.33", "W mean", "interval CV"); row(*["---"] * 7)
        for sh in shapes:
            so = v.get(f"solo|{sh}|100"); ho = v.get(f"honest|{sh}|100")
            if not so: continue
            ds = []
            for al in (0.1, 0.2, 0.33):
                r = v.get(f"attack|{sh}|100|{al}")
                d = depth_for({z: r[f"0|{z}"] for z in ZS}) if r else None
                ds.append("inf" if d is None or d == float("inf") else f"{d:.0f}")
            row(sh, f"{so['0.1']['time']/so['1.0']['time']:.2f}", *ds, f"{np.mean([x['W_mean'] for x in ho]):.0f}", f"{np.mean([x['iv_cv'] for x in ho]):.2f}")

open(f"{out}/slowdecay_tables.md", "w").write("\n".join(L) + "\n")

# ---- plots
fig, axs = plt.subplots(2, 2, figsize=(11, 8))
x = np.arange(len(shapes))
ax = axs[0][0]
for C in CS:
    ys = [g("solo", sh, C)["0.1"]["time"] / g("solo", sh, C)["1.0"]["time"] if g("solo", sh, C) else np.nan for sh in shapes]
    ax.plot(x, ys, "o-", label=f"C={C}")
ax.set_xticks(x); ax.set_xticklabels([s.split("(")[0] for s in shapes], rotation=30); ax.set_ylabel("10% miner solo time / network time"); ax.set_title("(a) compute sensitivity (1 = none)"); ax.legend(); ax.grid(alpha=.3)
ax = axs[0][1]
for k in (0, 4):
    for al, ls in ((0.1, "-"), (0.33, "--")):
        ys = [min(depth.get((sh, 100, al, k), np.nan) or np.nan, 400) for sh in shapes]
        ax.plot(x, ys, ls, marker="o", label=f"alpha {al}, k={k}")
ax.set_yscale("log"); ax.set_xticks(x); ax.set_xticklabels([s.split("(")[0] for s in shapes], rotation=30); ax.set_ylabel("confirmations for <0.1% (C=100)"); ax.set_title("(b) private-chain depth"); ax.legend(fontsize=7); ax.grid(alpha=.3)
ax = axs[1][0]
for C in CS:
    ys = [np.mean([y["W_mean"] for y in g("honest", sh, C)]) if g("honest", sh, C) else np.nan for sh in shapes]
    ax.plot(x, ys, "o-", label=f"C={C}")
ax.set_xticks(x); ax.set_xticklabels([s.split("(")[0] for s in shapes], rotation=30); ax.set_ylabel("W mean (s)"); ax.set_title("(c) hold time"); ax.legend(); ax.grid(alpha=.3)
ax = axs[1][1]
for gm in (0.0, 0.5):
    for C in (100,):
        ys = [thr(sh, C, gm) for sh in shapes]
        ys = [0.0 if (isinstance(v, str) and v.startswith("<")) else (0.5 if isinstance(v, str) else (np.nan if v is None else v)) for v in ys]
        ax.plot(x, ys, "o-", label=f"gamma {gm}, C={C}")
ax.set_xticks(x); ax.set_xticklabels([s.split("(")[0] for s in shapes], rotation=30); ax.set_ylabel("selfish threshold alpha"); ax.set_title("(d) selfish-mining threshold"); ax.legend(); ax.grid(alpha=.3)
plt.tight_layout(); plt.savefig(f"{out}/slowdecay_summary.png", dpi=130)
print("\n".join(L)[:3000])
