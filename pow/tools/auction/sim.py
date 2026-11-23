#!/usr/bin/env python3
"""Chain-level model of SM64Coin's frame-length auction.

Every block is a race: the limit (the longest accepted run, frames) grows with
wall time since the parent, each miner's best run so far (its envelope) falls
as it searches, and the block goes to whoever's run fits the limit first, as
the network sees it. This models, per block:

- the limit curve of a rule set (today's grace + decay, or no grace with a
  chosen slope), at the Monero retarget difficulty of the chain so far;
- miners' envelopes (parametric profiles, or measured ones from envbench);
- clock skew, the future-time limit (stamping ahead) and the hold policy
  (nodes accept a run only once it fits their own clock);
- propagation latency, so near-simultaneous fits are a coin flip;
- power-on restarts with a cost in frames that scales with progress lost;
- an optional withholding miner (selfish mining with a head start).

Outputs: block times, winners, winning run lengths, restart counts, and how
much a withholder gains. Run `sim.py --help`; `sweep.py` sweeps the slope.
"""
from __future__ import annotations

import argparse
import csv
import dataclasses
import json
import math
from dataclasses import dataclass, field

import numpy as np

FPS = 30
DT = 0.5  # seconds per simulation step


# ---------------------------------------------------------------- rules


@dataclass
class Rules:
    """A rule set's limit curve and weight. Mirrors pow/sm64pow/src/rules.rs."""

    base_frames: int = 18000
    min_frames: int = 600
    shrink: float = 0.98  # per difficulty level (1/8 doubling)
    grace: float = 600.0  # seconds the limit stays flat
    halving: float = 300.0  # seconds per difficulty halving of decay (8 levels)
    target: float = 300.0
    window: int = 60
    cut: int = 6
    weight_decays: bool = True  # today's weight = effective difficulty
    stall: float = 3600.0  # power-on gate (None-equivalent when restart_cost is used)
    restart_per_point: float | None = None  # seconds per progress point lost (new rule); None: the stall gate
    restart_base: float = 0.0  # seconds every restart waits
    restart_max: float = 7200.0
    ftl: float = 120.0  # future-time limit

    def level8(self, d: float) -> float:
        return 8.0 * math.log2(max(d, 1.0))

    def limit(self, d: float, t: np.ndarray) -> np.ndarray:
        lvl = self.level8(d) - 8.0 * np.maximum(0.0, t - self.grace) / self.halving
        lim = self.base_frames * np.power(self.shrink, np.maximum(lvl, 0.0))
        return np.clip(lim, self.min_frames, self.base_frames)

    def weight(self, d: float, elapsed: float) -> float:
        if not self.weight_decays or elapsed <= self.grace:
            return d
        return max(1.0, d / 2 ** int((elapsed - self.grace) // self.halving))

    def restart_cost(self, points: int) -> float | None:
        if self.restart_per_point is None:
            return None
        return min(self.restart_max, self.restart_base + self.restart_per_point * points)


def current_rules(**kw) -> Rules:
    return Rules(**kw)


def auction_rules(halving: float, ftl: float = 15.0, **kw) -> Rules:
    """The proposal: no grace, constant weight, restart cost instead of the gate."""
    base = dict(grace=0.0, halving=halving, weight_decays=False, restart_per_point=60.0, restart_base=3600.0, ftl=ftl, stall=float("inf"))
    base.update(kw)
    return Rules(**base)


def monero_retarget(ts: list[float], cum: list[float], r: Rules) -> float:
    n = min(len(ts), r.window)
    if n <= 1:
        return 1.0
    t = sorted(ts[-n:])
    c = cum[-n:]
    kept = r.window - 2 * r.cut
    if n <= kept:
        b, e = 0, n
    else:
        b = (n - kept + 1) // 2
        e = b + kept
    span = max(1.0, t[e - 1] - t[b])
    work = c[e - 1] - c[b]
    return max(1.0, work * r.target / span)


# ---------------------------------------------------------------- miners


@dataclass
class Profile:
    """A miner's envelope for a 1x star: no run before t0, first run L0,
    improving toward Lmin with time constant tau. Per block, t0/tau get a
    lognormal jitter and everything scales with the star's length factor."""

    name: str
    t0: float
    L0: float
    Lmin: float
    tau: float
    jitter: float = 0.35
    realtime: bool = False  # a human: a run of L frames takes L/30 s to play
    skew: float = 0.0  # this miner's clock offset, seconds (positive: fast)
    withhold: float = 0.0  # max seconds it holds a found block back (0: honest)
    stamp_ahead: bool = False  # stamps its blocks at the future-time limit

    def envelope(self, star: float, t: np.ndarray, rng: np.random.Generator) -> np.ndarray:
        j = lambda: math.exp(rng.normal(0.0, self.jitter))
        t0 = self.t0 * star * j()
        tau = self.tau * star * j()
        L0, Lmin = self.L0 * star, self.Lmin * star
        env = Lmin + (L0 - Lmin) * np.exp(-np.maximum(0.0, t - t0) / tau)
        if self.realtime:
            env = np.maximum(env, np.minimum(L0, FPS * t))
        env[t < t0] = np.inf
        return env


@dataclass
class Measured:
    """A measured envelope family (envbench CSV): per seed, the smallest cap
    under which the miner succeeded by time t. A block draws one seed at
    random and scales frames and seconds by the star factor."""

    name: str
    curves: list[tuple[np.ndarray, np.ndarray]]  # (seconds, frames) step points per seed
    skew: float = 0.0
    withhold: float = 0.0
    stamp_ahead: bool = False
    realtime: bool = False
    speed: float = 1.0  # >1: faster hardware (divides time)

    def envelope(self, star: float, t: np.ndarray, rng: np.random.Generator) -> np.ndarray:
        secs, frames = self.curves[rng.integers(len(self.curves))]
        env = np.full(t.shape, np.inf)
        for s, f in zip(secs * star / self.speed, frames * star):
            env = np.where(t >= s, np.minimum(env, f), env)
        return env


def load_measured(path: str, case: str, name: str, **kw) -> Measured:
    per_seed: dict[str, list[tuple[float, float]]] = {}
    with open(path) as fh:
        for row in csv.DictReader(fh):
            if row["case"] != case or not row["seconds_to_success"]:
                continue
            per_seed.setdefault(row["seed"], []).append((float(row["seconds_to_success"]), float(row["cap_frames"])))
    curves = []
    for pts in per_seed.values():
        pts.sort()
        best, out = math.inf, []
        for s, f in pts:
            if f < best:
                best = f
                out.append((s, f))
        curves.append((np.array([p[0] for p in out]), np.array([p[1] for p in out])))
    if not curves:
        raise SystemExit(f"no successful rows for case {case} in {path}")
    return Measured(name=name, curves=curves, **kw)


# Today's algorithms: a star takes tens of minutes of search, and the limit
# barely matters (the retarget sits at difficulty 1).
SLOW_MINERS = [
    Profile("quick bot", t0=600, L0=5000, Lmin=3800, tau=900, jitter=0.6),
    Profile("optimizer bot", t0=1500, L0=4200, Lmin=2600, tau=2400, jitter=0.6),
]

DEFAULT_MINERS = [
    Profile("quick bot", t0=15, L0=3600, Lmin=2600, tau=90),
    Profile("optimizer bot", t0=90, L0=3000, Lmin=1300, tau=400),
    Profile("human", t0=150, L0=4200, Lmin=3200, tau=500, realtime=True, jitter=0.5),
]

# A power-on restart: the easiest first star from a fresh game (frames, 1x).
RESTART = Profile("restart", t0=20, L0=2200, Lmin=1500, tau=120)


# ---------------------------------------------------------------- the chain


@dataclass
class World:
    blocks: int = 1500
    warmup: int = 200
    tmax: float = 7200.0
    star_sigma: float = 0.45  # lognormal spread of star lengths
    stuck_prob: float = 0.0  # chance a parent state can't progress at all (game softlock; rare)
    frontier: int | None = None  # miners can only do the first this many points of a cycle (intelligence softlock)
    latency: float = 1.0  # seconds for a block to reach the other miners
    node_skew: float = 0.5  # honest nodes' clock spread (s, std)
    hold: bool = False  # nodes accept a run only once it fits their own clock
    stale_limit: float | None = None  # nodes refuse stamps older than this on arrival (s)
    seed: int = 1


@dataclass
class Result:
    times: list[float] = field(default_factory=list)
    frames: list[float] = field(default_factory=list)
    winners: list[str] = field(default_factory=list)
    restarts: int = 0
    stalls: int = 0
    diffs: list[float] = field(default_factory=list)
    best_gap: list[float] = field(default_factory=list)  # winning run vs best run anyone held at that moment (frames)
    stuck_waits: list[float] = field(default_factory=list)  # block time of blocks whose parent nobody could progress from
    needless_restarts: int = 0  # restarts that beat a feasible next star
    cycle_peaks: list[int] = field(default_factory=list)  # points reached before each restart

    def summary(self, warmup: int) -> dict:
        t = np.array(self.times[warmup:])
        f = np.array(self.frames[warmup:])
        names = self.winners[warmup:]
        share = {n: names.count(n) / max(1, len(names)) for n in sorted(set(names))}
        return {
            "blocks": len(t),
            "mean_s": round(float(t.mean()), 1),
            "p50_s": round(float(np.percentile(t, 50)), 1),
            "p90_s": round(float(np.percentile(t, 90)), 1),
            "p99_s": round(float(np.percentile(t, 99)), 1),
            "cv": round(float(t.std() / t.mean()), 3),
            "mean_frames": round(float(f.mean()), 1),
            "restarts": self.restarts,
            "stalls": self.stalls,
            "share": {k: round(v, 3) for k, v in share.items()},
            "mean_lost_frames": round(float(np.mean(self.best_gap[warmup:])), 1),
            "needless_restarts": self.needless_restarts,
            "stuck_wait_s": round(float(np.mean(self.stuck_waits)), 1) if self.stuck_waits else None,
            "stuck_share_of_time": round(float(np.sum(self.stuck_waits) / max(1.0, np.sum(self.times))), 3),
            "mean_cycle_peak": round(float(np.mean(self.cycle_peaks)), 1) if self.cycle_peaks else None,
        }


def simulate(rules: Rules, miners: list, world: World) -> Result:
    rng = np.random.default_rng(world.seed)
    t = np.arange(0.0, world.tmax, DT)
    res = Result()
    ts: list[float] = [0.0]
    cum: list[float] = [1.0]
    now = 0.0
    points = 0
    winner = -1  # who mined the parent: everyone else hears of it `lag` seconds after its timestamp
    lag = 0.0
    for _ in range(world.blocks):
        d = monero_retarget(ts, cum, rules)
        star = math.exp(rng.normal(0.0, world.star_sigma))
        stuck = rng.random() < world.stuck_prob or (world.frontier is not None and points >= world.frontier)
        lim_true = rules.limit(d, t)  # limit by true time since the parent's timestamp
        cands = []  # (accepted_time, frames, miner index, published_time, found_time)
        env_all = []
        for i, m in enumerate(miners):
            env = np.full(t.shape, np.inf) if stuck else m.envelope(star, t, rng)
            if i != winner and lag > 0:
                k = int(round(lag / DT))  # it started searching `lag` seconds after the parent's timestamp
                env = np.concatenate([np.full(k, np.inf), env[: len(env) - k]]) if k < len(env) else np.full(t.shape, np.inf)
            env_all.append(env)
            # The moment the run fits the limit, by the clock that decides.
            if world.hold:
                shift = rng.normal(0.0, world.node_skew)  # a typical honest node's clock
            else:
                shift = (rules.ftl if m.stamp_ahead else 0.0) + m.skew  # stamping ahead, bounded by nodes ~ true time
            lim = rules.limit(d, t + max(0.0, shift))
            ok = np.nonzero(env <= lim)[0]
            if len(ok):
                k = ok[0]
                cands.append([t[k], env[k], i, t[k], t[k]])
        # A power-on restart, available to everyone (best restart search of the field).
        cost = rules.restart_cost(points)
        renv = RESTART.envelope(1.0, t, rng)
        if cost is not None:
            # A restart is judged against the limit `cost` seconds earlier: valid from t = cost.
            lim_r = np.where(t >= cost, rules.limit(d, t - cost), -1.0)
            ok = np.nonzero(renv <= lim_r)[0]
            restart_at = t[ok[0]] if len(ok) else math.inf
        else:
            ok = np.nonzero((t > rules.stall) & (renv <= lim_true))[0]
            restart_at = t[ok[0]] if len(ok) else math.inf
        # Withholding: a withholder publishes at found + withhold, or at once when another block appears.
        cands.sort(key=lambda c: c[0])
        normal = cands[0] if cands else None
        if normal is not None:
            rest = cands[1:]
            first_other = min([c[0] for c in rest] + [restart_at])
            m = miners[normal[2]]
            if m.withhold > 0:
                pub = min(normal[0] + m.withhold, first_other)
                normal[3] = pub
                if pub >= first_other - world.latency and rng.random() < 0.5:  # caught: a race, coin flip
                    normal = rest[0] if rest and rest[0][0] <= restart_at else None
        if normal is None or restart_at < normal[0] - world.latency * rng.random():
            if restart_at == math.inf:
                res.stalls += 1
                elapsed, frames, who, pub = world.tmax, math.nan, "stall", world.tmax
            else:
                elapsed, frames, who, pub = restart_at, float(renv[int(restart_at / DT)]), "restart", restart_at
                res.restarts += 1
                if not stuck:
                    res.needless_restarts += 1
                res.cycle_peaks.append(points)
            points = 1
            winner, lag = -1, world.latency
        else:
            found, frames, idx, pub = normal[0], normal[1], normal[2], normal[3]
            # Ties inside the latency: the earlier-fitting of two runs only wins reliably past the latency.
            close = [c for c in cands if c[2] != idx and c[0] - found < world.latency]
            if close and rng.random() < 0.5 * len(close) / (1 + len(close)):
                c = rng.choice(len(close))
                found, frames, idx, pub = close[c][0], close[c][1], close[c][2], close[c][3]
            who = miners[idx].name
            elapsed = found
            # The block's timestamp: backdated to the fit, unless nodes refuse stale stamps.
            if world.stale_limit is not None and pub - found > world.stale_limit:
                elapsed = pub - world.stale_limit
            winner, lag = idx, max(0.0, pub - elapsed) + world.latency
            points += 1
        best_now = min((e[min(len(t) - 1, int(elapsed / DT))] for e in env_all), default=math.inf)
        res.best_gap.append(0.0 if not math.isfinite(best_now) or not math.isfinite(frames) else max(0.0, frames - best_now))
        if points >= 70:
            points = 0
        now += max(pub, elapsed) if who not in ("stall",) else world.tmax
        res.times.append(elapsed if who != "stall" else world.tmax)
        if stuck:
            res.stuck_waits.append(res.times[-1])
        res.frames.append(frames)
        res.winners.append(who)
        res.diffs.append(d)
        ts.append(ts[-1] + res.times[-1])
        cum.append(cum[-1] + rules.weight(d, res.times[-1]))
    return res


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rules", choices=["current", "auction"], default="auction")
    ap.add_argument("--halving", type=float, default=300.0, help="seconds per difficulty halving of the limit's growth")
    ap.add_argument("--ftl", type=float, default=15.0)
    ap.add_argument("--hold", action="store_true")
    ap.add_argument("--withhold", type=float, default=0.0, help="the optimizer bot withholds up to this many seconds")
    ap.add_argument("--stale-limit", type=float, default=None)
    ap.add_argument("--blocks", type=int, default=1500)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--measured", help="envbench CSV; replaces the bot profiles with measured envelopes")
    ap.add_argument("--case", default="power_on")
    a = ap.parse_args()
    r = current_rules() if a.rules == "current" else auction_rules(a.halving, a.ftl)
    if a.rules == "current":
        r.ftl = a.ftl
    miners = [dataclasses.replace(m) for m in DEFAULT_MINERS]
    if a.measured:
        miners = [load_measured(a.measured, a.case, "measured bot"), load_measured(a.measured, a.case, "measured bot x4", speed=4.0), miners[2]]
    if a.withhold:
        miners[1 if not a.measured else 1].withhold = a.withhold
    res = simulate(r, miners, World(blocks=a.blocks, hold=a.hold, stale_limit=a.stale_limit, seed=a.seed))
    print(json.dumps(res.summary(World().warmup), indent=1))


if __name__ == "__main__":
    main()
