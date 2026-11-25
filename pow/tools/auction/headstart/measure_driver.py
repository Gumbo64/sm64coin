#!/usr/bin/env python3
"""Phase 1 driver: runs the `runlen` bin (miner/src/bin/runlen.rs) as a queue of single-thread jobs.
Arms (per target = a parent end state):
  S  same template (fixed pokes), independent searches, search seed varies          -> min-of-n by resampling
  G  seed grinding: every search on a different template (different poke seed)     -> min-of-n by resampling
  D  deep: one chain of K guided searches (best-so-far run's objective keys weight W)  -> best vs cum. compute
usage: measure_driver.py WORKDIR [--workers 6] [--maxload 24]
Resumable (skips finished job tags found in WORKDIR/results.jsonl). Waits while load - own > maxload."""
import os, sys, time, json, subprocess, argparse, threading
ap = argparse.ArgumentParser()
ap.add_argument("work"); ap.add_argument("--workers", type=int, default=6); ap.add_argument("--maxload", type=float, default=24)
ap.add_argument("--nS", type=int, default=24); ap.add_argument("--nG", type=int, default=24)
ap.add_argument("--nD", type=int, default=3); ap.add_argument("--K", type=int, default=8)
ap.add_argument("--guide", type=float, default=20.0)
ap.add_argument("--rom", default="/workspaces/sm64-combined/sm64-crypto3/baserom.us.z64")
ap.add_argument("--pidfile", default=None)
a = ap.parse_args()
W = os.path.abspath(a.work); BIN = W + "/../target/release/runlen"
OUT = W + "/results.jsonl"
done = set()
if os.path.exists(OUT):
    for l in open(OUT):
        try: done.add(json.loads(l)["tag"])
        except Exception: pass
targets = ["p29", "p30", "p34"]
jobs = []
for t in targets:
    for r in range(a.nD):
        jobs.append((f"{t}-D{r}", t, dict(seed_n=1, sseed0=1000 * (r + 1), searches=a.K, guide=a.guide)))
    for i in range(a.nS):
        jobs.append((f"{t}-S{i}", t, dict(seed_n=1, sseed0=i, searches=1, guide=0)))
    for i in range(a.nG):
        jobs.append((f"{t}-G{i}", t, dict(seed_n=100 + i, sseed0=0, searches=1, guide=0)))
# interleave so partial results cover every arm/target
jobs.sort(key=lambda j: (0 if '-D' in j[0] else 1, int(''.join(c for c in j[0].split('-')[1] if c.isdigit()) or 0), j[0]))  # long chains first, then S/G interleaved
jobs = [j for j in jobs if j[0] not in done]
lock = threading.Lock(); running = [0]
def work():
    while True:
        with lock:
            if not jobs: return
            tag, t, p = jobs.pop(0)
        while True:
            with lock:
                if os.getloadavg()[0] - running[0] <= a.maxload:
                    running[0] += 1; break
            time.sleep(20)
        scr = f"{W}/scr/{tag}"; os.makedirs(scr, exist_ok=True)
        cmd = [BIN, "--rom", a.rom, "--parent", "x", "--state-file", f"{W}/{t}.st", "--seed-n", str(p["seed_n"]),
               "--searches", str(p["searches"]), "--sseed0", str(p["sseed0"]), "--threads", "1", "--scratch", scr,
               "--out", f"{W}/raw/{tag}.jsonl", "--tag", tag, "--guide-weight", str(p["guide"])]
        os.makedirs(W + "/raw", exist_ok=True)
        cpu = None
        try:
            pr = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=open(f"{scr}.err", "w"))
            _, _, ru = os.wait4(pr.pid, 0)  # CPU seconds of the runlen process (its rrtrun child is reaped by it: add via children below)
            cpu = ru.ru_utime + ru.ru_stime
            pr.returncode = 0
        except Exception as e:
            print("fail", tag, e, flush=True)
        with lock:
            running[0] -= 1
            rows = [json.loads(l) for l in open(f"{W}/raw/{tag}.jsonl")] if os.path.exists(f"{W}/raw/{tag}.jsonl") else []
            with open(OUT, "a") as f:
                for r in rows: f.write(json.dumps({**r, "tag": tag, "arm": tag.split("-")[1][0], "target": t, "job_cpu": cpu}) + "\n")
                f.write(json.dumps({"tag": tag, "done": True}) + "\n") if not rows else None
        subprocess.run(["rm", "-rf", scr])
        print("done", tag, [r["frames"] for r in rows], flush=True)
ths = [threading.Thread(target=work) for _ in range(a.workers)]
for t in ths: t.start()
for t in ths: t.join()
print("ALL DONE", flush=True)
