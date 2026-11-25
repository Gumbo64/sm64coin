#!/usr/bin/env python3
"""Tables (fair_tables.md) and plots (fair_*.png) for FAIRNESS.md from fair_run.py / fair_game.py output.
usage: fair_report.py RESULTS.json[,MORE.json..] [GAME.json]"""
import json, sys, os, math, collections
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

HERE = os.path.dirname(os.path.abspath(__file__))
rows = []
for f in sys.argv[1].split(","):
    rows += json.load(open(f))
game = json.load(open(sys.argv[2])) if len(sys.argv) > 2 and os.path.exists(sys.argv[2]) else []
OUT = open(os.path.join(HERE, "fair_tables.md"), "w")
C = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#6250d6", "#e34948"]
INK, INK2, GRID, SURF = "#0b0b0b", "#52514e", "#e4e3df", "#fcfcfb"
plt.rcParams.update({"axes.edgecolor": INK2, "axes.labelcolor": INK, "xtick.color": INK2, "ytick.color": INK2,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.8, "axes.spines.top": False,
                     "axes.spines.right": False, "font.size": 10, "figure.facecolor": SURF, "axes.facecolor": SURF,
                     "lines.linewidth": 2, "lines.markersize": 6})


def P(*a):
    print(*a, file=OUT)


def agg(res, key):
    v = [r[key] for r in res if r.get(key) is not None]
    return float(np.mean(v)) if v else float("nan")


def cls(res):
    d = collections.defaultdict(list)
    for r in res:
        for k, v in r["classes"].items():
            d[k].append(v["ratio"])
    return {k: (float(np.mean(v)), float(np.std(v))) for k, v in d.items()}


def rkey(c):
    return json.dumps(c.get("rules", {}), sort_keys=True) + json.dumps(c.get("miner", {}), sort_keys=True) + json.dumps(c.get("per_class", {}), sort_keys=True, default=str)


H = [x for x in rows if x["cell"]["kind"] == "honest"]
by = collections.defaultdict(dict)
for x in H:
    c = x["cell"]
    by[c["exp"]][(c["mix"], c["N"], rkey(c))] = x

METRICS = "mean | cv | p95 | W | stale | F | log2 d"


def mline(res):
    return f"{agg(res,'mean'):.0f} | {agg(res,'cv'):.2f} | {agg(res,'p95'):.0f} | {agg(res,'W'):.0f} | {100*agg(res,'stale'):.1f}% | {agg(res,'F'):.0f} | {agg(res,'log2d'):.1f}"


def ratios(res):
    return ", ".join(f"{k} {m:.2f}" for k, (m, s) in cls(res).items())


MIXDESC = {"speed": "equal 4-core machines, per-core speed 1.0/1.3/1.6 (4 threads per search)",
           "speed1c": "equal 1-core machines, per-core speed 1.0/1.3/1.6 (1 thread per search)",
           "box16": "one 16-core box (4 searches x 4 threads) vs 16 one-core machines (+ background 4-core natives when N > 8)",
           "box16t1": "the same, the box running 16 one-thread searches",
           "bigsmall": "one farm with 40% of the compute vs 4-core machines",
           "browser": "half the machines browsers (4 workers x 1 thread, 0.5x speed) vs 4-core natives",
           "top4": "40/30/20/10 farms of 4-core machines", "top4t1": "40/30/20/10 farms of 1-core machines"}

if "mix" in by:
    P("## mix: share / compute per class, today's rules (G = 0, hold), by network size N (4-core machine units)\n")
    for mx in MIXDESC:
        ks = sorted([k for k in by["mix"] if k[0] == mx], key=lambda k: k[1])
        if not ks:
            continue
        P(f"### {mx}: {MIXDESC[mx]}\n\n| N | share / compute | {METRICS} |\n|---|---|---|---|---|---|---|---|---|")
        for k in ks:
            r = by["mix"][k]["res"]; P(f"| {k[1]} | {ratios(r)} | {mline(r)} |")
        P()

