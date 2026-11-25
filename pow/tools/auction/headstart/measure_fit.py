#!/usr/bin/env python3
"""Writes measure_model.json (the run-length model for slowdecay.py) from measure_driver's results.jsonl.
emp  = pooled single-search run lengths (S and G arms, all targets; G arm = fresh pokes per search)
tau  = mean CPU-seconds of one search (one core machine does one 'dot' per tau)
kappa= how much a guided deep search beats independent min-of-n (0 if it does not)."""
import json, sys, collections
import numpy as np
src, out = sys.argv[1], sys.argv[2]
rows = [json.loads(l) for l in open(src)]
rows = [r for r in rows if r.get("status") == "found"]
F = [r["frames"] for r in rows if r["arm"] in "SG"]
jobs = collections.defaultdict(list)
for r in rows:
    if r["arm"] in "SG":
        jobs[r["tag"]].append(r)
cpu = [v[0]["job_cpu"] / len(v) for v in jobs.values() if v[0].get("job_cpu")]
sim = [r["sim_frames"] for r in rows if r["arm"] in "SG"]
j = dict(tau=float(np.mean(cpu)), emp=F, sim_frames_per_search=float(np.mean(sim)), kappa=float(sys.argv[3]) if len(sys.argv) > 3 else 0.0, tau0=300.0)
j['fits'] = {}
for f0 in (0, 1000, 1500, 2000):
    y = np.log(np.array(F, float) - f0)
    j['fits'][str(f0)] = dict(mu=float(y.mean()), sigma=float(y.std(ddof=1)))
json.dump(j, open(out, "w"))
print(j["fits"]); print(f"{len(F)} searches, F mean {np.mean(F):.0f} med {np.median(F):.0f}, tau {j['tau']:.0f} CPU-s, {j['sim_frames_per_search']/1e6:.1f} M frames/search")
