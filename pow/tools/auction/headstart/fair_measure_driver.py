#!/usr/bin/env python3
"""Fairness measurement: native thread scaling of ONE search (runlen bin, threads 1/2/4 on one tree).
usage: fair_measure_driver.py WORKDIR STATEDIR --cap 10 --maxload 30 --hours 2.6
Jobs cycle (threads 4, 2, 1) x targets x search seeds 20.. ; a scheduler keeps the sum of running
threads <= --cap and waits while the 1-min load (minus own threads) > --maxload. Records wall seconds
(runlen's own clock), CPU seconds (wait4 of the process tree) and the load average at start/end."""
import os, sys, time, json, subprocess, argparse, threading
ap = argparse.ArgumentParser()
ap.add_argument("work"); ap.add_argument("states")
ap.add_argument("--cap", type=int, default=10); ap.add_argument("--maxload", type=float, default=30)
ap.add_argument("--hours", type=float, default=2.6)
ap.add_argument("--plan", default="threads", help="threads: T4/T2/T1 per seed; budget: T4 at course budgets default / 20 M / 12 M per seed")
ap.add_argument("--rom", default="/workspaces/sm64-combined/sm64-crypto3/baserom.us.z64")
ap.add_argument("--bin", required=True)
ap.add_argument("--out", default="results.jsonl")
ap.add_argument("--steplog", action="store_true", help="also keep each search's step log (runlen --steplog) in WORKDIR/steps/")
a = ap.parse_args()
W = os.path.abspath(a.work); OUT = W + "/" + a.out; os.makedirs(W + "/raw", exist_ok=True)
done = set()
if os.path.exists(OUT):
    for l in open(OUT):
        try: done.add(json.loads(l)["tag"])
        except Exception: pass
jobs = []
for i in range(12):
    for t in ["p34", "p29", "p30"]:
        if a.plan == "threads":
            for th in [4, 2, 1]:
                jobs.append((f"{t}-T{th}-{20 + i}", t, th, 20 + i, None))
        else:
            for cb in [None, 20, 12]:
                jobs.append((f"{t}-T4-{20 + i}" + (f"-C{cb}" if cb else ""), t, 4, 20 + i, cb))
for x in ["results.jsonl", a.out]:  # finished elsewhere, or still running from an earlier driver
    if os.path.exists(W + "/" + x):
        for l in open(W + "/" + x):
            try: done.add(json.loads(l)["tag"])
            except Exception: pass
if os.path.isdir(W + "/pids"):
    done |= {open(W + "/pids/" + p).read().strip() for p in os.listdir(W + "/pids")}
jobs = [j for j in jobs if j[0] not in done]
t_end = time.time() + a.hours * 3600
lock = threading.Lock(); used = [0]
def run(tag, t, th, ss, cb=None):
    scr = f"{W}/scr/{tag}"; os.makedirs(scr, exist_ok=True)
    cmd = [a.bin, "--rom", a.rom, "--parent", "x", "--state-file", f"{a.states}/{t}.st", "--seed-n", "1",
           "--searches", "1", "--sseed0", str(ss), "--threads", str(th), "--scratch", scr,
           "--out", f"{W}/raw/{tag}.jsonl", "--tag", tag]
    if cb:
        cmd += ["--course-frames", str(int(cb * 1e6))]
    if a.steplog:
        os.makedirs(W + "/steps", exist_ok=True); cmd += ["--steplog", f"{W}/steps/{tag}.log"]
    l0 = os.getloadavg()[0]; t0 = time.time()
    pr = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=open(f"{scr}.err", "w"))
    open(f"{W}/pids/{pr.pid}", "w").write(tag)
    _, _, ru = os.wait4(pr.pid, 0)
    os.remove(f"{W}/pids/{pr.pid}")
    cpu = ru.ru_utime + ru.ru_stime
    rows = [json.loads(l) for l in open(f"{W}/raw/{tag}.jsonl")] if os.path.exists(f"{W}/raw/{tag}.jsonl") else []
    with lock:
        used[0] -= th
        with open(OUT, "a") as f:
            for r in rows:
                f.write(json.dumps({**r, "tag": tag, "target": t, "threads": th, "job_cpu": cpu, "course_budget": cb, "load0": l0,
                                    "load1": os.getloadavg()[0], "wall": time.time() - t0}) + "\n")
    subprocess.run(["rm", "-rf", scr])
    print("done", tag, [r["frames"] for r in rows], round(time.time() - t0), flush=True)
os.makedirs(W + "/pids", exist_ok=True)
ths = []
while jobs and time.time() < t_end:
    tag, t, th, ss, cb = jobs[0]
    with lock:
        ok = used[0] + th <= a.cap and os.getloadavg()[0] - used[0] <= a.maxload
        if ok: used[0] += th; jobs.pop(0)
    if ok:
        x = threading.Thread(target=run, args=(tag, t, th, ss, cb)); x.start(); ths.append(x)
    else:
        time.sleep(15)
for x in ths: x.join()
print("ALL DONE", flush=True)
