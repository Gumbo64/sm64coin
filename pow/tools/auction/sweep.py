#!/usr/bin/env python3
"""Sweeps the limit's slope (seconds per difficulty halving) under the
auction rules, against today's rules, for honest play, stamping ahead,
the hold policy and a withholding miner. Writes results.json and prints a
markdown table. `--measured CSV` uses envbench envelopes for the bots."""
from __future__ import annotations

import argparse
import dataclasses
import json
from concurrent.futures import ProcessPoolExecutor

import sim

SCENARIOS = {
    # name: (World overrides, withhold seconds for the optimizer)
    "honest, FTL 15 s": (dict(), 0.0, 15.0),
    "hold policy": (dict(hold=True), 0.0, 15.0),
    "quick bot stamps +120 s": (dict(), 0.0, 120.0, "ahead"),
    "quick bot stamps +120 s, hold": (dict(hold=True), 0.0, 120.0, "ahead"),
    "withhold 120 s": (dict(), 120.0, 15.0),
    "withhold 120 s + stale 20 s": (dict(stale_limit=20.0), 120.0, 15.0),
}


def miners_for(measured: str | None, withhold: float, ahead: bool = False):
    ms = [dataclasses.replace(m) for m in sim.DEFAULT_MINERS]
    if measured:
        ms = [sim.load_measured(measured, "power_on", "measured bot"), sim.load_measured(measured, "power_on", "measured bot x4", speed=4.0), ms[2]]
    ms[1].withhold = withhold
    ms[0].stamp_ahead = ahead
    return ms


def run(job):
    rules_name, halving, scen, blocks, seed, measured = job
    world_kw, withhold, ftl, *flag = SCENARIOS[scen]
    r = sim.current_rules(ftl=ftl) if rules_name == "current" else sim.auction_rules(halving, ftl)
    w = sim.World(blocks=blocks, seed=seed, **world_kw)
    res = sim.simulate(r, miners_for(measured, withhold, bool(flag)), w)
    return (rules_name, halving, scen, seed), res.summary(w.warmup)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--halvings", default="15,30,60,120,300,1200")
    ap.add_argument("--blocks", type=int, default=1500)
    ap.add_argument("--seeds", type=int, default=3)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--measured")
    ap.add_argument("--out", default="results.json")
    a = ap.parse_args()
    hs = [float(x) for x in a.halvings.split(",")]
    jobs = [("current", 300.0, s, a.blocks, k, a.measured) for s in SCENARIOS for k in range(a.seeds)]
    jobs += [("auction", h, s, a.blocks, k, a.measured) for h in hs for s in SCENARIOS for k in range(a.seeds)]
    with ProcessPoolExecutor(a.workers) as ex:
        out = list(ex.map(run, jobs))
    agg: dict = {}
    for (rn, h, scen, _), summ in out:
        agg.setdefault((rn, h, scen), []).append(summ)
    rows = []
    for (rn, h, scen), ss in agg.items():
        mean = lambda k: sum(s[k] for s in ss) / len(ss)
        names = sorted({n for s in ss for n in s["share"]})
        share = {n: round(sum(s["share"].get(n, 0) for s in ss) / len(ss), 3) for n in names}
        rows.append(dict(rules=rn, halving=h, scenario=scen, mean_s=round(mean("mean_s"), 1), p50_s=round(mean("p50_s"), 1),
                         p90_s=round(mean("p90_s"), 1), p99_s=round(mean("p99_s"), 1), cv=round(mean("cv"), 3),
                         restarts=round(mean("restarts"), 1), stalls=round(mean("stalls"), 1), share=share))
    json.dump(rows, open(a.out, "w"), indent=1)
    print("| rules | halving s | scenario | mean s | p50 | p90 | p99 | CV | restarts | stalls | shares |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        sh = ", ".join(f"{k} {v:.0%}" for k, v in r["share"].items())
        h = "grace 600" if r["rules"] == "current" else int(r["halving"])
        print(f"| {r['rules']} | {h} | {r['scenario']} | {r['mean_s']} | {r['p50_s']} | {r['p90_s']} | {r['p99_s']} | {r['cv']} | {r['restarts']} | {r['stalls']} | {sh} |")


if __name__ == "__main__":
    main()
