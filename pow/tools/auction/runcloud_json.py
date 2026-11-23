#!/usr/bin/env python3
"""Collects runcloud CSVs (pow/target/runcloud/*.csv, from
pow/miner/src/bin/runcloud.rs) into one JSON file for the run-cloud page:
every attempt of every strategy, found or given up, with its wall time.

  python3 runcloud_json.py ../../target/runcloud OUT.json
"""
import csv
import glob
import json
import os
import sys

src = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "../../target/runcloud")
out = sys.argv[2] if len(sys.argv) > 2 else "runcloud.json"
rows = []
for path in sorted(glob.glob(os.path.join(src, "*.csv"))):
    with open(path) as fh:
        for r in csv.DictReader(fh):
            rows.append({
                "strategy": r["strategy"],
                "attempt": int(r["attempt"]),
                "t": float(r["seconds"]),
                "frames": int(r["frames"]) if r["frames"] else None,
                "attempt_s": float(r["attempt_seconds"]),
                "sim_frames": int(r["sim_frames"]) if r["sim_frames"] else None,
                "result": r["result"],
            })
meta = {}
for path in sorted(glob.glob(os.path.join(src, "*.log"))):
    name = os.path.basename(path)[:-4]
    pid = os.path.join(src, name + ".pid")
    running = False
    if os.path.exists(pid):
        try:
            os.kill(int(open(pid).read().strip()), 0)
            running = True
        except (OSError, ValueError):
            pass
    meta[name] = {"running": running}
json.dump({"rows": rows, "strategies": meta}, open(out, "w"), indent=0)
print(f"{len(rows)} attempts from {len(meta)} strategies -> {out}")
