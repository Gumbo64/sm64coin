#!/usr/bin/env python3
"""Fits the progress-aware search law for fair.py from every measured search.
usage: fair_fit.py FAIRWORK [out.json]
Inputs: measure_results.jsonl (MEASURE.md's 36 S/G searches, 1 thread, loaded machine) and
FAIRWORK/raw/*.jsonl + FAIRWORK/steps/*.log (this branch's runs: thread scaling, course budgets,
step logs written by `runlen --steplog`). Prints the tables quoted in FAIRNESS.md and writes
fair_model.json (joint law of (cost T, run length F), thread scaling, per-step costs)."""
import glob, json, math, os, re, sys
import numpy as np
from scipy import stats

HERE = os.path.dirname(os.path.abspath(__file__))
W = sys.argv[1]
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "fair_model.json")

old = [json.loads(l) for l in open(os.path.join(HERE, "measure_results.jsonl"))]
old = [r for r in old if r.get("arm") in ("S", "G") and r.get("status") == "found"]
new = []
for f in sorted(glob.glob(W + "/raw/*.jsonl")):
    for l in open(f):
        r = json.loads(l)
        tag = r["tag"]
        r["target"] = tag.split("-")[0]
        m = re.search(r"-C(\d+)$", tag)
        r["cb"] = int(m.group(1)) if m else None
        new.append(r)


def steps_of(path):
    """[(kind, zone, sim_start, sim_end, outcome)] of one search's step log."""
    out = []; cur = None
    for line in open(path):
        p = line.rstrip("\n").split("\t", 3)
        if len(p) < 4:
            continue
        t, sim, k, msg = float(p[0]), int(p[1]), int(p[2]), p[3]
        m = re.match(r"step (\d+): (\S+) at frame", msg)
        if m:
            if cur:
                out.append(cur + [sim, "?"])
            cur = ["castle" if m.group(2) == "castle" else "course", m.group(2), sim, t]
            continue
        if cur is None:
            continue
        if msg.startswith("  progress at frame"):
            out.append(cur + [sim, "star"]); cur = None
        elif re.match(r"  \S+ at frame \d+ \(\+\d+\), [\d.]+ M frames", msg) and cur[0] == "castle":
            out.append(cur + [sim, "reached"]); cur = None
        elif "checkpoint" in msg and "; checkpoint" in msg:
            out.append(cur + [sim, "checkpoint"]); cur = None
        elif "back to the castle" in msg or "did not reach" in msg or "found no new course" in msg:
            out.append(cur + [sim, "fail"]); cur = None
    return out


steps = {}
for f in sorted(glob.glob(W + "/steps/*.log")):
    steps[os.path.basename(f)[:-4]] = steps_of(f)

# ---------------------------------------------------------------- thread scaling (same search, k threads)
print("## thread scaling (pairs: same target, same search seed)")
pairs = {}
for r in new:
    if r["cb"] is None and r.get("status") == "found":
        pairs.setdefault((r["target"], r["sseed"]), {})[r["threads"]] = r
eff = {1: [], 2: [], 4: []}
for key, d in sorted(pairs.items()):
    if len(d) > 1:
        row = {k: (d[k]["frames"], round(d[k]["sim_frames"] / 1e6, 1), round(d[k]["secs"])) for k in sorted(d)}
        print(key, row)
        base = None
        if 1 in d:
            base = d[1]["secs"]
            for k in (2, 4):
                if k in d:
                    eff[k].append(base / d[k]["secs"] / k)
        elif 2 in d and 4 in d:
            eff[4].append(d[2]["secs"] / d[4]["secs"] / 2 * (np.mean(eff[2]) if eff[2] else 0.93))
EFF = {1: 1.0}
for k in (2, 4):
    if eff[k]:
        EFF[k] = float(np.mean(eff[k]))
