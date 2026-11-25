#!/usr/bin/env python3
"""Runs the head-start / selfish / fix-variant experiments.
  python3 run_experiments.py [group ...]    groups: grid dev kappa selfish A F C Cselfish E ctrl
Writes results_<group>.json (list of {key, runs:[stats...]}). 4 worker processes. Fixed seeds."""
import json, sys, time
from multiprocessing import Pool
import numpy as np
from hs import *

SCEN = {
    "40/30/20/10": [40, 30, 20, 10],
    "60/20/20": [60, 20, 20],
    "25x4": [25] * 4,
    "10x10": [10] * 10,
    "40+30x2": [40] + [2] * 30,
}
SEEDS = 6
NB = 3000
CS = [0.05, 0.3, 1, 3, 10, 30, 100, 300]
HS = [0, 60, 120, 300]
S4 = "40/30/20/10"


def build(spec):
    kw = dict(spec)
    r = kw.pop("rule", None)
    if r == "publish":
        kw["rule"] = AtPublish()
    elif isinstance(r, (int, float)):
        kw["rule"] = AtPublishDelay(r)
    u = kw.pop("uncle", None)
    if u is not None:
        kw["reward"] = UncleReward(u)
        kw["publish_losers"] = True
    kw["model"] = Model(**kw.pop("model", {})) if "model" in kw else None
    return kw


def one(job):
    key, shares, C, spec, seed, nb = job
    s = Sim(shares, C, seed=seed, **build(spec)).run(nb)
    r = s.stats()
    return key, {k: (v.tolist() if hasattr(v, "tolist") else v) for k, v in r.items()}


def add(jobs, key, scen, C, spec, seed0, nb=NB):
    for i in range(SEEDS):
        jobs.append((key, SCEN[scen] if isinstance(scen, str) else scen, C, spec, seed0 + i, nb))


