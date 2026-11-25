#!/usr/bin/env python3
"""Progress-aware mining-fairness simulator for SM64Coin (fairness-research, FAIRNESS.md).

The earlier simulators (hs.py, slowdecay.py) let a miner's finished searches arrive as a Poisson
stream. Real mining is not memoryless: one search (one rrt tree, `rrt::mine`) has to run for its
whole cost T (emulated frames) before it produces ANY run, and the work is lost when the block
changes (new parent, new pokes). Here every miner has `slots` concurrent searches; a slot runs
search after search (restarting after `budget` frames if one has not found yet), each search
draws a (cost T, run length F) pair from the measured joint law:

  T = T0 + exp(mu_T + s_T z1)  (M frames; 3-parameter lognormal fitted to the 36 measured searches)
  F = f0 + exp(mu_F + s_F z2)  (frames; SLOWDECAY's measure_model.json fit, f0 = 1500)
  corr(z1, z2) = rho            (Gaussian copula; measured within-target normal-score rho = 0.72:
                                 searches that finish early also find SHORT runs)

A slot of a miner with per-core speed `speed` and `k` threads on one tree does
`speed * FPS1 * k * eff(k)` frames/s. Validity is the real rule (rules.rs `max_frames`, continuous
level8): a run of F frames is valid `G + dh/8 * (level8(d) + 8*sh - ln(36000/F)/-ln 0.98)` s after
the parent's stamp (G = decay_grace; 0 if F already fits). Monero retarget (window 60, cut 6,
target 300 s, d >= 1) on the chain's own stamps.

Rules:
  hold  today: a node holds a run until it is valid; the earliest-valid run wins (first seen).
  pof   signed proofs + publish-on-find: runs are relayed at find, become eligible at validity;
        within omega s of the first eligible sibling a strictly shorter eligible sibling displaces
        it; after that the height is frozen (longer chains still win).
Miner policy per miner: H (commit horizon, s): a miner whose best held run is within H of valid
stops this height and starts the next on its own (unpublished) block (the head start of RESULTS.md);
H < 0: never commits (keeps improving until the height is decided, then starts at publication).
"""
from __future__ import annotations

import math
from dataclasses import dataclass, field

import numpy as np

LN098 = -math.log(0.98)
BASE = 36000.0
WINDOW, CUT, TARGET = 60, 6, 300.0
FPS1 = 38.8e3          # emulated frames per CPU-second per search thread (MEASURE.md + this branch's runs)
DELTA = 2.0            # propagation (s)

# joint law of one search (fitted in fair_fit.py, written to fair_model.json)
LAW = dict(T0=15.40, muT=math.log(14.35), sT=0.677, f0=1500.0, muF=7.3282, sF=0.3739, rho=0.72)

# native thread scaling of one search: effective speed-up per thread (filled from fair_measure)
EFF = {1: 1.0, 2: 0.95, 4: 0.85}


def eff_of(k):
    if k in EFF:
        return EFF[k]
    ks = sorted(EFF)
    if k > ks[-1]:  # Amdahl-ish extrapolation from the largest measured
        e = EFF[ks[-1]]; s = (1 / e - 1) / (ks[-1] - 1) if ks[-1] > 1 else 0
        return 1.0 / (1 + s * (k - 1))
    return float(np.interp(k, ks, [EFF[x] for x in ks]))