print("efficiency per thread:", {k: round(v, 3) for k, v in EFF.items()}, "n =", {k: len(v) for k, v in eff.items()})
fps = [r["sim_frames"] / r["secs"] / r["threads"] / EFF.get(r["threads"], 1) for r in new if r.get("status") == "found" and r["cb"] is None]
print("wall frames/s per thread (load ~20 on 16 cores): median %.0f" % np.median(fps))

# ---------------------------------------------------------------- joint law (cost, run length)
pool = [(r["target"], r["sim_frames"] / 1e6, r["frames"]) for r in old]
seen = set()
for r in new:  # independent new searches: default budget, one per (target, search seed)
    if r["cb"] is None and r.get("status") == "found" and (r["target"], r["sseed"]) not in seen:
        seen.add((r["target"], r["sseed"])); pool.append((r["target"], r["sim_frames"] / 1e6, r["frames"]))
tg = np.array([p[0] for p in pool]); T = np.array([p[1] for p in pool]); F = np.array([p[2] for p in pool], float)
print(f"\n## joint law, N = {len(T)} searches (36 MEASURE.md + {len(T) - 36} new)")
sh, loc, sc = stats.lognorm.fit(T)
print("T (M frames): mean %.1f median %.1f min %.1f max %.1f; 3-par lognormal shift %.2f scale %.2f sigma %.3f" % (T.mean(), np.median(T), T.min(), T.max(), loc, sc, sh))
f0 = 1500.0
x = np.log(F - f0)
print("F: mean %.0f median %.0f sd %.0f; F - 1500 lognormal mu %.3f sigma %.3f" % (F.mean(), np.median(F), F.std(), x.mean(), x.std(ddof=1)))
z1 = []; z2 = []
for t in sorted(set(tg)):
    m = tg == t; n = m.sum()
    z1 += list(stats.norm.ppf((stats.rankdata(T[m]) - .5) / n)); z2 += list(stats.norm.ppf((stats.rankdata(F[m]) - .5) / n))
rho = float(np.corrcoef(z1, z2)[0, 1])
sp = stats.spearmanr(T, F).statistic
bo = []
rng = np.random.default_rng(1)
z1 = np.array(z1); z2 = np.array(z2)
for _ in range(2000):
    i = rng.integers(0, len(z1), len(z1)); bo.append(np.corrcoef(z1[i], z2[i])[0, 1])
print("cost vs run length: Spearman %.2f (pooled), within-target normal-score rho %.2f (bootstrap 10-90%%: %.2f..%.2f)" % (sp, rho, *np.percentile(bo, [10, 90])))
# hazard of finding: success within budget b (one search, default miner)
print("\n## one search: P(found within b M frames), cost per find, mean run of the finds (restart at b)")
for b in [16, 18, 20, 22, 25, 28, 30, 35, 40, 50, 60, 80]:
    p = (T <= b).mean(); et = np.minimum(T, b).mean()
    print("b %3d  P %.2f  cost/find %6.1f M  mean F|found %s" % (b, p, et / p if p else float("inf"), "%.0f" % F[T <= b].mean() if p else "-"))

# ---------------------------------------------------------------- steps
print("\n## steps (from the step logs)")
cast = []; course = []; nsteps = []; outcomes = {}
for tag, st in steps.items():
    if not st:
        continue
    nsteps.append(len(st))
    for kind, zone, s0, t0, s1, oc in st:
        outcomes[(kind, oc)] = outcomes.get((kind, oc), 0) + 1
        if kind == "castle" and oc == "reached":
            cast.append((s1 - s0) / 1e6)
        if kind == "course":
            course.append(((s1 - s0) / 1e6, oc, tag))
print("searches with step logs:", len(nsteps), " steps per search:", dict(zip(*np.unique(nsteps, return_counts=True))))
print("outcomes:", outcomes)
if cast:
    print("castle step (reached a course): n %d, M frames mean %.1f median %.1f min %.1f max %.1f" % (len(cast), np.mean(cast), np.median(cast), min(cast), max(cast)))
