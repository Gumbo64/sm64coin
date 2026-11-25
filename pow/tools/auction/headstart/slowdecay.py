#!/usr/bin/env python3
"""Phase 2: what a slower-decaying time limit does to SM64Coin, with the run-length model measured in phase 1.
Builds on hs.py (event-driven multi-miner sim with the Monero retarget).

Shapes (rules.rs STAR_CYCLE: decay_halving dh seconds per difficulty halving, start_halvings sh):
  today  dh 60,  sh 20          x1..x50: dh = 60*k, sh 0 (the d>=1 floor never binds)
Analyses (each a function; `python3 slowdecay.py all` runs them with a process pool):
  solo     (a) a 10/20/33% miner's solo block time vs the network's, at the network's retargeted difficulty
  attack   (b) private-chain double-spend race under block-count and run-quality-weighted fork choice
  excess   (c) hold time W and the #1 miner's share excess (40/30/20/10)
  selfish  (d) Eyal-Sirer threshold
  retarget (e) block-time spread and the response to a x4 / x1/4 compute step
  human    manual-miner impact: winning run length vs a human's
Output: slowdecay_results.json (+ pngs by slowdecay_plots.py).
"""
from __future__ import annotations
import copy, json, math, os, sys, time
from multiprocessing import Pool
import numpy as np
from hs import *

FIT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "measure_model.json")

SHAPES = {
    "today(dh60,sh20)": dict(dh=60.0, start_halv=20),
    "x1(dh60,sh0)": dict(dh=60.0, start_halv=0),
    "x2(dh120)": dict(dh=120.0, start_halv=0),
    "x4(dh240)": dict(dh=240.0, start_halv=0),
    "x8(dh480)": dict(dh=480.0, start_halv=0),
    "x16(dh960)": dict(dh=960.0, start_halv=0),
    "x50(dh3000)": dict(dh=3000.0, start_halv=0),
}
CS = [10, 100, 1000]  # single-core native machines (one search per tau = measured CPU-seconds)


def load_model(name="measured"):
    """Model from measure_model.json (written by measure_fit.py), or the old assumed one."""
    if name == "assumed":
        return Model()
    j = json.load(open(FIT))
    if name.startswith("f0="):  # shifted lognormal fitted to the measured searches with this floor
        f0 = name[3:]
        ft = j["fits"][f0]
        return Model(tau=j["tau"], fmin=float(f0), med=math.exp(ft["mu"]), sigma=ft["sigma"], kappa=j.get("kappa", 0.0), tau0=j.get("tau0", 300.0))
    return Model(tau=j["tau"], fmin=0.0, emp=np.array(j["emp"], float), kappa=j.get("kappa", 0.0), tau0=j.get("tau0", 300.0))


# ---------------------------------------------------------------- quality-weighted fork choice
class ByQuality(ForkChoice):
    """Chain weight = sum over its blocks of (F0/F)^k (F the winning run's frames). k=0 is block counting."""
    def __init__(self, k, F0=3000.0):
        self.k, self.F0 = k, F0
        self.name = f"quality k={k:g}"
        self.memo = {0: 0.0}

    def key(self, sim, b):
        m = self.memo
        if b in m:
            return m[b]
        path = []
        x = b
        while x not in m:
            path.append(x); x = sim.par[x]
        v = m[x]
        for y in reversed(path):
            v += (self.F0 / max(sim.Fr[y], 1.0)) ** self.k
            m[y] = v
        return v


def make_sim(shares, C, shape, model, seed, **kw):
    sp = dict(SHAPES[shape]) if isinstance(shape, str) else dict(shape)
    sp.update(kw)
    return Sim(shares, C, model=model, seed=seed, **sp)


# ---------------------------------------------------------------- (a) solo
def solo_rates(shape, C, model, alphas=(1.0, 0.33, 0.2, 0.1), nb=500, seed=5):
    """Mean seconds for an alpha miner alone to produce a valid block at the network's d (idealised retarget:
    the log2 d at which the whole network's block time is 300 s), and the mean run length of those blocks."""
    kw = dict(SHAPES[shape])
    ld = calibrate_ld([1.0], C, H=120, nb=nb, model=model, **kw)
    out = {"ld": ld}
    for a in alphas:
        s = make_sim([a, 1 - a] if a < 1 else [1.0], C, shape, model, seed, fixed_ld=ld, H=120)
        s.dn[0] = 2.0 ** ld
        ts, Fs = [], []
        for i in range(400):
            s.rng = np.random.default_rng(seed * 100003 + i)
            c, v, F, fd, stamp = s.plan(0, 0, 0.0)
            ts.append(v); Fs.append(F)
        out[a] = dict(time=float(np.mean(ts)), med=float(np.median(ts)), F=float(np.mean(Fs)))
    return out