@dataclass
class Miner:
    name: str
    cores: int = 4
    speed: float = 1.0       # per-core speed relative to the measured core
    k: int = 4               # threads per search (browser: 1)
    budget: float = math.inf  # restart a search after this many M frames without a find
    H: float = 120.0         # commit horizon (s); < 0 never
    kind: str = "native"
    D: float = 0.0           # withhold a found block D s before publishing it (hold rule)
    fd: float = 0.0          # forward-date the stamp this many s (<= 120, the future limit)
    bd: bool = False         # back-date: stamp the block as early as allowed (its run's valid time, or the
                             # freshness bound: publication - fresh), not "now"

    @property
    def slots(self):
        return max(1, self.cores // self.k)

    @property
    def rate(self):  # frames/s of one slot
        return self.speed * FPS1 * self.k * eff_of(self.k)

    @property
    def compute(self):  # frames/s the miner COULD do with perfect scaling: cores x speed
        return self.cores * self.speed * FPS1


def browser(name, cores=4, speed=0.5, **kw):
    return Miner(name, cores=cores, speed=speed, k=1, kind="browser", **kw)


class Law:
    def __init__(self, rho=None, **kw):
        self.p = dict(LAW, **kw)
        if rho is not None:
            self.p["rho"] = rho

    def draw(self, rng, shape):
        p = self.p
        z1 = rng.standard_normal(shape)
        z2 = p["rho"] * z1 + math.sqrt(1 - p["rho"] ** 2) * rng.standard_normal(shape)
        if p.get("exp"):  # check only: memoryless searches (Poisson finds), mean exp M frames
            return rng.exponential(p["exp"] * 1e6, shape), p["f0"] + np.exp(p["muF"] + p["sF"] * z2)
        T = p["T0"] + np.exp(p["muT"] + p["sT"] * z1)
        F = p["f0"] + np.exp(p["muF"] + p["sF"] * z2)
        return T * 1e6, F


@dataclass
class Rules:
    rule: str = "hold"       # hold | pof
    G: float = 0.0           # decay_grace (s)
    dh: float = 60.0         # decay_halving
    sh: int = 20             # start_halvings
    omega: float = 60.0      # pof displacement window
    target: float = TARGET
    fresh: float = math.inf  # freshness bound: a tip stamped more than this before it arrives loses, so a
                             # back-dater stamps no earlier than publication - fresh
    flat: bool = False       # check only: every run valid on find (classic PoW race)
    clock: str = "stamp"     # the child's limit clock starts at the parent's "stamp" (rules.rs) or at the
                             # parent run's earliest "valid" time (stamp-independent alternative)

    def valid_after(self, F, d):
        if self.flat:
            return np.zeros_like(F)
        L = 8.0 * math.log2(max(d, 1.0)) + 8.0 * self.sh
        x = L - np.log(BASE / F) / LN098
        return np.where(x > 0, self.G + self.dh / 8.0 * x, 0.0)


def next_diff(ts, cm, target):
    n = len(ts)
    if n <= 1:
        return 1.0
    ts = ts[-WINDOW:]; cm = cm[-WINDOW:]; n = len(ts)
    if n <= WINDOW - 2 * CUT:
        lo, hi = 0, n
    else:
        lo = (n - (WINDOW - 2 * CUT) + 1) // 2
        hi = lo + WINDOW - 2 * CUT
    sts = sorted(ts)
    span = max(sts[hi - 1] - sts[lo], 1.0)
    return max(1.0, (cm[hi - 1] - cm[lo]) * target / span)


class Chain:
    """Stamps / cumulative difficulty of one chain (for its own retarget)."""

    def __init__(self, target=TARGET, ts=None, cm=None, d=1.0):
        self.ts = list(ts or [0.0]); self.cm = list(cm or [1.0]); self.d = d; self.target = target
        self.clock = self.ts[-1]  # where the child's limit clock starts (the stamp; or the run's valid time)

    def copy(self):
        c = Chain(self.target, self.ts[-WINDOW:], self.cm[-WINDOW:], self.d); c.clock = self.clock
        return c

    def add(self, stamp, clock=None):
        self.clock = stamp if clock is None else clock
        self.ts.append(stamp); self.cm.append(self.cm[-1] + self.d)
        if len(self.ts) > 4 * WINDOW:
            self.ts = self.ts[-WINDOW:]; self.cm = self.cm[-WINDOW:]
        self.d = next_diff(self.ts, self.cm, self.target)

    @property
    def stamp(self):
        return self.ts[-1]


class Field:
    """The searches of a set of miners on ONE parent: slots start at their miner's start time and
    run search after search; `extend(t)` makes sure every run found before t is listed."""

    def __init__(self, miners, starts, law, rng, slot_scale=None):
        self.law, self.rng = law, rng
        mid, rate, bud, st = [], [], [], []
        for i, (m, s) in enumerate(zip(miners, starts)):
            if s is None or not math.isfinite(s):
                continue
            n = m.slots if slot_scale is None else slot_scale[i]
            mid += [i] * n; rate += [m.rate] * n; bud += [m.budget * 1e6] * n; st += [s] * n
        self.mid = np.array(mid, int); self.rate = np.array(rate); self.bud = np.array(bud)
        self.clock = np.array(st, float)  # per slot: time its current search started
        self.f = np.empty(0); self.F = np.empty(0); self.m = np.empty(0, int)
        self.upto = -math.inf

    def extend(self, t):
        if t <= self.upto or len(self.mid) == 0:
            self.upto = max(self.upto, t); return
        while True:
            need = t - self.clock
            if np.all(need <= 0):
                break
            J = int(np.clip(np.max(need * self.rate / np.minimum(LAW["T0"] * 1e6, self.bud)), 1, 64)) + 1
            T, F = self.law.draw(self.rng, (len(self.mid), J))
            cost = np.minimum(T, self.bud[:, None])
            tt = self.clock[:, None] + np.cumsum(cost, axis=1) / self.rate[:, None]
            ok = (T <= self.bud[:, None]) & (tt - cost / self.rate[:, None] < t)  # search started before t
            # keep every completed search that started before t (their finds may lie after t: fine)
            self.f = np.concatenate([self.f, tt[ok]]); self.F = np.concatenate([self.F, F[ok]])
            self.m = np.concatenate([self.m, np.broadcast_to(self.mid[:, None], tt.shape)[ok]])
            started = tt - cost / self.rate[:, None] < t
            # new clock: end of the last search that started before t
            last = np.where(started.any(axis=1), np.max(np.where(started, tt, -np.inf), axis=1), self.clock)
            self.clock = np.maximum(self.clock, last)
            if np.all(self.clock >= t):
                break
        self.upto = t


def resolve(field, miners, rules, chain, until_hint=600.0):
    """Decide one height on `chain`'s tip. Returns dict(t=time the height is decided (block public),
    w=winner index, stamp, F, c=winner's commit time, cm=per-miner commit, stale=bool, W=head start)."""
    n = len(miners)
    Hs = np.array([m.H for m in miners], float)
    stamp0 = chain.clock
    t0 = max(stamp0, float(np.min(field.clock)) if len(field.clock) else stamp0)
    t = t0 + until_hint
    while True:
        field.extend(t)
        f, F, mi = field.f, field.F, field.m
        if len(f):
            v = stamp0 + rules.valid_after(F, chain.d)
            v = np.maximum(v, max(stamp0, chain.stamp) + 1.0)  # stamps increase
            pub = np.maximum(f, v)
            # commit time per miner
            cm = np.full(n, np.inf)
            Hm = Hs[mi]
            cc = np.where(Hm >= 0, np.maximum(f, v - np.maximum(Hm, 0)), np.inf)
            np.minimum.at(cm, mi, cc)
            usable = f <= cm[mi] + 1e-9
            if rules.rule == "hold":
                pm = np.full(n, np.inf)
                np.minimum.at(pm, mi[usable], pub[usable])
                Dm = np.array([m.D for m in miners])
                pp = pm + Dm  # publication (a withholder publishes D s after it could)
                w = int(np.argmin(pp)); tw = pp[w]
                if tw <= field.upto:
                    srt = np.sort(pp)
                    stale = n > 1 and srt[1] < tw + DELTA
                    j = np.where(usable & (mi == w) & (np.abs(pub - pm[w]) < 1e-9))[0][0]
                    c = min(cm[w], pm[w])
                    stamp = max(v[j], tw - rules.fresh) if miners[w].bd else tw + min(miners[w].fd, 120.0)
                    return dict(t=tw, w=w, stamp=stamp, F=F[j], c=c, cm=cm, stale=bool(stale), W=tw - c, decided=tw, gap=tw - stamp, v=v[j])
            else:  # pof
                e = np.maximum(f + DELTA, v)
                e = np.where(usable, e, np.inf)
                E1 = e.min()
                if math.isfinite(E1):
                    X = E1 + rules.omega
                    if X <= field.upto:
                        cand = e <= X
                        j = np.where(cand)[0][np.argmin(F[cand])]
                        w = int(mi[j])
                        st = max(f[j], v[j])
                        # displaced = the first eligible was not the winner
                        j1 = int(np.argmin(e))
                        c = min(cm[w], X)
                        return dict(t=X, w=w, stamp=st, F=F[j], c=c, cm=cm, stale=bool(int(mi[j1]) != w or e[j] > E1 + 1e-9 and False),
                                    displaced=bool(j1 != j), W=X - c, decided=X, E1=E1)
        t = t0 + 2 * (t - t0)
        if t - t0 > 2e5:
            raise RuntimeError(f"no block in 2e5 s (d {chain.d:.3g}, runs {len(field.f)}, slots {len(field.mid)})")


def simulate(miners, rules=None, law=None, blocks=2000, warm=200, seed=0, chain=None):
    """Honest network (every miner follows its own policy, nobody withholds). Returns stats."""
    rules = rules or Rules()
    law = law or Law()
    rng = np.random.default_rng(seed)
    n = len(miners)
    ch = chain or Chain(rules.target)
    starts = [0.0] * n
    wins = np.zeros(n); itv = []; Ws = []; stale = 0; disp = 0; Fw = []; ds = []; gaps = []; tps = []
    for b in range(blocks + warm):
        fld = Field(miners, starts, law, rng)
        r = resolve(fld, miners, rules, ch)
        prev = ch.stamp
        ch.add(r["stamp"], r.get("v") if rules.clock == "valid" else None)
        if b >= warm:
            wins[r["w"]] += 1; itv.append(r["stamp"] - prev); Ws.append(r["W"]); stale += r["stale"]
            disp += r.get("displaced", False); Fw.append(r["F"]); ds.append(ch.d); gaps.append(r.get("gap", 0.0))
        tpub = r["t"]
        if b >= warm:
            tps.append(tpub)
        starts = [tpub + DELTA] * n
        starts[r["w"]] = min(r["c"], tpub) if (miners[r["w"]].H >= 0 or miners[r["w"]].D > 0) else tpub
        if rules.rule == "pof":
            # everyone saw every run by X: they start at X (no propagation lag), the winner at its commit
            starts = [tpub] * n
            starts[r["w"]] = min(r["c"], tpub)
    itv = np.array(itv)
    comp = np.array([m.compute for m in miners]); comp = comp / comp.sum()
    share = wins / wins.sum()
    return dict(share=share.tolist(), compute=comp.tolist(), ratio=(share / comp).tolist(),
                mean=float(itv.mean()), cv=float(itv.std() / itv.mean()), p95=float(np.percentile(itv, 95)),
                W=float(np.mean(Ws)), stale=stale / blocks, displaced=disp / blocks, F=float(np.median(Fw)),
                d=float(np.median(ds)), log2d=float(np.median(np.log2(ds))), gap=float(np.mean(gaps)),
                wall=float(np.mean(np.diff(tps))))


# ------------------------------------------------------------------ attacks
def _race(miners, starts, rules, chain, law, rng, slot_scale=None):
    return resolve(Field(miners, starts, law, rng, slot_scale), miners, rules, chain)


def selfish(alpha, honest_k=8, rules=None, law=None, gamma=0.0, blocks=3000, seed=0, scale=1.0,
            H_honest=120.0, H_selfish=120.0, k=4, cores_per_unit=4):
    """Eyal-Sirer SM1 with progress-aware searches. Network = `scale` x 100 native 4-core machines
    worth of compute; the selfish miner has alpha of it, `honest_k` honest miners share the rest.
    hold: lead-1 race when the selfish block is valid by the honest publication -> gamma.
    pof : the selfish miner sees every honest run (published on find) and reveals within the window:
          it wins the lead-1 race iff its run is strictly shorter than the honest winner's."""
    rules = rules or Rules(); law = law or Law(); rng = np.random.default_rng(seed)
    units = 100 * scale
    S = Miner("selfish", cores=max(1, int(round(alpha * units)) * cores_per_unit), k=k, H=H_selfish)
    hc = (1 - alpha) * units / honest_k
    Hm = [Miner(f"h{i}", cores=max(1, int(round(hc * cores_per_unit))), k=k, H=H_honest) for i in range(honest_k)]
    pub = Chain(rules.target)
    # warm the retarget with honest-only blocks
    st = [0.0] * honest_k
    for _ in range(150):
        r = _race(Hm, st, rules, pub, law, rng); pub.add(r["stamp"]); t = r["t"]
        st = [t + DELTA] * honest_k; st[r["w"]] = min(r["c"], t)
    t0 = t
    sel_rev = 0; hon_rev = 0
    priv = None; lead = 0; nfork = 0   # honest blocks since the fork point
    h_st = st; s_st = t + DELTA; s_pend = None
    while sel_rev + hon_rev < blocks:
        base = priv if lead > 0 else pub
        # the selfish search already under way on its own tip (drawn earlier, known not to have
        # found before the last honest block) must be kept, not redrawn
        sres = s_pend if s_pend is not None else _race([S], [s_st], rules, base, law, rng)
        s_pend = None
        hres = _race(Hm, h_st, rules, pub, law, rng)
        if sres["c"] < hres["t"] and (rules.rule == "hold" or sres["c"] < hres["t"]):
            # selfish acquires a private block before the honest height is decided
            if lead == 0:
                priv = pub.copy(); nfork = 0
            priv.add(sres["stamp"]); lead += 1
            s_st = sres["c"]
            # honest race continues (same draws would be ideal; we redraw honest from its old starts
            # only when its tip changes, so keep hres for the next round by re-using h_st)
            # resolve next: compare the selfish's next block with this honest result
            while True:
                s2 = _race([S], [s_st], rules, priv, law, rng)
                if s2["c"] < hres["t"]:
                    priv.add(s2["stamp"]); lead += 1; s_st = s2["c"]
                    continue
                s_pend = s2  # the selfish's next block on its tip comes after the honest block
                break
            # honest block hres is published at hres["t"]
            th = hres["t"]
            nfork += 1
            if lead - nfork >= 2 or (lead - nfork >= 1 and nfork >= 2):
                pass
            if lead == 1 and nfork == 1:
                # lead-1 race
                if rules.rule == "hold":
                    p_s = priv.stamp
                    if p_s <= th + DELTA:
                        # tie: gamma of honest mine on the selfish block
                        on_s = rng.random(honest_k) < gamma
                        hon_base = pub.copy(); hon_base.add(hres["stamp"])
                        stH = [th + DELTA] * honest_k; stH[hres["w"]] = min(hres["c"], th)
                        # selfish next on its block, honest split
                        s2 = s_pend; s_pend = None
                        idx_s = [i for i in range(honest_k) if on_s[i]]; idx_h = [i for i in range(honest_k) if not on_s[i]]
                        r_s = _race([Hm[i] for i in idx_s], [th + DELTA] * len(idx_s), rules, priv, law, rng) if idx_s else None
                        r_h = _race([Hm[i] for i in idx_h], [stH[i] for i in idx_h], rules, hon_base, law, rng) if idx_h else None
                        ts_ = s2["t"]; tsh = r_s["t"] if r_s else math.inf; thh = r_h["t"] if r_h else math.inf
                        m = min(ts_, tsh, thh)
                        if m == ts_:
                            sel_rev += 2; priv.add(s2["stamp"]); pub = priv; s_st = s2["c"]; base_t = ts_
                            h_st = [ts_ + DELTA] * honest_k
                        elif m == tsh:
                            sel_rev += 1; hon_rev += 1; priv.add(r_s["stamp"]); pub = priv
                            w = idx_s[r_s["w"]]; h_st = [tsh + DELTA] * honest_k; h_st[w] = min(r_s["c"], tsh); s_st = tsh + DELTA
                        else:
                            hon_rev += 2; hon_base.add(r_h["stamp"]); pub = hon_base
                            w = idx_h[r_h["w"]]; h_st = [thh + DELTA] * honest_k; h_st[w] = min(r_h["c"], thh); s_st = thh + DELTA
                        lead = 0; priv = None
                        continue
                    # the selfish block is not valid in time: honest wins the height
                    hon_rev += 1; pub.add(hres["stamp"]); lead = 0; priv = None; s_pend = None
                    h_st = [th + DELTA] * honest_k; h_st[hres["w"]] = min(hres["c"], th); s_st = th + DELTA
                    continue
                else:  # pof: reveal within the window iff shorter
                    sF = sres["F"]
                    if sF < hres["F"]:
                        sel_rev += 1; pub = priv; lead = 0; priv = None
                        h_st = [th] * honest_k  # honest switch at the window's close
                        continue
                    hon_rev += 1; pub.add(hres["stamp"]); lead = 0; priv = None; s_pend = None
                    h_st = [th] * honest_k; h_st[hres["w"]] = min(hres["c"], th); s_st = th + DELTA
                    continue
            # lead >= 2 at the honest block: publish all, override
            pub_h = pub.copy(); pub_h.add(hres["stamp"])
            # SM1 with a larger lead: release just enough; we account the end result:
            # the selfish chain is longer than the honest one for as long as its lead > nfork
            # (simplification: override now; the selfish wins its whole private chain)
            sel_rev += lead; pub = priv; lead = 0; priv = None
            h_st = [th + DELTA] * honest_k
            continue
        else:
            # honest block first at lead 0: adopt
            th = hres["t"]
            hon_rev += 1; pub.add(hres["stamp"])
            h_st = [th + DELTA] * honest_k; h_st[hres["w"]] = min(hres["c"], th)
            s_st = th + DELTA
    return dict(alpha=alpha, share=sel_rev / (sel_rev + hon_rev), mean_itv=(pub.stamp - t0) / max(1, sel_rev + hon_rev))


def private_race(alpha, rules=None, law=None, trials=600, K=120, seed=0, scale=1.0, honest_k=8, k=4,
                 cores_per_unit=4, H_honest=120.0):
    """Private chain from a fork at the tip: the attacker (alpha of the compute, one miner) builds on its
    own fork; the honest miners on theirs (both with their own retarget). Returns P(success) vs z,
    success = the attacker reaches some height j > z before the honest chain does."""
    rules = rules or Rules(); law = law or Law(); rng = np.random.default_rng(seed)
    units = 100 * scale
    A = Miner("att", cores=max(1, int(round(alpha * units)) * cores_per_unit), k=k, H=H_honest)
    hc = (1 - alpha) * units / honest_k
    Hm = [Miner(f"h{i}", cores=max(1, int(round(hc * cores_per_unit))), k=k, H=H_honest) for i in range(honest_k)]
    base = Chain(rules.target); st = [0.0] * honest_k
    for _ in range(150):
        r = _race(Hm, st, rules, base, law, rng); base.add(r["stamp"]); t = r["t"]
        st = [t + DELTA] * honest_k; st[r["w"]] = min(r["c"], t)
    t0 = t
    wins = np.zeros(K + 1)
    lead_max = []
    for tr in range(trials):
        hc_ = base.copy(); ac = base.copy()
        hst = [t0 + DELTA] * honest_k; ast = t0 + DELTA
        Ht = []; At = []
        tt = t0
        for j in range(K):
            r = _race(Hm, hst, rules, hc_, law, rng); hc_.add(r["stamp"]); Ht.append(r["t"])
            hst = [r["t"] + DELTA] * honest_k; hst[r["w"]] = min(r["c"], r["t"])
        while len(At) < K:
            r = _race([A], [ast], rules, ac, law, rng); ac.add(r["stamp"]); At.append(r["t"]); ast = min(r["c"], r["t"])
            if r["t"] > Ht[-1] + 3000:
                break
        At = np.array(At + [math.inf] * (K - len(At))); Ht = np.array(Ht)
        ok = At < Ht  # attacker reaches height j+1 first
        # success for z: some height j+1 > z with ok
        idx = np.where(ok)[0]
        last = idx.max() + 1 if len(idx) else 0  # highest height (1-based) the attacker reached first
        wins[:last] += 1  # success for every z < last
    P = wins / trials
    return dict(alpha=alpha, P=P.tolist())


def z_of(P, eps=1e-3):
    """Smallest z with P(z) < eps; log-linear extrapolation of the tail when the sample runs out."""
    P = np.asarray(P)
    for z, p in enumerate(P):
        if p < eps:
            if p > 0 or z == 0:
                return z
            # resolution limit: fit the tail where P in (1/trials .. 0.3)
            break
    good = np.where((P > 0) & (P < 0.3))[0]
    if len(good) < 3:
        return int(np.argmax(P < eps)) if np.any(P < eps) else None
    a, b = np.polyfit(good, np.log(P[good]), 1)
    if a >= 0:
        return None
    return int(math.ceil((math.log(eps) - b) / a))
