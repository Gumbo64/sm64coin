#!/usr/bin/env python3
"""Head-start / selfish-mining simulator for SM64Coin's time-limit auction.

Event-driven, multi-miner, with a real block tree (forks, first-seen ties,
longest-chain-by-blocks fork choice as in rules.rs `weigh_by_blocks`).

Pluggable hooks (see the classes below):
  StartRule   when may a miner begin searching block N+1 (AtFind, AtPublish,
              AtPublishDelay(D)); `private_ok` says if a withheld block can be
              built on at all.
  ForkChoice  ByBlocks (consensus today) or ByCumDifficulty.
  Reward      PerBlock, PerDifficulty, or your own.
Model assumptions are in RESULTS.md. Rules constants mirror pow/sm64pow/src/rules.rs.
"""
from __future__ import annotations

import heapq
import math
from dataclasses import dataclass, field

import numpy as np

LN098 = -math.log(0.98)
BASE_FRAMES = 36000.0   # 20 min, rules.rs base_frames
MIN_FRAMES = 600.0
START_HALVINGS = 20
HALVING = 60.0          # seconds per difficulty halving (8 levels)
SEC_PER_LEVEL = HALVING / 8.0
TARGET = 300.0
WINDOW, CUT = 60, 6


# ------------------------------------------------------------------ hooks
class StartRule:
    name = "?"
    private_ok = True  # can a miner search on top of a block it has not published?

    def search_start(self, known: float, pub: float) -> float:
        raise NotImplementedError


class AtFind(StartRule):
    """Today: the author knows its block (hash, end state) the moment its run is found."""
    name = "at-find"

    def search_start(self, known, pub):
        return known


class AtPublish(StartRule):
    """Next search may start only when the block is public (e.g. the seed commits to a publication beacon)."""
    name = "at-publish"
    private_ok = False

    def search_start(self, known, pub):
        return max(known, pub)


class AtPublishDelay(StartRule):
    """Next search starts D seconds after publication, for everybody (author included)."""
    private_ok = False

    def __init__(self, D):
        self.D = D
        self.name = f"publish+{D:g}s"

    def search_start(self, known, pub):
        return max(known, pub + self.D)


class ForkChoice:
    name = "?"

    def key(self, sim, b):
        raise NotImplementedError


class ByBlocks(ForkChoice):
    name = "blocks"

    def key(self, sim, b):
        return sim.hgt[b]


class ByCumDifficulty(ForkChoice):
    name = "cumdiff"

    def key(self, sim, b):
        return sim.cum[b]


class Reward:
    name = "?"

    def reward(self, sim, b) -> float:
        raise NotImplementedError


class PerBlock(Reward):
    name = "per-block"

    def reward(self, sim, b):
        return 1.0


class PerDifficulty(Reward):
    name = "per-difficulty"

    def reward(self, sim, b):
        return sim.cum[b] - sim.cum[sim.par[b]]


class UncleReward(Reward):
    """Winner gets 1 - u per uncle (max 2); each uncle (a published stale sibling valid within
    omega_u of the winner's validity) gets u. Needs Sim(publish_losers=True)."""

    def __init__(self, u=0.5, omega_u=120.0):
        self.u = u; self.omega_u = omega_u; self.name = f"uncle u={u:g}"

    def reward(self, sim, b):
        return 1.0

    def all(self, sim, main, ch):
        ms = set(main)
        sib = {}
        for b in range(1, len(sim.par)):
            if sim.pub[b] < math.inf and b not in ms:
                sib.setdefault(sim.par[b], []).append(b)
        rew = np.zeros(sim.n)
        for b in ch:
            us = sorted([x for x in sib.get(sim.par[b], []) if sim.vr[x] <= sim.vr[b] + self.omega_u], key=lambda x: sim.vr[x])[:2]
            rew[sim.miner[b]] += 1.0 - self.u * len(us)
            for x in us:
                rew[sim.miner[x]] += self.u
        return rew