# ---------------------------------------------------------------- (b) private-chain race
class Chain:
    """One side of the race: a Sim used only for its plan() / new_block / retarget on a copied history."""
    def __init__(self, base, lam, seed):
        s = copy.copy(base)
        for nm in ("par", "hgt", "ts", "miner", "dn", "cum", "pub", "fdot", "Fr", "cmt", "dead", "vr"):
            setattr(s, nm, list(getattr(base, nm)))
        s.rng = np.random.default_rng(seed)
        s.lam = np.array([lam])
        s.n = 1
        s.H = np.array([base.H[0]])
        self.s = s
        self.tip = base.pubtip
        self.t = base.vr[self.tip]
        self.times, self.Fs = [], []

    def extend(self):
        s = self.s
        c, v, F, fd, stamp = s.plan(0, self.tip, self.t)
        b = s.new_block(self.tip, 0, max(v, s.vr[self.tip]), c, fd, F, stamp)
        self.tip = b
        self.t = c  # AtFind: the author builds on it at find time
        self.times.append(s.vr[b]); self.Fs.append(F)


ZS = (0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32)
KS = (0, 1, 2, 4)
KS2 = (0, 4, 16, 40, 'work')  # the extended weight family (attack2)


def race_paths(base, C, alpha, seed, nh_total):
    """One sample path of both sides from the base chain's tip: honest (1-alpha of C) and attacker (alpha)."""
    tau = base.model.tau
    H = Chain(base, (1 - alpha) * C / tau, seed * 2 + 1)
    A = Chain(base, alpha * C / tau, seed * 2 + 2)
    for _ in range(nh_total):
        H.extend()
    Th = np.array(H.times)
    for _ in range(4 * nh_total):
        A.extend()
        if A.times[-1] > Th[-1]:
            break
    return Th, np.array(H.Fs), np.array(A.times), np.array(A.Fs)


def weights(k, F, F0, model):
    if k == 'work':  # expected searches to find a run at least this short (1/CDF of the fitted single-search law), capped
        z = (np.log(np.maximum(F - model.fmin, 1.0)) - math.log(model.med)) / model.sigma
        G = 0.5 * np.array([math.erfc(-x / math.sqrt(2)) for x in z])
        return 1.0 / np.maximum(G, 1e-9)
    return (F0 / F) ** k if k else np.ones_like(F)


def race_outcomes(Th, FH, Ta, FA, F0, zs=ZS, ks=KS, model=None):
    """For each (k, z): does the attacker, who releases a j-block private chain at max(its j-th block's time, T_z),
    outweigh the honest chain of the blocks published by then, with at least z of them (the merchant's confirmations)?"""
    out = {}
    for k in ks:
        wh = np.cumsum(weights(k, FH, F0, model))
        wa = np.cumsum(weights(k, FA, F0, model))
        for z in zs:
            Tz = Th[z - 1] if z > 0 else 0.0
            tr = np.maximum(Ta, Tz)
            nh = np.searchsorted(Th, tr, side="right")
            valid = nh < len(Th)  # beyond the horizon: not counted
            ok = valid & (nh >= z) & (wa > np.where(nh > 0, wh[np.minimum(nh, len(wh)) - 1], 0.0))
            out[(k, z)] = bool(ok.any())
    return out


def attack_curve(shape, C, alpha, model, trials=1200, seed=3, F0=2400.0, H=120, hist=100, ks=KS):
    """P(attack succeeds) for each fork-choice weight exponent k and confirmation depth z."""
    bases = [make_sim([1.0], C, shape, model, 700 + i, H=H).run(hist) for i in range(3)]
    cnt = {(k, z): 0 for k in ks for z in ZS}
    for t in range(trials):
        Th, FH, Ta, FA = race_paths(bases[t % 3], C, alpha, seed * 10_000_019 + t, max(ZS) + 70)
        for key, v in race_outcomes(Th, FH, Ta, FA, F0, ks=ks, model=model).items():
            cnt[key] += v
    return {f"{k}|{z}": cnt[(k, z)] / trials for k in ks for z in ZS}


def depth_for(res, p=1e-3):
    """smallest z (interpolated on the log-linear tail) with P < p."""
    zs = sorted(res)
    pts = [(z, res[z]) for z in zs if res[z] > 0]
    if len(pts) < 2:
        return zs[0] if pts == [] else None
    # fit log P = a + b z on the tail points with P < 0.5
    tail = [(z, p_) for z, p_ in pts if p_ < 0.5] or pts[-3:]
    if len(tail) < 2:
        return None
    x = np.array([t[0] for t in tail], float); y = np.log([t[1] for t in tail])
    b, a = np.polyfit(x, y, 1)
    if b >= -1e-6:
        return float("inf")
    return float((math.log(p) - a) / b)