def make(group):
    jobs = []
    if group == "grid":
        for sn in SCEN:
            for C in CS:
                for H in HS:
                    add(jobs, [sn, C, H, "auction"], sn, C, dict(H=H), 1000)
                add(jobs, [sn, C, 120, "at-publish"], sn, C, dict(H=120, rule="publish"), 1000)
    elif group == "dev":  # miner i alone uses H, others 0
        for sn in [S4, "40+30x2"]:
            n = len(SCEN[sn])
            for C in [3, 30, 300]:
                for who in [0, n - 1]:
                    for H in [0, 60, 120, 300, 600]:
                        Hv = [0.0] * n; Hv[who] = H
                        add(jobs, [sn, C, who, H], sn, C, dict(H=Hv), 2000)
    elif group == "kappa":
        for C in CS:
            for H in HS:
                add(jobs, [S4, C, H], S4, C, dict(H=H, model=dict(kappa=0.3)), 3000)
    elif group == "selfish":
        for C in [10, 100]:
            for a in [0.1, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45]:
                sh = [a] + [(1 - a) / 5] * 5
                add(jobs, ["honest", C, a, 0, 0], sh, C, dict(H=0), 4000)
                add(jobs, ["honest", C, a, 0, 120], sh, C, dict(H=120), 4000)
                for g in [0.0, 0.5, 1.0]:
                    for Hh in [0, 120]:
                        add(jobs, ["selfish", C, a, g, Hh], sh, C, dict(H=Hh, selfish=0, gamma=g), 4000)
                add(jobs, ["selfish-at-publish", C, a, 0.5, 120], sh, C, dict(H=120, rule="publish", selfish=0, gamma=0.5), 4000)
    elif group == "A":  # slope dial at the real Monero retarget; sh=0 so the d>=1 floor never binds
        for sn in [S4, "60/20/20", "40+30x2"]:
            for C in [3, 30, 300]:
                for H in ([0, 120, 300] if sn == S4 else [120]):
                    for dh in [30, 60, 120, 300, 1000, 3000, "flat"]:
                        sp = dict(H=H, start_halv=0)
                        if dh == "flat":
                            sp["valid"] = "flat"
                        else:
                            sp["dh"] = dh
                        add(jobs, [sn, C, H, dh], sn, C, sp, 6000)
        for C in [0.3, 3, 30, 300]:  # today's constants (sh=20) for reference
            add(jobs, [S4, C, 120, "today"], S4, C, dict(H=120), 6000)
    elif group == "F":  # slower chain
        for C in [3, 30, 300]:
            for H in [0, 120, 300]:
                for B in [300, 600, 1200]:
                    add(jobs, [S4, C, H, B], S4, C, dict(H=H, target=B), 7000)
    elif group == "C":  # valid-on-find + displacement window
        for sn in [S4, "40+30x2"]:
            for C in [3, 30, 300]:
                add(jobs, [sn, C, "first", 0], sn, C, dict(H=0, valid="flat"), 8000)
                for om in [15, 30, 60, 120]:
                    add(jobs, [sn, C, "shortest", om], sn, C, dict(H=0, valid="flat", omega=om, tiebreak="shortest"), 8000)
    elif group == "Cselfish":
        for C in [30]:
            for a in [0.1, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45]:
                sh = [a] + [(1 - a) / 5] * 5
                add(jobs, ["honest", C, a], sh, C, dict(H=0, valid="flat"), 9000)
                for g in [0.0, 0.5, 1.0]:
                    add(jobs, ["C-first", C, a, g, 0], sh, C, dict(H=0, valid="flat", selfish=0, gamma=g), 9000)
                for om in [30, 60]:
                    add(jobs, ["C-shortest", C, a, 0.0, om], sh, C, dict(H=0, valid="flat", selfish=0, gamma=0.0, omega=om, tiebreak="shortest"), 9000)
    elif group == "E":
        for sn in [S4, "40+30x2"]:
            for C in [3, 30, 300]:
                for u in [0.0, 0.5, 0.875]:
                    add(jobs, [sn, C, "S0", u], sn, C, dict(H=120, uncle=u), 10000)
                    add(jobs, [sn, C, "C-30", u], sn, C, dict(H=0, valid="flat", omega=30, tiebreak="shortest", uncle=u), 10000)
    elif group == "fresh":  # back-dating vs the freshness bound
        for sn in [S4, "40+30x2"]:
            for var, base in [("S0", dict(H=120)), ("A300", dict(H=120, start_halv=0, dh=300))]:
                for C in [0.3, 3, 30]:
                    for bd, who in [("none", []), ("top", [0]), ("all", True)]:
                        add(jobs, [sn, var, C, bd, "nobound", "-"], sn, C, dict(base, backdate=who, delay_jitter=3.0), 12000)
                        for D in [30, 60, 120]:
                            for mode in (["full", "aware"] if bd != "none" else ["full"]):
                                add(jobs, [sn, var, C, bd, D, mode], sn, C, dict(base, backdate=who, delay_jitter=3.0, fresh=D, bd_mode=mode), 12000)
    elif group == "ctrl":
        for sn in [S4, "40+30x2"]:
            for C in [3, 30, 300]:
                for H in [0, 120, 300]:
                    add(jobs, [sn, C, H, "S0"], sn, C, dict(H=H), 11000)
                    add(jobs, [sn, C, H, "S0-early"], sn, C, dict(H=H, announce_early=True), 11000)
                    add(jobs, [sn, C, H, "S0-backdate"], sn, C, dict(H=H, backdate=True), 11000)
                    add(jobs, [sn, C, H, "A300-backdate"], sn, C, dict(H=H, start_halv=0, dh=300, backdate=True), 11000)
                    add(jobs, [sn, C, H, "A300"], sn, C, dict(H=H, start_halv=0, dh=300), 11000)
    return jobs


if __name__ == "__main__":
    groups = sys.argv[1:] or ["grid", "dev", "kappa", "selfish", "A", "F", "C", "Cselfish", "E", "ctrl"]
    for g in groups:
        jobs = make(g)
        print(g, len(jobs), "runs", flush=True)
        t0 = time.time()
        out = {}
        with Pool(4) as p:
            for i, (k, r) in enumerate(p.imap_unordered(one, jobs, chunksize=2)):
                out.setdefault(json.dumps(k), []).append(r)
                if i % 100 == 0:
                    print(g, i, round(time.time() - t0), flush=True)
        json.dump(out, open(f"results_{g}.json", "w"))
        print(g, "done", round(time.time() - t0), flush=True)