for exp, title, field in [("grace", "decay_grace G (start_halvings 20)", "G"), ("gracec", "G with start_halvings 20 - G/60", "G"), ("target", "target block time", "target")]:
    if exp not in by:
        continue
    P(f"## {exp}: {title}\n")
    for mx in MIXDESC:
        for N in sorted({k[1] for k in by[exp] if k[0] == mx}):
            ks = [k for k in by[exp] if k[0] == mx and k[1] == N]
            if not ks:
                continue
            ks.sort(key=lambda k: json.loads(k[2].split("}")[0] + "}").get(field, 0))
            P(f"### {mx}, N = {N}\n\n| {field} | share / compute | {METRICS} |\n|---|---|---|---|---|---|---|---|---|")
            for k in ks:
                r = by[exp][k]["res"]; v = by[exp][k]["cell"]["rules"].get(field, 0)
                P(f"| {v} | {ratios(r)} | {mline(r)} |")
            P()

if "pof" in by:
    P("## pof: signed proofs + publish-on-find, shorter run displaces within omega; H = -1: everyone waits for the window, H = 120: authors start early\n")
    for mx in MIXDESC:
        for N in sorted({k[1] for k in by["pof"] if k[0] == mx}):
            ks = sorted([k for k in by["pof"] if k[0] == mx and k[1] == N])
            if not ks:
                continue
            P(f"### {mx}, N = {N}\n\n| G | omega | H | share / compute | {METRICS} | displaced |\n|---|---|---|---|---|---|---|---|---|---|---|---|")
            for k in sorted(ks, key=lambda k: (by["pof"][k]["cell"]["rules"]["G"], by["pof"][k]["cell"]["rules"]["omega"], by["pof"][k]["cell"]["miner"]["H"])):
                c = by["pof"][k]["cell"]; r = by["pof"][k]["res"]
                P(f"| {c['rules']['G']} | {c['rules']['omega']} | {c['miner']['H']} | {ratios(r)} | {mline(r)} | {100*agg(r,'displaced'):.0f}% |")
            P()

if "backdate" in by:
    P("## backdate: the 30% farm withholds D s and back-dates its stamp (as early as its run, or the freshness bound, allows); others honest\n")
    P("share/compute of the 30% farm; gap = publication - stamp of the winning block (s, mean over all blocks); wall = mean publication interval\n")
    for mx in ("top4", "top4t1"):
        for N in (10, 100):
            ks = [k for k in by["backdate"] if k[0] == mx and k[1] == N]
            if not ks:
                continue
            P(f"### {mx}, N = {N}\n\n| G | fresh | D | 30% share/compute | others | gap | wall |\n|---|---|---|---|---|---|---|")
            def sk(k):
                c = by["backdate"][k]["cell"]; return (c["rules"]["G"], c["rules"]["fresh"], c["per_class"]["30%"]["D"])
            for k in sorted(ks, key=sk):
                c = by["backdate"][k]["cell"]; r = by["backdate"][k]["res"]; cl = cls(r)
                P(f"| {c['rules']['G']} | {c['rules']['fresh']} | {c['per_class']['30%']['D']} | {cl['30%'][0]:.2f} | " + ", ".join(f"{a} {b[0]:.2f}" for a, b in cl.items() if a != "30%") + f" | {agg(r,'gap'):.0f} | {agg(r,'wall'):.0f} |")
            P()

if "stamps" in by:
    P("## stamps: grace vs timestamp games, one-core 1.0/1.3/1.6x network (speed1c); bd = back-date (as early as allowed), fd = forward-date +120 s\n")
    P("| N | G | scenario | fresh | share/compute x1 / x1.3 / x1.6 | block (stamps) | block (wall) | gap |\n|---|---|---|---|---|---|---|---|")
    def sk(k):
        c = by["stamps"][k]["cell"]; return (c["N"], c["rules"]["G"], c.get("scen", ""), str(c["rules"].get("fresh", "")))
    for k in sorted(by["stamps"], key=sk):
        c = by["stamps"][k]["cell"]; r = by["stamps"][k]["res"]; cl = cls(r)
        P(f"| {c['N']} | {c['rules']['G']} | {c.get('scen')} | {c['rules'].get('fresh', '-')} | {cl['x1'][0]:.2f} / {cl['x1.3'][0]:.2f} / {cl['x1.6'][0]:.2f} | {agg(r,'mean'):.0f} | {agg(r,'wall'):.0f} | {agg(r,'gap'):.0f} |")
    P()