# ---------------------------------------------------------------- (c)(e) honest runs
def honest_stats(shape, C, model, seeds=(1, 2, 3), nb=2500, shares=(40, 30, 20, 10), H=120, **kw):
    runs = []
    for sd in seeds:
        s = make_sim(list(shares), C, shape, model, 4000 + sd, H=H, **kw).run(nb)
        st = s.stats(warm=150)
        ch = s.main_chain()[150:-10]
        iv = np.diff([s.ts[b] for b in ch])
        Fw = np.array([s.Fr[b] for b in ch])
        st = {k: (v.tolist() if hasattr(v, "tolist") else v) for k, v in st.items()}
        st.update(iv_cv=float(iv.std() / iv.mean()), iv_p95=float(np.quantile(iv, 0.95)), Fw_q=[float(x) for x in np.quantile(Fw, [0.1, 0.5, 0.9])],
                  ld_sd=float(np.std([math.log2(s.dn[b]) for b in ch])), Fw=Fw[::5].tolist())
        runs.append(st)
    return runs


def selfish_share(shape, C, model, alpha, gamma, seeds=(1, 2, 3), nb=3000, H=120):
    out = []
    sh = [alpha] + [(1 - alpha) / 5] * 5
    for sd in seeds:
        s = make_sim(sh, C, shape, model, 9000 + sd, H=H, selfish=0, gamma=gamma).run(nb)
        out.append(float(s.stats(warm=150)["share"][0]))
    return float(np.mean(out))


def step_response(shape, C, model, factor, seed=11, nb1=300, nb2=300):
    s = make_sim([40, 30, 20, 10], C, shape, model, seed, H=120).run(nb1)
    s.lam = s.lam * factor
    s.run(nb1 + nb2)
    ch = s.main_chain()
    ts = np.array([s.ts[b] for b in ch])
    iv = np.diff(ts)
    pre = float(iv[nb1 - 60: nb1].mean())
    post = [float(iv[nb1 + i: nb1 + i + 20].mean()) for i in range(0, nb2 - 25, 20)]
    return dict(pre=pre, post20=post)


# ---------------------------------------------------------------- driver
def job(a):
    kind, shape, C, args = a
    m = load_model(os.environ.get("SLOW_MODEL", "measured"))
    if "tau" in args:
        m.tau = args["tau"]
    if kind == "solo":
        return a[:3], solo_rates(shape, C, m)
    if kind == "attack":
        return (kind, shape, C, args["alpha"]), attack_curve(shape, C, args["alpha"], m, trials=args.get("trials", 1200))
    if kind == "attack2":
        return ("attack", shape, C, args["alpha"]), attack_curve(shape, C, args["alpha"], m, trials=args.get("trials", 1200), ks=KS2)
    if kind == "honest":
        return a[:3], honest_stats(shape, C, m)
    if kind == "selfish":
        return (kind, shape, C, args["alpha"], args["gamma"]), selfish_share(shape, C, m, args["alpha"], args["gamma"])
    if kind == "step":
        return (kind, shape, C, args["factor"]), step_response(shape, C, m, args["factor"])


def jobs(which):
    J = []
    for shape in ([x for x in SHAPES if x.split('(')[0] in os.environ['ONLYSHAPES'].split(',')] if os.environ.get('ONLYSHAPES') else SHAPES):
        for C in ([int(os.environ['ONLYC'])] if os.environ.get('ONLYC') else CS):
            if "solo" in which: J.append(("solo", shape, C, {}))
            if "honest" in which: J.append(("honest", shape, C, {}))
            if "attack" in which:
                for al in (0.1, 0.2, 0.33):
                    J.append(("attack", shape, C, dict(alpha=al)))
            if "attack2" in which:
                for al in (0.1, 0.2, 0.33):
                    J.append(("attack2", shape, C, dict(alpha=al)))
            if "selfish" in which:
                for al in (0.1, 0.2, 0.25, 0.3, 0.35, 0.4):
                    for g in (0.0, 0.5):
                        J.append(("selfish", shape, C, dict(alpha=al, gamma=g)))
            if "step" in which:
                for f in (4.0, 0.25):
                    J.append(("step", shape, C, dict(factor=f)))
    return J


if __name__ == "__main__":
    which = sys.argv[1].split(",") if len(sys.argv) > 1 and sys.argv[1] != "all" else ["solo", "honest", "attack", "selfish", "step"]
    out = sys.argv[2] if len(sys.argv) > 2 else "slowdecay_results.json"
    nproc = int(os.environ.get("NPROC", 6))
    J = jobs(which)
    res = {}
    t0 = time.time()
    with Pool(nproc) as p:
        for i, (k, v) in enumerate(p.imap_unordered(job, J)):
            res["|".join(map(str, k))] = v
            if i % 10 == 0:
                print(i, len(J), f"{time.time() - t0:.0f}s", k, flush=True)
    json.dump(res, open(out, "w"), indent=0, default=float)
