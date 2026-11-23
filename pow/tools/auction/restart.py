#!/usr/bin/env python3
"""The power-on restart as an escape from intelligence softlocks: miners can
only do the first K points of a cycle (the frontier); at K the chain is stuck
until a restart. Compares today's 3600 s gate with restart costs (frames a
power-on run counts extra, base + per point lost), under today's rules and
the auction rules. Prints a markdown table; writes restart_results.json."""
from __future__ import annotations

import dataclasses
import json
import sys
from concurrent.futures import ProcessPoolExecutor

import sim

FRONTIERS = [2, 10, 30, None]
DESIGNS = {
    "gate 3600 s": dict(restart_per_point=None, stall=3600.0),
    "delay 1200 s + 30 s/pt": dict(restart_base=1200.0, restart_per_point=30.0, stall=float("inf")),
    "delay 3600 s + 60 s/pt": dict(restart_base=3600.0, restart_per_point=60.0, stall=float("inf")),
    "delay 7200 s flat": dict(restart_base=7200.0, restart_per_point=0.0, stall=float("inf"), restart_max=7200.0),
}
WORLDS = {"fast bots": "DEFAULT_MINERS", "slow bots (today)": "SLOW_MINERS"}


def run(job):
    rules_name, design, frontier, seed, world = job
    if rules_name == "current":
        r = sim.current_rules(**DESIGNS[design])
    else:
        r = sim.auction_rules(float(rules_name.split()[1]), 15.0, **DESIGNS[design])
    w = sim.World(blocks=900, seed=seed, frontier=frontier, tmax=14400.0)
    res = sim.simulate(r, [dataclasses.replace(m) for m in getattr(sim, WORLDS[world])], w)
    return (world, rules_name, design, frontier), res.summary(w.warmup)


def main():
    rules = ["current"] + [f"auction {h}" for h in (sys.argv[1:] or ["60"])]
    jobs = [(rn, d, f, k, w) for w in WORLDS for rn in rules for d in DESIGNS for f in FRONTIERS for k in range(3)]
    if "current" in rules:
        jobs = [j for j in jobs if not (j[0] == "current" and j[1] != "gate 3600 s")]
    with ProcessPoolExecutor(4) as ex:
        out = list(ex.map(run, jobs))
    agg = {}
    for key, s in out:
        agg.setdefault(key, []).append(s)
    rows = []
    for (wn, rn, d, f), ss in agg.items():
        m = lambda k: round(sum((s[k] or 0) for s in ss) / len(ss), 2)
        rows.append(dict(world=wn, rules=rn, design=d, frontier=f, mean_s=m("mean_s"), p99_s=m("p99_s"), restarts=m("restarts"),
                         needless=m("needless_restarts"), stuck_wait_s=m("stuck_wait_s"), stuck_time=m("stuck_share_of_time"),
                         cycle_peak=m("mean_cycle_peak")))
    json.dump(rows, open("restart_results.json", "w"), indent=1)
    print("| world | rules | restart rule | frontier K | mean s | p99 s | restarts | needless | wait when stuck s | share of time stuck | points per cycle |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['world']} | {r['rules']} | {r['design']} | {r['frontier'] or 'none'} | {r['mean_s']} | {r['p99_s']} | {r['restarts']} | {r['needless']} | {r['stuck_wait_s']} | {r['stuck_time']:.0%} | {r['cycle_peak']} |")


if __name__ == "__main__":
    main()