cs = [c for c, oc, _ in course if oc == "star"]
if cs:
    print("course step ending in the star: n %d, M frames mean %.1f median %.1f min %.1f max %.1f" % (len(cs), np.mean(cs), np.median(cs), min(cs), max(cs)))
# per-step success within a step budget b (default-budget searches only: a course tree there ends at the star or
# at its 150 M budget, so the cost of a star-ending step is uncensored)
dflt = {t for t in steps if not re.search(r"-C\d+$", t)}
c1 = [(st_[0][4] - st_[0][2]) / 1e6 for st_ in steps.values() if st_ and st_[0][0] == "castle" and st_[0][5] == "reached"]  # first castle step (hub budget is the same in every arm)
c2 = []
for t in dflt:
    cs_ = [(s1 - s0) / 1e6 for (kind, z, s0, t0, s1, oc) in steps[t] if kind == "course"]
    ocs = [oc for (kind, z, s0, t0, s1, oc) in steps[t] if kind == "course"]
    if cs_:
        c2.append(cs_[0] if ocs[0] == "star" else float("inf"))
if c1 and c2:
    print("per-step P(success within b), default-budget searches (castle n %d, first course step n %d):" % (len(c1), len(c2)))
    for b in [4, 6, 8, 10, 12, 15, 18, 20, 25, 30]:
        p1 = np.mean(np.array(c1) <= b); p2 = np.mean(np.array(c2) <= b)
        print("  b %2d M: castle %.2f  course %.2f  both (castle b, course b) %.2f" % (b, p1, p2, p1 * p2))
# budget arms: paired with the default search of the same seed
print("\n## course budget arms (paired with the default search of the same target and seed)")
byk = {}
for r in new:
    if r["threads"] == 4:
        byk.setdefault((r["target"], r["sseed"]), {})[r["cb"] or 0] = r
arms = {20: [], 12: []}
for key, d in sorted(byk.items()):
    if 0 in d:
        for cb in (20, 12):
            if cb in d:
                a0, a1 = d[0], d[cb]
                arms[cb].append((a0["sim_frames"] / 1e6, a1["sim_frames"] / 1e6 if a1["status"] == "found" else None,
                                 a0["frames"], a1["frames"], a1["status"]))
for cb, rows in arms.items():
    if rows:
        ok = [r for r in rows if r[4] == "found"]
        print(f"course budget {cb} M: n {len(rows)}, found {len(ok)}; cost default/arm mean %.1f / %.1f M; run default/arm mean %.0f / %.0f" % (
            np.mean([r[0] for r in ok]) if ok else float("nan"), np.mean([r[1] for r in ok]) if ok else float("nan"),
            np.mean([r[2] for r in ok]) if ok else float("nan"), np.mean([r[3] for r in ok]) if ok else float("nan")))
        for r in rows:
            print("   ", r)
arm_all = {}
for r in new:
    if r["threads"] == 4 and r.get("status") == "found":
        arm_all.setdefault(r["cb"] or 0, []).append((r["sim_frames"] / 1e6, r["frames"]))
for cb, v in sorted(arm_all.items()):
    v = np.array(v)
    print(f"all T4 searches at course budget {cb or 'default'}: n {len(v)} cost mean %.1f median %.1f, run mean %.0f median %.0f" % (v[:, 0].mean(), np.median(v[:, 0]), v[:, 1].mean(), np.median(v[:, 1])))

model = dict(law=dict(T0=float(loc), muT=float(math.log(sc)), sT=float(sh), f0=f0, muF=float(x.mean()), sF=float(x.std(ddof=1)), rho=rho),
             eff=EFF, n=len(T), pool=[list(map(float, p[1:])) + [p[0]] for p in pool],
             castle=cast, course=[list(c) for c in course],
             arms={str(k): v for k, v in arm_all.items()})
json.dump(model, open(OUT, "w"), indent=1, default=lambda o: o.item() if hasattr(o, "item") else str(o))
print("\nwrote", OUT)