# ------------------------------------------------------------------ run model
@dataclass
class Model:
    """Run discovery. One compute unit = one web-miner machine: a finished search
    ("dot") every `tau` seconds on average (Poisson). A dot's frame length is
    fmin + lognormal(median `med`, sigma) * (1+u/tau0)^-kappa, u = seconds searched on this tip.
    kappa>0: deeper search on the same tip gives shorter runs (the envelope falls faster than min-of-iid)."""
    tau: float = 60.0
    fmin: float = 1800.0
    med: float = 6000.0
    sigma: float = 0.8
    kappa: float = 0.0
    tau0: float = 300.0
    emp: object = None  # array of measured single-search run lengths: bootstrap them instead of the lognormal

    def draw(self, rng, n, u):
        """n run lengths (frames) of searches finishing u seconds (array) into a miner's work on this tip."""
        if self.emp is not None:
            ex = rng.choice(self.emp, n) - self.fmin
        else:
            ex = np.exp(math.log(self.med) + self.sigma * rng.standard_normal(n))
        if self.kappa:
            ex = ex * (1.0 + u / self.tau0) ** (-self.kappa)
        return self.fmin + ex


def level_of_frames(F):
    return np.log(BASE_FRAMES / F) / LN098


class Sim:
    def __init__(self, shares, C, model=None, rule=None, fork=None, reward=None, H=120.0, delta=2.0,
                 seed=0, selfish=None, gamma=0.0, retarget=True, fixed_ld=None,
                 start_halv=START_HALVINGS, dh=HALVING, target=TARGET, valid="auction", omega=0.0, tiebreak="first",
                 announce_early=False, backdate=False, publish_losers=False,
                 delay_jitter=0.0, fresh=None, bd_mode="full", bd_margin=10.0):
        self.rng = np.random.default_rng(seed)
        sh = np.asarray(shares, float)
        self.share = sh / sh.sum()
        self.n = len(sh)
        self.model = model or Model()
        self.rule = rule or AtFind()
        self.fork = fork or ByBlocks()
        self.rew = reward or PerBlock()
        self.lam = C * self.share / self.model.tau
        self.H = np.broadcast_to(np.asarray(H, float), (self.n,)).copy()
        self.delta = delta
        self.selfish = selfish
        self.gamma = gamma
        self.retarget = retarget
        self.fixed_ld = fixed_ld
        self.sh = start_halv; self.spl = dh / 8.0; self.target = target
        self.valid = valid; self.omega = omega; self.tiebreak = tiebreak
        self.announce_early = announce_early; self.backdate = backdate; self.publish_losers = publish_losers
        self.seen = {}
        self.delay_jitter = delay_jitter; self.fresh = fresh; self.bd_mode = bd_mode; self.bd_margin = bd_margin
        if backdate is True:
            backdate = list(range(self.n))
        self.bd = set(backdate or [])
        self.drop_honest = 0; self.recv_honest = 0
        self.dmax = 2.0 ** 17 if valid == "flat" else 2.0 ** 60
        # block arrays; genesis = 0
        self.par = [-1]; self.hgt = [0]; self.ts = [0.0]; self.miner = [-1]
        self.dn = [1.0]; self.cum = [1.0]; self.pub = [0.0]; self.fdot = [0.0]
        self.Fr = [0.0]; self.cmt = [0.0]; self.dead = [False]; self.vr = [0.0]
        self.tip = [0] * self.n
        self.token = [0] * self.n
        self.pubtip = 0
        self.tie = False
        self.q = []; self.seq = 0; self.now = 0.0
        for m in range(self.n):
            self.start_task(m, 0, 0.0)

    # ---- events
    def push(self, t, kind, *a):
        self.seq += 1
        heapq.heappush(self.q, (t, self.seq, kind, a))

    # ---- retarget (Monero next_difficulty, window 60 cut 6)
    def next_diff(self, b):
        if self.fixed_ld is not None:
            return 2.0 ** self.fixed_ld
        ts, cm = [], []
        x = b
        while x >= 0 and len(ts) < WINDOW:
            ts.append(self.ts[x]); cm.append(self.cum[x]); x = self.par[x]
        ts.reverse(); cm.reverse()
        n = len(ts)
        if n <= 1:
            return 1.0
        if n <= WINDOW - 2 * CUT:
            lo, hi = 0, n
        else:
            lo = (n - (WINDOW - 2 * CUT) + 1) // 2
            hi = lo + WINDOW - 2 * CUT
        sts = sorted(ts)
        span = sts[hi - 1] - sts[lo]
        if span <= 0:
            span = 1.0
        work = cm[hi - 1] - cm[lo]
        return min(max(1.0, work * self.target / span), self.dmax)

    def new_block(self, par, miner, v, c, fdot, F, stamp=None):
        d = self.dn[par]
        b = len(self.par)
        self.par.append(par); self.hgt.append(self.hgt[par] + 1); self.ts.append(v if stamp is None else stamp); self.vr.append(v); self.miner.append(miner)
        self.cum.append(self.cum[par] + d); self.pub.append(math.inf); self.fdot.append(fdot)
        self.Fr.append(F); self.cmt.append(c); self.dead.append(False); self.dn.append(1.0)
        self.dn[b] = self.next_diff(b) if self.retarget else d
        return b

    # ---- the search plan of one miner on one tip
    def plan(self, m, P, st):
        md = self.model
        lam = self.lam[m]
        Tp = self.ts[P]
        l8 = 8.0 * math.log2(self.dn[P])
        base = l8 + 8.0 * self.sh
        Hm = self.H[m]
        rng = self.rng
        fs = np.empty(0); vs = np.empty(0); Fs = np.empty(0); tvs = np.empty(0)
        t = st
        chunk = 16
        while True:
            f = t + np.cumsum(rng.exponential(1.0 / lam, chunk))
            F = md.draw(rng, chunk, f - st)
            lev = level_of_frames(np.minimum(F, BASE_FRAMES))
            if self.valid == "flat":  # valid on find iff under the flat cap of the difficulty
                tv = np.where(lev >= l8, 0.0, math.inf)
            else:
                tv = np.maximum(0.0, (base - lev) * self.spl)
            tv[F > BASE_FRAMES] = math.inf
            v = np.maximum(f, Tp + tv)
            fs = np.concatenate([fs, f]); vs = np.concatenate([vs, v]); Fs = np.concatenate([Fs, F]); tvs = np.concatenate([tvs, tv])
            t = fs[-1]
            cm = np.minimum.accumulate(vs)
            c = np.maximum(fs, cm - Hm)
            ok = np.nonzero(c[:-1] < fs[1:])[0]
            if len(ok):
                i = ok[0]
                j = int(np.argmin(vs[: i + 1]))
                stamp = None
                if m in self.bd and self.valid == "auction":
                    stamp = max(Tp + 1.0, Tp + float(tvs[j]))
                    if self.bd_mode == "aware" and self.fresh is not None:
                        stamp = max(stamp, float(cm[i]) - (self.fresh - self.bd_margin))
                return float(c[i]), float(cm[i]), float(Fs[j]), float(fs[j]), stamp
            chunk = min(chunk * 2, 4096)

    def start_task(self, m, tip, t_start):
        self.tip[m] = tip
        self.token[m] += 1
        t_start = max(t_start, self.now)
        c, v, F, fd, stamp = self.plan(m, tip, t_start)
        self.seen[(m, tip)] = self.now
        self.push(c, "commit", m, self.token[m], tip, v, F, fd, stamp)

    def own_start(self, b, known, pub):
        return self.rule.search_start(known, pub)

    def abandon_private(self, m):
        x = self.tip[m]
        chain = []
        while x > 0 and self.miner[x] == m and self.pub[x] == math.inf and not self.dead[x]:
            chain.append(x)
            x = self.par[x]
        for y in chain:
            self.dead[y] = True
        if self.publish_losers and chain and self.pub[self.par[chain[-1]]] < math.inf:
            y = chain[-1]  # the lowest block: a stale sibling, published as a would-be uncle
            self.dead[y] = False
            self.push(max(self.now, self.vr[y]), "publish", y)

    def unpublished(self, m):
        out = []
        x = self.tip[m]
        while x > 0 and self.pub[x] == math.inf and not self.dead[x]:
            out.append(x); x = self.par[x]
        return out[::-1]

    def release(self, blocks, now):
        for b in blocks:
            self.push(max(now, self.vr[b]), "publish", b)

    # ---- handlers
    def on_commit(self, m, token, tip, v, F, fd, stamp):
        if token != self.token[m]:
            return
        c = self.now
        v = max(v, self.vr[tip])
        b = self.new_block(tip, m, v, c, fd, F, stamp)
        self.tip[m] = b
        S = self.selfish
        if m == S and self.rule.private_ok:
            if self.tie:
                self.tie = False
                self.release(self.unpublished(m), c)
            # else: withhold
        else:
            self.push(v, "publish", b)
        if self.announce_early:
            for o in range(self.n):
                if o != m:
                    self.push(c + self.lag(), "recv", o, b, False)
        self.start_task(m, b, self.rule.search_start(c, v))

    def on_publish(self, b):
        if self.dead[b] or self.pub[b] < math.inf:
            return
        self.pub[b] = self.now
        if self.fork.key(self, b) > self.fork.key(self, self.pubtip):
            self.pubtip = b
        a = self.miner[b]
        for m in range(self.n):
            if m != a:
                self.push(self.now + self.lag(), "recv", m, b, False)

    def lag(self):
        return self.delta + (self.rng.exponential(self.delay_jitter) if self.delay_jitter else 0.0)

    def switch(self, m, b):
        self.abandon_private(m)
        self.start_task(m, b, self.rule.search_start(self.now, self.pub[b] if self.pub[b] < math.inf else self.vr[b]))

    def on_recv(self, m, b, force):
        if self.dead[b]:
            return
        if self.fresh is not None and not force:
            honest = self.miner[b] not in self.bd
            if honest:
                self.recv_honest += 1
            if self.ts[b] < self.now - self.fresh:  # a stale tip: not adopted, not relayed
                if honest:
                    self.drop_honest += 1
                return
        kb = self.fork.key(self, b)
        kt = self.fork.key(self, self.tip[m])
        if m == self.selfish and self.rule.private_ok:
            self.selfish_recv(m, b)
            return
        own_unpub = self.pub[self.tip[m]] == math.inf and self.miner[self.tip[m]] == m
        tip = self.tip[m]
        pref = kb > kt
        if not pref and kb == kt and b != tip:
            if own_unpub or force:
                pref = True
            elif self.announce_early and self.ts[b] < self.ts[tip] and self.hgt[b] == self.hgt[tip]:
                pref = True
            elif self.tiebreak == "shortest" and self.hgt[b] == self.hgt[tip] and self.Fr[b] < self.Fr[tip] \
                    and self.now - self.seen.get((m, tip), -1e18) <= self.omega:
                pref = True
        if pref:
            self.switch(m, b)

    def selfish_recv(self, m, b):
        """Eyal-Sirer on a public block `b` reaching the selfish miner."""
        S = m
        self.tie = False
        t = self.tip[S]
        if self.hgt[b] < self.hgt[self.pubtip]:
            return  # stale news
        unp = self.unpublished(S)
        lead = self.hgt[t] - self.hgt[b]
        if not unp or lead < 0:
            if self.fork.key(self, b) > self.fork.key(self, t):
                self.switch(S, b)
            return
        if lead == 0:
            self.release(unp, self.now)
            self.tie = True
            for h in range(self.n):
                if h != S and self.rng.random() < self.gamma:
                    self.push(self.now, "recv", h, unp[-1], True)
        elif lead == 1:
            self.release(unp, self.now)
        else:
            self.release(unp[:1], self.now)

    def run(self, nblocks):
        while self.q and self.hgt[self.pubtip] < nblocks:
            t, _, kind, a = heapq.heappop(self.q)
            self.now = t
            if kind == "commit":
                self.on_commit(*a)
            elif kind == "publish":
                self.on_publish(*a)
            else:
                self.on_recv(*a)
        return self

    # ---- results
    def main_chain(self):
        ch = []
        x = self.pubtip
        while x > 0:
            ch.append(x); x = self.par[x]
        return ch[::-1]

    def stats(self, warm=100, tail=10):
        ch = self.main_chain()
        ch = ch[warm: len(ch) - tail]
        w = np.array([self.miner[b] for b in ch])
        cnt = np.bincount(w, minlength=self.n).astype(float)
        if hasattr(self.rew, "all"):
            rew = self.rew.all(self, self.main_chain(), ch)
        else:
            rew = np.zeros(self.n)
            for b in ch:
                rew[self.miner[b]] += self.rew.reward(self, b)
        cw = self.share
        pw = np.zeros(self.n); pl = np.zeros(self.n); nw = np.zeros(self.n); nl = np.zeros(self.n)
        for i in range(self.n):
            prev = w[:-1] == i; nxt = w[1:] == i
            nw[i] = prev.sum(); pw[i] = (prev & nxt).sum(); nl[i] = (~prev).sum(); pl[i] = ((~prev) & nxt).sum()
        ts = np.array([self.ts[b] for b in ch])
        W = np.array([self.vr[b] - self.fdot[b] for b in ch])
        Fw = np.array([self.Fr[b] for b in ch])
        created = len([1 for b in range(1, len(self.par)) if self.pub[b] < math.inf])
        return dict(
            share=cnt / cnt.sum(), reward_share=rew / rew.sum(),
            same_winner=float(np.mean(w[1:] == w[:-1])),
            same_winner_expected=float(np.sum((cnt / cnt.sum()) ** 2)),
            W_mean=float(W.mean()), W_med=float(np.median(W)), W_pos=float(np.mean(W > 1.0)),
            blocktime=float(np.mean(np.diff(ts))), ld=float(np.mean([math.log2(self.dn[b]) for b in ch])),
            stale=1.0 - len(self.main_chain()) / max(created, 1),
            real_blocktime=float(np.mean(np.diff([self.vr[b] for b in ch]))),
            bd_gain=float(np.mean([self.vr[b] - self.ts[b] for b in ch if self.miner[b] in self.bd] or [0.0])),
            bd_frac=float(np.mean([self.vr[b] - self.ts[b] > 1.0 for b in ch if self.miner[b] in self.bd] or [0.0])),
            false_stale=self.drop_honest / max(self.recv_honest, 1),
            n=len(ch), F_mean=float(Fw.mean()), F_med=float(np.median(Fw)),
            p_after_win=(pw / np.maximum(nw, 1)).tolist(), p_after_loss=(pl / np.maximum(nl, 1)).tolist(),
        )


def calibrate_ld(shares, C, nb=500, iters=11, seed=777, **kw):
    """Idealised retarget: the fixed log2-difficulty whose mean block time is the target (bisection)."""
    tgt = kw.get("target", TARGET)
    lo, hi = 0.0, (17.0 if kw.get("valid") == "flat" else 60.0)
    for _ in range(iters):
        mid = (lo + hi) / 2
        s = Sim(shares, C, seed=seed, fixed_ld=mid, **kw).run(nb)
        ch = s.main_chain()
        bt = (s.ts[ch[-1]] - s.ts[ch[10]]) / (len(ch) - 11)
        if bt > tgt:
            hi = mid
        else:
            lo = mid
    return (lo + hi) / 2