# ---------------------------------------------------------------- attacks
S = [x for x in rows if x["cell"]["kind"] == "selfish"]
R = [x for x in rows if x["cell"]["kind"] == "race"]
RULENAME = lambda rl: ("pof w=%g" % rl["omega"] if rl.get("rule") == "pof" else "hold") + (" G=%g" % rl["G"] if rl.get("G") else "") + (" target %g" % rl["target"] if rl.get("target") else "")
if S:
    P("## selfish mining (SM1), selfish revenue share vs alpha (C = 100 machines, 8 honest miners)\n")
    tab = collections.defaultdict(dict)
    for x in S:
        c = x["cell"]; tab[(RULENAME(c["rules"]), c["gamma"])][c["alpha"]] = float(np.mean([r["share"] for r in x["res"]]))
    al = sorted({a for d in tab.values() for a in d})
    P("| rule | gamma | " + " | ".join(str(a) for a in al) + " | threshold |\n|---|---|" + "---|" * (len(al) + 1))
    thr = {}
    for (rn, g), d in sorted(tab.items()):
        t = next((a for a in al if d.get(a, 0) > a), None)
        thr[(rn, g)] = t
        P(f"| {rn} | {g} | " + " | ".join(f"{d.get(a, float('nan')):.3f}" for a in al) + f" | {t if t is not None else '>0.4'} |")
    P()
    fig, ax = plt.subplots(figsize=(7, 4.2))
    keep = [("hold", 0.0), ("hold", 0.5), ("hold G=240", 0.5), ("hold target 600", 0.5), ("pof w=60", 0.0), ("pof w=120", 0.0)]
    for i, (rn, g) in enumerate([k for k in keep if k in tab]):
        d = tab[(rn, g)]
        xs = sorted(d); ax.plot(xs, [d[a] for a in xs], marker="o", color=C[i % len(C)], label=f"{rn}, gamma {g}" if "pof" not in rn else rn)
    ax.plot([0.1, 0.4], [0.1, 0.4], color=INK2, lw=1, ls="--", label="honest share = alpha")
    ax.set_xlabel("selfish miner's compute share alpha"); ax.set_ylabel("selfish miner's block share")
    ax.set_title("Selfish mining (SM1) under hold / publish-on-find rules", color=INK, loc="left")
    ax.legend(fontsize=8, frameon=False); fig.tight_layout(); fig.savefig(os.path.join(HERE, "fair_selfish.png"), dpi=130)
if R:
    P("## private chain from the tip: z for P(success) < 0.1% (and P at z = 6)\n\n| rule | alpha | z(0.1%) | P(z=6) |\n|---|---|---|---|")
    for x in sorted(R, key=lambda x: (RULENAME(x["cell"]["rules"]), x["cell"]["alpha"])):
        r = x["res"][0]; P(f"| {RULENAME(x['cell']['rules'])} | {x['cell']['alpha']} | {r['z']} | {r['P'][6]:.3f} |")
    P()

# ---------------------------------------------------------------- threshold plot
if "threshold" in by:
    import fair
    def latency(k, sp):  # mean seconds to finish one search
        m = fair.Miner("x", 4, sp, k=k)
        L = fair.LAW
        ET = (L["T0"] + math.exp(L["muT"] + L["sT"] ** 2 / 2)) * 1e6
        return ET / m.rate
    P("## threshold: a probe miner (5% of the compute) - share/compute vs its mean time to finish one search\n")
    rules = sorted({by["threshold"][k]["cell"].get("rules", {}).__repr__() for k in by["threshold"]})
    rl_list = []
    for k, x in by["threshold"].items():
        rl = json.dumps(x["cell"].get("rules", {}), sort_keys=True)
        if rl not in rl_list:
            rl_list.append(rl)
    rl_list.sort(key=lambda s: (len(s), s))
    nc = 3; nr = (len(rl_list) + nc - 1) // nc
    fig, axs = plt.subplots(nr, nc, figsize=(11, 3.4 * nr), sharey=True, squeeze=False)
    for ai, rl in enumerate(rl_list):
        ax = axs[ai // nc][ai % nc]
        P(f"### rules {rl}\n\n| N | probe threads | speed | mean search s | probe share/compute | block mean s |\n|---|---|---|---|---|---|")
        for ni, N in enumerate([3, 10, 100, 1000]):
            pts = []
            for k, x in by["threshold"].items():
                c = x["cell"]
                if c["N"] != N or json.dumps(c.get("rules", {}), sort_keys=True) != rl:
                    continue
                _, kk, sp = c["mix"].split(":")
                lat = latency(int(kk), float(sp)); rr = cls(x["res"])["probe"][0]
                pts.append((lat, rr, int(kk), float(sp), agg(x["res"], "mean")))
            if not pts:
                continue
            pts.sort()
            for p in pts:
                P(f"| {N} | {p[2]} | {p[3]} | {p[0]:.0f} | {p[1]:.2f} | {p[4]:.0f} |")
            ax.plot([p[0] for p in pts], [p[1] for p in pts], marker="o", ls="none" if False else "-", color=C[ni], label=f"N = {N}")
        P()
        ax.set_xscale("log"); ax.axhline(1, color=INK2, lw=1, ls="--")
        rd = json.loads(rl)
        nm = ("pof w=%g" % rd["omega"]) if rd.get("rule") == "pof" else ("G=%g" % rd["G"] if "G" in rd else ("target %g" % rd["target"] if "target" in rd else "today"))
        ax.set_title(nm, color=INK, loc="left", fontsize=10); ax.set_xlabel("probe's mean search time (s)")
        if ai % nc == 0:
            ax.set_ylabel("probe share / compute share")
        if ai == 0:
            ax.legend(fontsize=8, frameon=False)
    fig.suptitle("Small-miner threshold: payoff vs time to finish one search", x=0.01, ha="left", color=INK)
    fig.tight_layout(); fig.savefig(os.path.join(HERE, "fair_threshold.png"), dpi=130)

# ---------------------------------------------------------------- grace plot
if "grace" in by:
    fig, axs = plt.subplots(1, 2, figsize=(10, 3.8))
    series = [("speed1c", 10, "x1"), ("speed1c", 100, "x1"), ("browser", 10, "browser"), ("box16", 10, "1core"), ("speed", 10, "x1")]
    for i, (mx, N, c) in enumerate(series):
        ks = [k for k in by["grace"] if k[0] == mx and k[1] == N]
        if not ks:
            continue
        pts = sorted((by["grace"][k]["cell"]["rules"]["G"], cls(by["grace"][k]["res"])[c][0], agg(by["grace"][k]["res"], "mean")) for k in ks)
        lab = f"{mx} N={N}: {c}"
        axs[0].plot([p[0] for p in pts], [p[1] for p in pts], marker="o", color=C[i], label=lab)
        axs[1].plot([p[0] for p in pts], [p[2] for p in pts], marker="o", color=C[i], label=lab)
    axs[0].axhline(1, color=INK2, lw=1, ls="--"); axs[0].set_xlabel("decay_grace G (s)"); axs[0].set_ylabel("slow class: block share / compute")
    axs[1].set_xlabel("decay_grace G (s)"); axs[1].set_ylabel("mean block time (s)"); axs[0].legend(fontsize=8, frameon=False)
    axs[0].set_title("Fairness for the slow class", loc="left", color=INK, fontsize=10); axs[1].set_title("Price: block time (d pinned at 1)", loc="left", color=INK, fontsize=10)
    fig.tight_layout(); fig.savefig(os.path.join(HERE, "fair_grace.png"), dpi=130)

# ---------------------------------------------------------------- game
if game:
    P("## budget / thread game: default vs best-response equilibrium (block share; compute share)\n")
    P("| mix | N | G | class | compute | default share | eq. strategy (k threads, restart budget M) | eq. share | converged |\n|---|---|---|---|---|---|---|---|---|")
    for g in game:
        for c, cp in g["compute"].items():
            if c == "bg":
                continue
            P(f"| {g['mix']} | {g['N']} | {g['rules'].get('G', 0)} | {c} | {cp:.3f} | {g['default']['shares'].get(c, float('nan')):.3f} | {g['equilibrium']['profile'].get(c)} | {g['equilibrium']['shares'].get(c, float('nan')):.3f} | {g['converged']} |")
    P()
OUT.close()
print("wrote fair_tables.md")
